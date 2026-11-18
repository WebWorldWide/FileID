import CryptoKit
import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

/// Trash put-back restores a file's faces and People assignments, in the format the Rust
/// engine writes (shared/test-corpus/conformance/files.json, files-trash-put-back-restores-faces).
@Suite("Trash snapshot keeps faces")
struct TrashFaceSnapshotTests {
    private struct Fixture {
        let root: URL
        let db: FileIDEngine.Database
        let journal: URL
        let bin: URL
        let a: URL
        let b: URL
    }

    private func makeFixture() async throws -> Fixture {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDTrashFaces-\(UUID().uuidString)")
        let bin = root.appendingPathComponent("FakeTrash")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let a = root.appendingPathComponent("a.jpg"), b = root.appendingPathComponent("b.jpg")
        try Data("face a".utf8).write(to: a)
        try Data("face b".utf8).write(to: b)
        try await db.pool.write { sql in
            for (id, url) in [(Int64(1), a), (2, b)] {
                try sql.execute(
                    sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,file_ref,has_faces) VALUES(?,?,?,6,0,'image','jpg',?,1)",
                    arguments: [id, url.path, StablePathHash.hash(url.path), Discovery.inode(of: url).map { Int64(bitPattern: $0) }])
            }
            try sql.execute(sql: "INSERT INTO persons(id,first_name,created_at,representative_face_id,file_count) VALUES(1,'Alex',123,1,2),(2,'Sam',123,3,1)")
            try sql.execute(sql: """
                INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,face_quality)
                VALUES(1,1,1,X'00','[0,0,1,1]',X'0000803f',0.9),(2,2,1,X'00','[0,0,1,1]',X'0000803f',0.8),(3,1,2,X'00','[0,0,1,1]',X'0000803f',0.7)
                """)
        }
        return Fixture(root: root, db: db, journal: root.appendingPathComponent("trash-journal.json"), bin: bin, a: a, b: b)
    }

    private func trashFirstFile(_ f: Fixture) async throws -> String {
        let result = await FileIDEngineMain.trashFiles(database: f.db, fileIDs: [1], journal: f.journal) { url in
            let landed = f.bin.appendingPathComponent(url.lastPathComponent)
            try FileManager.default.moveItem(at: url, to: landed)
            return landed
        }
        return try #require(result.action.split(separator: ":").last.map(String.init))
    }

    private func faces(_ db: FileIDEngine.Database) async throws -> [[Int64?]] {
        try await db.pool.read { sql in
            try Row.fetchAll(sql, sql: "SELECT id,file_id,person_id FROM face_prints ORDER BY id").map { [$0[0], $0[1], $0[2]] }
        }
    }

    private func persons(_ db: FileIDEngine.Database) async throws -> [[Int64?]] {
        try await db.pool.read { sql in
            try Row.fetchAll(sql, sql: "SELECT id,file_count,representative_face_id FROM persons ORDER BY id").map { [$0[0], $0[1], $0[2]] }
        }
    }

    @Test("put back restores the faces, their people and the counts")
    func putBackRestoresFaces() async throws {
        let f = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: f.root) }
        let batchID = try await trashFirstFile(f)

        #expect(try await faces(f.db) == [[2, 2, 1]])
        #expect(try await persons(f.db) == [[1, 1, 2], [2, 0, nil]])

        let result = await TrashJournal.restore(batchID: batchID, database: f.db, journal: f.journal)
        #expect(result.succeeded == 1 && result.failed == 0)
        #expect(try await faces(f.db) == [[1, 1, 1], [2, 2, 1], [3, 1, 2]])
        #expect(try await persons(f.db) == [[1, 2, 2], [2, 1, 3]])
        let (hasFaces, blob, quality): (Int64?, Data?, Double?) = try await f.db.pool.read { sql in
            (try Int64.fetchOne(sql, sql: "SELECT has_faces FROM files WHERE id = 1"),
             try Data.fetchOne(sql, sql: "SELECT arcface_embedding FROM face_prints WHERE id = 3"),
             try Double.fetchOne(sql, sql: "SELECT face_quality FROM face_prints WHERE id = 3"))
        }
        #expect(hasFaces == 1 && blob == Data([0, 0, 0x80, 0x3f]) && quality == 0.7)
    }

    @Test("a person deleted while the file was in the Trash leaves the face unassigned")
    func missingPersonLeavesFaceUnassigned() async throws {
        let f = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: f.root) }
        let batchID = try await trashFirstFile(f)
        try await f.db.pool.write { try $0.execute(sql: "DELETE FROM persons WHERE id = 2") }

        let result = await TrashJournal.restore(batchID: batchID, database: f.db, journal: f.journal)
        #expect(result.succeeded == 1)
        #expect(try await faces(f.db) == [[1, 1, 1], [2, 2, 1], [3, 1, nil]])
        #expect(try await persons(f.db) == [[1, 2, 2]])
    }

    @Test("a journal written before faces were snapshotted still restores the file")
    func oldJournalRestoresWithoutFaces() async throws {
        let f = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: f.root) }
        let batchID = try await trashFirstFile(f)
        var batch = try #require(TrashJournal.find(batchID, at: f.journal))
        batch.items = batch.items.map { item in
            var old = item
            old.rows.removeAll { $0.table == "face_prints" }
            return old
        }
        try TrashJournal.save([batch], at: f.journal)

        let result = await TrashJournal.restore(batchID: batchID, database: f.db, journal: f.journal)
        #expect(result.succeeded == 1 && result.failed == 0)
        #expect(try await faces(f.db) == [[2, 2, 1]])
        #expect(try await persons(f.db) == [[1, 1, 2], [2, 0, nil]])
        #expect(FileManager.default.fileExists(atPath: f.a.path))
    }

    @Test("a face row that belongs to another file fails the restore of that file")
    func foreignFaceRowIsRefused() async throws {
        let f = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: f.root) }
        let batchID = try await trashFirstFile(f)
        var batch = try #require(TrashJournal.find(batchID, at: f.journal))
        batch.items[0].rows = batch.items[0].rows.map { row in
            guard row.table == "face_prints" else { return row }
            var changed = row
            changed.columns["file_id"] = TrashJournal.Value(int: 2)
            return changed
        }
        try TrashJournal.save([batch], at: f.journal)

        let result = await TrashJournal.restore(batchID: batchID, database: f.db, journal: f.journal)
        #expect(result.failed == 1)
        #expect(try await faces(f.db) == [[2, 2, 1]])
    }

    @Test("Swift's signed Rust-format journal carries the faces in the shape the Rust engine reads")
    func dualWriteCarriesFaceRows() async throws {
        let f = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: f.root) }
        _ = try await trashFirstFile(f)

        let text = try String(contentsOf: RustTrashLog.url(beside: f.journal), encoding: .utf8)
        let payload = try #require(text.split(separator: "\n").first?.split(separator: "\t").first)
        let object = try #require(try JSONSerialization.jsonObject(with: Data(payload.utf8)) as? [String: Any])
        let rows = try #require(((object["items"] as? [[String: Any]])?.first?["rows"]) as? [[String: Any]])
        #expect(rows.first?["table"] as? String == "files")
        let faceRows = rows.filter { $0["table"] as? String == "face_prints" }
        #expect(faceRows.count == 2)
        let first = try #require(faceRows.first?["columns"] as? [String: Any])
        #expect((first["file_id"] as? [String: Any])?["int"] as? Int == 1)
        #expect((first["person_id"] as? [String: Any])?["int"] as? Int == 1)
        #expect((first["arcface_embedding"] as? [String: Any])?["blob"] as? String == "AACAPw==")
        #expect((first["bbox"] as? [String: Any])?["text"] as? String == "[0,0,1,1]")
    }

    @Test("a batch the Rust engine trashed puts back with its faces")
    func rustBatchRestoresFaces() async throws {
        let f = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: f.root) }
        let trashed = f.bin.appendingPathComponent("a.jpg")
        let inode = try #require(Discovery.inode(of: f.a))
        try FileManager.default.moveItem(at: f.a, to: trashed)
        try await f.db.pool.write { try $0.execute(sql: "DELETE FROM files WHERE id = 1") }
        let json = """
        {"batch_id":"rust-batch","timestamp":1700000000.25,"items":[{"file_id":1,"original_path":"\(f.a.path)",\
        "source_identity":{"volume":1,"file":\(inode)},"trashed_path":"\(trashed.path)","rows":[\
        {"table":"files","columns":{"extension":{"text":"jpg"},"file_ref":{"int":\(Int64(bitPattern: inode))},"has_faces":{"int":1},"id":{"int":1},\
        "kind":{"text":"image"},"path_hash":{"int":\(StablePathHash.hash(f.a.path))},"path_text":{"text":"\(f.a.path)"},\
        "scanned_at":{"real":0.0},"size_bytes":{"int":6}}},\
        {"table":"face_prints","columns":{"arcface_embedding":{"blob":"AACAPw=="},"bbox":{"text":"[0,0,1,1]"},"excluded":{},"face_quality":{"real":0.9},\
        "file_id":{"int":1},"id":{"int":1},"person_id":{"int":1},"print_data":{"blob":"AA=="}}},\
        {"table":"face_prints","columns":{"arcface_embedding":{"blob":"AACAPw=="},"bbox":{"text":"[0,0,1,1]"},"face_quality":{"real":0.7},\
        "file_id":{"int":1},"id":{"int":3},"person_id":{"int":2},"print_data":{"blob":"AA=="}}}]}]}
        """
        let key = Data((0..<32).map { UInt8($0) })
        try key.write(to: f.root.appendingPathComponent("log-hmac.key"))
        let mac = HMAC<SHA256>.authenticationCode(for: Data(json.utf8), using: SymmetricKey(data: key))
        try (json + "\t" + Data(mac).map { String(format: "%02x", $0) }.joined() + "\n")
            .write(to: f.root.appendingPathComponent("trash_log.json"), atomically: true, encoding: .utf8)

        try await f.db.pool.write {
            try $0.execute(sql: "INSERT INTO library_roots(path,display,kind,added_at) VALUES(?,?, 'internal',1)", arguments: [f.root.path, f.root.path])
        }
        let result = await TrashJournal.restore(batchID: "rust-batch", database: f.db, journal: f.journal)
        #expect(result.succeeded == 1 && result.failed == 0)
        #expect(try await faces(f.db) == [[1, 1, 1], [2, 2, 1], [3, 1, 2]])
        #expect(try await persons(f.db) == [[1, 2, 1], [2, 1, 3]])
    }
}
