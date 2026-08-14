import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

private typealias EngineDatabase = FileIDEngine.Database

@Suite("Bulk filesystem mutations")
struct BulkMutationTests {
    private func makeDB(_ root: URL) throws -> EngineDatabase {
        try EngineDatabase(at: root.appendingPathComponent("bulk.sqlite"))
    }

    private func insert(_ db: EngineDatabase, id: Int64, path: String) async throws {
        try await db.pool.write { sql in
            try sql.execute(
                sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES (?,?,?,1,0,'doc','txt')",
                arguments: [id, path, StablePathHash.hash(path)])
        }
    }

    @Test("Bulk rename updates path identity in one chunk and isolates invalid entries")
    func renameChunk() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDBulkRename-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let first = root.appendingPathComponent("first.txt")
        let second = root.appendingPathComponent("second.txt")
        try Data("1".utf8).write(to: first)
        try Data("2".utf8).write(to: second)
        let db = try makeDB(root)
        try await insert(db, id: 1, path: first.path)
        try await insert(db, id: 2, path: second.path)

        let result = await FileIDEngineMain.renameFiles(
            database: db,
            renames: [
                RenameEntry(fileID: 1, newName: "renamed.md"),
                RenameEntry(fileID: 2, newName: "bad/name.txt"),
                RenameEntry(fileID: 999, newName: "missing.txt")
            ])
        #expect(result.succeeded == 1)
        #expect(result.failed == 2)
        let renamed = root.appendingPathComponent("renamed.md")
        #expect(FileManager.default.fileExists(atPath: renamed.path))
        #expect(FileManager.default.fileExists(atPath: second.path))
        let fetched: Row? = try await db.pool.read { sql in
            try Row.fetchOne(sql, sql: "SELECT path_text, path_hash, extension FROM files WHERE id = 1")
        }
        let row = try #require(fetched)
        let path: String = row["path_text"]
        let pathHash: Int64 = row["path_hash"]
        let ext: String = row["extension"]
        #expect(path == renamed.path)
        #expect(pathHash == StablePathHash.hash(renamed.path))
        #expect(ext == "md")
    }

    @Test("Bulk rename rejects a same-path replacement by file identity")
    func renameRejectsReplacement() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDBulkIdentity-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let source = root.appendingPathComponent("source.txt")
        try Data("replacement".utf8).write(to: source)
        let inode = try #require(Discovery.inode(of: source))
        let db = try makeDB(root)
        try await insert(db, id: 1, path: source.path)
        try await db.pool.write { sql in
            try sql.execute(sql: "UPDATE files SET file_ref = ? WHERE id = 1",
                            arguments: [Int64(bitPattern: inode &+ 1)])
        }

        let result = await FileIDEngineMain.renameFiles(
            database: db,
            renames: [RenameEntry(fileID: 1, newName: "renamed.txt")])
        #expect(result.succeeded == 0)
        #expect(result.failed == 1)
        #expect(FileManager.default.fileExists(atPath: source.path))
        #expect(!FileManager.default.fileExists(
            atPath: root.appendingPathComponent("renamed.txt").path))
    }

    @Test("Trash request IDs are deduplicated in stable order")
    func trashIDsAreOrderedUnique() {
        #expect(FileIDEngineMain.orderedUniqueFileIDs([3, 1, 3, 2, 1]) == [3, 1, 2])
    }

    @Test("Different-people verdict persists stable face anchors")
    func markPersonsDifferentPersistsStableAnchors() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDDifferentPeople-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let db = try makeDB(root)
        try await db.pool.write { sql in
            try sql.execute(sql: """
                INSERT INTO files
                    (id, path_text, path_hash, size_bytes, scanned_at, kind, extension)
                VALUES (1, '/a.jpg', 1, 1, 0, 'image', 'jpg'),
                       (2, '/b.jpg', 2, 1, 0, 'image', 'jpg')
                """)
            try sql.execute(sql: """
                INSERT INTO persons (id, representative_face_id, file_count, created_at)
                VALUES (10, 101, 1, 0), (20, 202, 1, 0)
                """)
            try sql.execute(
                sql: "INSERT INTO face_prints (id, file_id, person_id, print_data, bbox) VALUES (?, ?, ?, ?, ?)",
                arguments: [101, 1, 10, Data(), "0,0,0.2,0.2"])
            try sql.execute(
                sql: "INSERT INTO face_prints (id, file_id, person_id, print_data, bbox) VALUES (?, ?, ?, ?, ?)",
                arguments: [202, 2, 20, Data(), "0.5,0.5,0.2,0.2"])
        }

        let result = await FileIDEngineMain.markPersonsDifferent(
            database: db,
            sourcePersonID: 20,
            destinationPersonID: 10,
            sourceAnchorFaceID: 202,
            destinationAnchorFaceID: 101
        )
        #expect(result.succeeded == 1)
        let row = try await db.pool.read { sql in
            try Row.fetchOne(sql, sql: """
                SELECT person_a, person_b, face_a, face_b, file_a, bbox_a, file_b, bbox_b
                FROM face_verifications
                """)
        }
        let verdict = try #require(row)
        #expect((verdict["person_a"] as Int64?) == 10)
        #expect((verdict["person_b"] as Int64?) == 20)
        #expect((verdict["face_a"] as Int64?) == 101)
        #expect((verdict["face_b"] as Int64?) == 202)
        #expect((verdict["file_a"] as Int64?) == 1)
        #expect((verdict["bbox_b"] as String?) == "0.5,0.5,0.2,0.2")
    }

    @Test("Path prefetch crosses SQLite parameter chunks without omissions")
    func pathPrefetchChunks() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDBulkFetch-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let db = try makeDB(root)
        try await db.pool.write { sql in
            for id in 1...1_005 {
                let path = root.appendingPathComponent("\(id).txt").path
                try sql.execute(
                    sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES (?,?,?,1,0,'doc','txt')",
                    arguments: [id, path, StablePathHash.hash(path)])
            }
        }
        let ids = (1...1_005).map(Int64.init)
        let states = try await FileIDEngineMain.fetchBulkStates(database: db, fileIDs: ids)
        #expect(states.count == ids.count)
        #expect(states[1]?.path.hasSuffix("1.txt") == true)
        #expect(states[1_005]?.path.hasSuffix("1005.txt") == true)
    }
}
