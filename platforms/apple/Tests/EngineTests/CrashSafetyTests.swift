import CryptoKit
import AsyncAlgorithms
import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

/// Each test stops an operation at the point where a SIGKILL would land and checks what a
/// restarted engine finds on disk (shared/scripts/e2e/crash_safety.py drives the real thing).
@Suite("Crash safety of file-changing commands")
struct CrashSafetyTests {
    private func makeRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDCrash-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }

    private func insert(_ db: FileIDEngine.Database, id: Int64, path: String) async throws {
        let fileRef = Discovery.inode(of: URL(fileURLWithPath: path)).map { Int64(bitPattern: $0) }
        try await db.pool.write { sql in
            try sql.execute(
                sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, file_ref) VALUES (?,?,?,1,0,'image','jpg',?)",
                arguments: [id, path, StablePathHash.hash(path), fileRef])
            try sql.execute(sql: "INSERT INTO tags(file_id, tag, source, score) VALUES (?, 'mine', 'user', NULL)", arguments: [id])
        }
    }

    @Test("trashFiles journals every moved file before it moves the next one")
    func trashJournalsEachFileBeforeTheNext() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let bin = root.appendingPathComponent("FakeTrash")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        let first = root.appendingPathComponent("first.jpg"), second = root.appendingPathComponent("second.jpg")
        try Data("one".utf8).write(to: first)
        try Data("two".utf8).write(to: second)
        try await insert(db, id: 1, path: first.path)
        try await insert(db, id: 2, path: second.path)
        let journal = root.appendingPathComponent("trash-journal.json")
        var journalWhenSecondMoved: [TrashJournal.Batch] = []
        let result = await FileIDEngineMain.trashFiles(database: db, fileIDs: [1, 2], journal: journal) { url in
            if url.lastPathComponent == "second.jpg" { journalWhenSecondMoved = TrashJournal.load(at: journal) }
            let landed = bin.appendingPathComponent(url.lastPathComponent)
            try FileManager.default.moveItem(at: url, to: landed)
            return landed
        }
        #expect(result.succeeded == 2)
        // A kill at this moment must leave first.jpg restorable (its Trash location is
        // recorded) and second.jpg findable (it is journaled ahead of its move).
        let items = journalWhenSecondMoved.flatMap(\.items)
        #expect(items.map(\.fileID) == [1, 2])
        #expect(items.first?.trashedPath.isEmpty == false)
        #expect(items.last?.trashedPath.isEmpty == true)
        let batchID = try #require(result.action.split(separator: ":").last.map(String.init))
        #expect(TrashJournal.load(at: journal).map(\.id) == [batchID])
        #expect(TrashJournal.load(at: journal).first?.items.map(\.fileID) == [1, 2])
        let restored = await TrashJournal.restore(batchID: batchID, database: db, journal: journal)
        #expect(restored.succeeded == 2)
        #expect(try String(contentsOf: first, encoding: .utf8) == "one")
    }

    @Test("restoreFromTrash puts back a batch the Rust engine journaled, with its catalog snapshot")
    func restoresARustBatch() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let bin = root.appendingPathComponent("FakeTrash")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        let original = root.appendingPathComponent("Trip/photo.jpg")
        let trashed = bin.appendingPathComponent("photo.jpg")
        try Data("rust".utf8).write(to: trashed)
        let inode = try #require(Discovery.inode(of: trashed))
        // One line exactly as the Rust engine's `trash_log::append` writes it (BTreeMap column
        // order, base64 blobs, null as {}, other platforms' fields absent).
        let json = """
        {"batch_id":"rust-batch","timestamp":1700000000.25,"items":[{"file_id":7,"original_path":"\(original.path)",\
        "source_identity":{"volume":1,"file":\(inode)},"trashed_path":"\(trashed.path)","rows":[\
        {"table":"files","columns":{"extension":{"text":"jpg"},"file_ref":{"int":\(Int64(bitPattern: inode))},"id":{"int":7},\
        "kind":{"text":"image"},"path_hash":{"int":\(StablePathHash.hash(original.path))},"path_text":{"text":"\(original.path)"},\
        "scanned_at":{"real":2.5},"size_bytes":{"int":4},"vlm_description":{}}},\
        {"table":"clip_embeddings","columns":{"embedding":{"blob":"AAECAw=="},"file_id":{"int":7},"model":{"text":"clip"}}},\
        {"table":"tags","columns":{"file_id":{"int":7},"score":{},"source":{"text":"user"},"tag":{"text":"beach"}}}]}]}
        """
        let key = Data((0..<32).map { UInt8($0) })
        try key.write(to: root.appendingPathComponent("log-hmac.key"))
        func signed(_ text: String, with key: Data) -> String {
            let mac = HMAC<SHA256>.authenticationCode(for: Data(text.utf8), using: SymmetricKey(data: key))
            return text + "\t" + Data(mac).map { String(format: "%02x", $0) }.joined() + "\n"
        }
        let forged = json.replacingOccurrences(of: "\"beach\"", with: "\"forged\"")
        try (signed(json, with: key) + signed(forged, with: Data(repeating: 9, count: 32)))
            .write(to: root.appendingPathComponent("trash_log.json"), atomically: true, encoding: .utf8)
        let journal = root.appendingPathComponent("trash-journal.json")
        let logBefore = try Data(contentsOf: root.appendingPathComponent("trash_log.json"))

        #expect(RustTrashLog.find("other", beside: journal) == nil)
        let result = await TrashJournal.restore(batchID: "rust-batch", database: db, journal: journal)
        #expect(result.succeeded == 1)
        #expect(try String(contentsOf: original, encoding: .utf8) == "rust")
        #expect(!FileManager.default.fileExists(atPath: trashed.path))
        let (path, tag, blob): (String?, String?, Data?) = try await db.pool.read { sql in
            (try String.fetchOne(sql, sql: "SELECT path_text FROM files WHERE id = 7"),
             try String.fetchOne(sql, sql: "SELECT tag FROM tags WHERE file_id = 7"),
             try Data.fetchOne(sql, sql: "SELECT embedding FROM clip_embeddings WHERE file_id = 7"))
        }
        #expect(path == original.path)
        #expect(tag == "beach")
        #expect(blob == Data([0, 1, 2, 3]))
        // Read-only: the Rust log is untouched, and a wrong key finds nothing.
        #expect(try Data(contentsOf: root.appendingPathComponent("trash_log.json")) == logBefore)
        try Data(repeating: 7, count: 32).write(to: root.appendingPathComponent("log-hmac.key"))
        #expect(RustTrashLog.find("rust-batch", beside: journal) == nil)
    }

    @Test("trashFiles dual-writes the batch in the Rust engine's signed journal")
    func trashDualWritesTheRustJournal() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let bin = root.appendingPathComponent("FakeTrash")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        let photo = root.appendingPathComponent("photo.jpg")
        try Data("one".utf8).write(to: photo)
        let identity = try #require(RustTrashLog.identity(of: photo))
        try await insert(db, id: 1, path: photo.path)
        let journal = root.appendingPathComponent("trash-journal.json")
        let result = await FileIDEngineMain.trashFiles(database: db, fileIDs: [1], journal: journal) { url in
            let landed = bin.appendingPathComponent(url.lastPathComponent)
            try FileManager.default.moveItem(at: url, to: landed)
            return landed
        }
        let batchID = try #require(result.action.split(separator: ":").last.map(String.init))

        let key = try #require(FileManager.default.contents(atPath: root.appendingPathComponent("log-hmac.key").path))
        #expect(key.count == 32)
        let text = try String(contentsOf: RustTrashLog.url(beside: journal), encoding: .utf8)
        let lines = text.split(separator: "\n")
        #expect(lines.count == 1)
        let parts = try #require(lines.first).split(separator: "\t").map(String.init)
        #expect(parts.count == 2)
        let mac = HMAC<SHA256>.authenticationCode(for: Data(parts[0].utf8), using: SymmetricKey(data: key))
        #expect(Data(mac).map { String(format: "%02x", $0) }.joined() == parts[1])

        // The exact keys the Rust engine's `TrashLogEntry` reads.
        let object = try #require(try JSONSerialization.jsonObject(with: Data(parts[0].utf8)) as? [String: Any])
        #expect(object["batch_id"] as? String == batchID)
        let item = try #require((object["items"] as? [[String: Any]])?.first)
        #expect(item["file_id"] as? Int == 1)
        #expect(item["original_path"] as? String == photo.path)
        #expect(item["trashed_path"] as? String == bin.appendingPathComponent("photo.jpg").path)
        let source = try #require(item["source_identity"] as? [String: Any])
        #expect((source["file"] as? NSNumber)?.uint64Value == identity.file)
        let tables = (item["rows"] as? [[String: Any]])?.compactMap { $0["table"] as? String }
        #expect(tables?.contains("files") == true && tables?.contains("tags") == true)

        // A second batch reuses the key and appends.
        try Data("two".utf8).write(to: photo)
        try await insert(db, id: 2, path: photo.path)
        _ = await FileIDEngineMain.trashFiles(database: db, fileIDs: [2], journal: journal) { url in
            let landed = bin.appendingPathComponent("photo 2.jpg")
            try FileManager.default.moveItem(at: url, to: landed)
            return landed
        }
        let again = try String(contentsOf: RustTrashLog.url(beside: journal), encoding: .utf8)
        #expect(again.split(separator: "\n").count == 2)
        #expect(FileManager.default.contents(atPath: root.appendingPathComponent("log-hmac.key").path) == key)
    }

    @Test("Cleanup's exact duplicates are re-proved by the engine, journaled and restorable")
    func exactDuplicateTrashIsVerifiedAndUndoable() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let bin = root.appendingPathComponent("FakeTrash")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        let keep = root.appendingPathComponent("keep.jpg"), copy = root.appendingPathComponent("copy.jpg")
        let changed = root.appendingPathComponent("changed.jpg"), orphan = root.appendingPathComponent("orphan.jpg")
        for url in [keep, copy, changed, orphan] { try Data("same".utf8).write(to: url) }
        for (id, url) in [(Int64(1), copy), (2, changed), (3, orphan)] {
            try await insert(db, id: id, path: url.path)
            try await db.pool.write { try $0.execute(sql: "UPDATE files SET size_bytes = 4 WHERE id = ?", arguments: [id]) }
        }
        let sha = ExactFileDigest.compute(url: keep, expectedSize: 4)!.map { String(format: "%02x", $0) }.joined()
        func evidence(_ id: Int64, _ url: URL, keeper: URL) -> ExactTrashIdentity {
            ExactTrashIdentity(fileID: id, path: url.path, sizeBytes: 4, sha256Hex: sha,
                               keeperPath: keeper.path, keeperSizeBytes: 4, keeperSha256Hex: sha)
        }
        try Data("diff".utf8).write(to: changed)
        let missingKeeper = root.appendingPathComponent("gone.jpg")
        let exact: [Int64: ExactTrashIdentity] = [1: evidence(1, copy, keeper: keep), 2: evidence(2, changed, keeper: keep), 3: evidence(3, orphan, keeper: missingKeeper)]
        let journal = root.appendingPathComponent("trash-journal.json")
        let result = await FileIDEngineMain.trashFiles(database: db, fileIDs: [1, 2, 3], exact: exact, journal: journal) { url in
            let landed = bin.appendingPathComponent(url.lastPathComponent)
            try FileManager.default.moveItem(at: url, to: landed)
            return landed
        }
        #expect(result.succeeded == 1 && result.failed == 2)
        #expect(!FileManager.default.fileExists(atPath: copy.path))
        #expect(FileManager.default.fileExists(atPath: changed.path) && FileManager.default.fileExists(atPath: orphan.path))
        #expect(TrashJournal.load(at: journal).first?.items.map(\.fileID) == [1])
        let batchID = try #require(result.action.split(separator: ":").last.map(String.init))
        let restored = await TrashJournal.restore(batchID: batchID, database: db, journal: journal)
        #expect(restored.succeeded == 1)
        #expect(try String(contentsOf: copy, encoding: .utf8) == "same")
        let tag = try await db.pool.read { try String.fetchOne($0, sql: "SELECT tag FROM tags WHERE file_id = 1") }
        #expect(tag == "mine")
    }

    @Test("restoreFromTrash keeps the batch journaled while files are moving back")
    func restoreKeepsTheJournalUntilEachFileIsBack() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let journal = root.appendingPathComponent("trash-journal.json")
        var items: [TrashJournal.Item] = []
        for id in [Int64(1), 2] {
            let original = root.appendingPathComponent("shot\(id).jpg")
            try Data("bytes\(id)".utf8).write(to: original)
            try await insert(db, id: id, path: original.path)
            let rows = try await db.pool.read { sql in try TrashJournal.snapshot(sql, fileID: id) }
            let parked = root.appendingPathComponent("parked\(id).jpg")
            try FileManager.default.moveItem(at: original, to: parked)
            try await db.pool.write { sql in try sql.execute(sql: "DELETE FROM files WHERE id=?", arguments: [id]) }
            items.append(TrashJournal.Item(fileID: id, originalPath: original.path, trashedPath: parked.path, rows: rows))
        }
        let batch = TrashJournal.Batch(id: UUID().uuidString, createdAt: 1, items: items)
        try TrashJournal.append(batch, at: journal)
        var journalDuringFirstMove: [TrashJournal.Batch] = []
        let result = await TrashJournal.restore(batchID: batch.id, database: db, journal: journal) { from, to in
            if from.lastPathComponent == "parked1.jpg" { journalDuringFirstMove = TrashJournal.load(at: journal) }
            try FileManager.default.moveItem(at: from, to: to)
        }
        #expect(result.succeeded == 2)
        // A kill during the first move must not forget shot2.jpg, which is still in the Trash.
        #expect(journalDuringFirstMove.first { $0.id == batch.id }?.items.contains { $0.fileID == 2 } == true)
        #expect(TrashJournal.load(at: journal).isEmpty)
    }

    @Test("A retried restore finishes a file a killed restore had already moved back")
    func restoreRetryFinishesAFileAlreadyMovedBack() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let journal = root.appendingPathComponent("trash-journal.json")
        let original = root.appendingPathComponent("shot.jpg")
        try Data("bytes".utf8).write(to: original)
        try await insert(db, id: 7, path: original.path)
        let rows = try await db.pool.read { sql in try TrashJournal.snapshot(sql, fileID: 7) }
        try await db.pool.write { sql in try sql.execute(sql: "DELETE FROM files WHERE id=7") }
        // The killed restore moved the file home but died before re-inserting its rows.
        let batch = TrashJournal.Batch(id: UUID().uuidString, createdAt: 1, items: [
            TrashJournal.Item(fileID: 7, originalPath: original.path,
                              trashedPath: root.appendingPathComponent("gone.jpg").path, rows: rows),
        ])
        try TrashJournal.append(batch, at: journal)
        let result = await TrashJournal.restore(batchID: batch.id, database: db, journal: journal)
        #expect(result.succeeded == 1 && result.failed == 0)
        let tags = try await db.pool.read { sql in
            try String.fetchAll(sql, sql: "SELECT tag FROM tags WHERE file_id=7 AND source='user'")
        }
        #expect(tags == ["mine"])
        #expect(TrashJournal.load(at: journal).isEmpty)
    }

    @Test("A retried restore leaves an unrelated file at the original path alone")
    func restoreRetryRefusesAnUnrelatedOccupant() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let journal = root.appendingPathComponent("trash-journal.json")
        let original = root.appendingPathComponent("shot.jpg")
        try Data("old".utf8).write(to: original)
        try await insert(db, id: 7, path: original.path)
        let rows = try await db.pool.read { sql in try TrashJournal.snapshot(sql, fileID: 7) }
        try await db.pool.write { sql in try sql.execute(sql: "DELETE FROM files WHERE id=7") }
        try FileManager.default.removeItem(at: original)
        let keeper = root.appendingPathComponent("keeper.jpg")
        try Data("keeper".utf8).write(to: keeper)
        try Data("someone else".utf8).write(to: original)
        let batch = TrashJournal.Batch(id: UUID().uuidString, createdAt: 1, items: [
            TrashJournal.Item(fileID: 7, originalPath: original.path,
                              trashedPath: root.appendingPathComponent("gone.jpg").path, rows: rows),
        ])
        try TrashJournal.append(batch, at: journal)
        let result = await TrashJournal.restore(batchID: batch.id, database: db, journal: journal)
        #expect(result.succeeded == 0 && result.failed == 1)
        #expect(try await db.pool.read { sql in try Int.fetchOne(sql, sql: "SELECT COUNT(*) FROM files") } == 0)
    }

    @Test("Undo of an export removes an output published just before the engine was killed")
    func toolsUndoRemovesAnOutputPublishedBeforeACrash() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let out = root.appendingPathComponent("out")
        try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
        let output = out.appendingPathComponent("shot - Export.jpg")
        try Data("export".utf8).write(to: output)
        let source = root.appendingPathComponent("shot.jpg")
        try Data("source".utf8).write(to: source)
        // State a kill leaves between linking the output and recording it as completed:
        // the receipt (with the output's hash) is persisted, its state is still pending.
        let item = MediaTools.Item(output: ToolOutput(fileID: 1, sourcePath: source.path, outputPath: output.path),
                                   sourceHash: try MediaTools.hash(source), chapters: [])
        let plan = MediaTools.Plan(recipe: ToolRecipe(kind: "photo", format: "jpeg", maxDimension: 800), items: [item])
        let receipt = MediaTools.Receipt(output: item.output, hash: try MediaTools.hash(output))
        let id = UUID().uuidString
        try await db.pool.write { sql in
            try sql.execute(sql: "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?,?,?,'failed',0)",
                            arguments: [id, String(decoding: try JSONEncoder().encode(plan), as: UTF8.self),
                                        String(decoding: try JSONEncoder().encode([receipt]), as: UTF8.self)])
        }
        let response = await MediaTools.shared.handle(
            ToolRequest(requestID: "undo", action: "undo", destination: out.path, operationID: id), database: db)
        #expect(response.status == "ok")
        #expect(!FileManager.default.fileExists(atPath: output.path))
        #expect(try String(contentsOf: source, encoding: .utf8) == "source")
    }

    @Test("A file deleted mid-scan after discovery does not get a new catalog row")
    func vanishedFileGetsNoFailedRow() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let gone = root.appendingPathComponent("deleted.jpg")
        let unreadable = root.appendingPathComponent("broken.jpg")
        try Data("not a jpeg".utf8).write(to: unreadable)
        let channel = AsyncChannel<TaggedFile>()
        let writer = DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(), sessionID: UUID().uuidString)
        let producer = Task {
            for url in [gone, unreadable] {
                await channel.send(TaggedFile(url: url, kind: "image", extension: "jpg", sizeBytes: 10,
                                              createdAt: nil, modifiedAt: nil, failed: true,
                                              errorMessage: "Could not decode image"))
            }
            channel.finish()
        }
        await writer.drain(channel)
        await producer.value
        let paths = try await db.pool.read { sql in try String.fetchAll(sql, sql: "SELECT path_text FROM files") }
        #expect(paths == [unreadable.path])
    }

    @Test("A retried restructure undo accepts a file a killed undo had already moved back")
    func undoRetryAcceptsAFileAlreadyMovedBack() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let library = root.appendingPathComponent("lib")
        let group = library.appendingPathComponent("Group")
        try FileManager.default.createDirectory(at: group, withIntermediateDirectories: true)
        let home = library.appendingPathComponent("shot.jpg")
        let moved = group.appendingPathComponent("shot.jpg")
        try Data("bytes".utf8).write(to: home)
        try await insert(db, id: 3, path: home.path)
        // The killed undo moved the file home but its catalog row still names the group folder.
        try await db.pool.write { sql in
            try sql.execute(sql: "UPDATE files SET path_text=?, path_hash=? WHERE id=3",
                            arguments: [moved.path, StablePathHash.hash(moved.path)])
        }
        let journal = root.appendingPathComponent("restructure_undo.ndjson")
        let handle = try Restructure.openUndoJournalTruncating(at: journal)
        try Restructure.appendUndoEntry(Restructure.UndoEntry(fileID: 3, from: moved.path, to: home.path), to: handle)
        try handle.close()
        let result = await Restructure.undoLast(database: db, libraryRoot: library, isCancelled: { false }, undoJournal: journal)
        #expect(result.failed == 0)
        #expect(FileManager.default.fileExists(atPath: home.path))
        let path = try await db.pool.read { sql in try String.fetchOne(sql, sql: "SELECT path_text FROM files WHERE id=3") }
        #expect(path == home.path)
        #expect(!FileManager.default.fileExists(atPath: journal.path))
        #expect(!FileManager.default.fileExists(atPath: group.path))
    }

    @Test("A file trashed just before a kill is journaled ahead of the move and restorable by identity")
    func killRightAfterTheMoveStillRestores() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let bin = root.appendingPathComponent("FakeTrash")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        let photo = root.appendingPathComponent("photo.jpg")
        try Data("bytes".utf8).write(to: photo)
        try await insert(db, id: 1, path: photo.path)
        let journal = root.appendingPathComponent("trash-journal.json")
        var journalAtTheMove: [TrashJournal.Batch] = []
        _ = await FileIDEngineMain.trashFiles(database: db, fileIDs: [1], journal: journal) { url in
            journalAtTheMove = TrashJournal.load(at: journal)
            let landed = bin.appendingPathComponent(url.lastPathComponent)
            try FileManager.default.moveItem(at: url, to: landed)
            return landed
        }
        let pending = try #require(journalAtTheMove.first?.items.first, "the file is journaled before it moves")
        #expect(pending.trashedPath.isEmpty, "its Trash location is not known yet")

        // Simulate the kill: only the write-ahead entry survives, and the catalog row was not deleted.
        try TrashJournal.save(journalAtTheMove, at: journal)
        try await insert(db, id: 1, path: photo.path)
        await TrashJournal.reconcile(database: db, journal: journal, trashFolders: [bin])
        let rows = try await db.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM files") ?? -1 }
        #expect(rows == 0, "startup drops the row of a file that is provably in the Trash")
        #expect(TrashJournal.load(at: journal).first?.items.first?.trashedPath == bin.appendingPathComponent("photo.jpg").path)

        let restored = await TrashJournal.restore(batchID: journalAtTheMove[0].id, database: db, journal: journal)
        #expect(restored.succeeded == 1, "\(restored)")
        #expect(try String(contentsOf: photo, encoding: .utf8) == "bytes")
    }

    @Test("A write-ahead entry whose file never moved is completed as already home")
    func killBeforeTheMoveLeavesTheFileInPlace() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let photo = root.appendingPathComponent("stay.jpg")
        try Data("here".utf8).write(to: photo)
        try await insert(db, id: 1, path: photo.path)
        let rows = try await db.pool.read { try TrashJournal.snapshot($0, fileID: 1) }
        let journal = root.appendingPathComponent("trash-journal.json")
        let batch = TrashJournal.Batch(id: "b1", createdAt: 1, items: [TrashJournal.Item(fileID: 1, originalPath: photo.path, trashedPath: "", rows: rows)])
        try TrashJournal.save([batch], at: journal)
        await TrashJournal.reconcile(database: db, journal: journal, trashFolders: [root.appendingPathComponent("FakeTrash")])
        let kept = try await db.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM files") ?? -1 }
        #expect(kept == 1, "a file that never left keeps its row")
        #expect(TrashJournal.load(at: journal).isEmpty, "nothing left to restore once the only file is home")
    }

    @Test("A rename killed between the move and the catalog update is repaired at startup")
    func renameKilledBeforeTheCatalogUpdateIsRepaired() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let old = root.appendingPathComponent("IMG_1.jpg"), new = root.appendingPathComponent("Beach.jpg")
        try Data("x".utf8).write(to: old)
        try await insert(db, id: 1, path: old.path)
        let log = root.appendingPathComponent("moves.ndjson")
        let ref = Discovery.inode(of: old)
        // Simulate the kill: the intent was logged and the file moved, but the row still names the old path.
        PathMoveJournal.begin(fileID: 1, from: old.path, to: new.path, fileRef: ref, at: log)
        try FileManager.default.moveItem(at: old, to: new)
        await PathMoveJournal.reconcile(database: db, at: log)
        let path = try await db.pool.read { try String.fetchOne($0, sql: "SELECT path_text FROM files WHERE id = 1") }
        #expect(path == new.path)
        #expect(PathMoveJournal.pending(at: log).isEmpty, "the log is cleared once repaired")
    }

    @Test("A rename intent whose file never moved, or moved somewhere else, changes nothing")
    func renameIntentWithoutTheMoveIsIgnored() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let old = root.appendingPathComponent("IMG_2.jpg")
        try Data("y".utf8).write(to: old)
        try await insert(db, id: 1, path: old.path)
        let log = root.appendingPathComponent("moves.ndjson")
        PathMoveJournal.begin(fileID: 1, from: old.path, to: root.appendingPathComponent("Never.jpg").path, fileRef: Discovery.inode(of: old), at: log)
        // A different file now sits where a second intent pointed.
        let other = root.appendingPathComponent("Other.jpg")
        try Data("z".utf8).write(to: other)
        PathMoveJournal.begin(fileID: 1, from: old.path, to: other.path, fileRef: Discovery.inode(of: old), at: log)
        await PathMoveJournal.reconcile(database: db, at: log)
        let path = try await db.pool.read { try String.fetchOne($0, sql: "SELECT path_text FROM files WHERE id = 1") }
        #expect(path == old.path)
    }

    @Test("A finished rename leaves nothing pending")
    func finishedRenameClearsItsIntent() {
        let log = FileManager.default.temporaryDirectory.appendingPathComponent("moves-\(UUID().uuidString).ndjson")
        defer { try? FileManager.default.removeItem(at: log) }
        let id = PathMoveJournal.begin(fileID: 7, from: "/a/x.jpg", to: "/a/y.jpg", fileRef: 42, at: log)
        PathMoveJournal.finish(id, at: log)
        #expect(PathMoveJournal.pending(at: log).isEmpty)
    }

    @Test("Tests never write engine state into the user's real Application Support folder")
    func testStateIsIsolated() {
        let real = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID", isDirectory: true)
        if ProcessInfo.processInfo.environment["FILEID_DATABASE_PATH"] == nil {
            #expect(EngineStateRoot.testRoot != nil, "the test process is detected")
            #expect(EngineStateRoot.directory != real)
        }
        #expect(!PathMoveJournal.defaultURL.path.hasPrefix(real.path + "/"))
        #expect(!TrashJournal.defaultURL.path.hasPrefix(real.path + "/"))
        #expect(!(Restructure.defaultUndoJournalURL?.path.hasPrefix(real.path + "/") ?? false))
    }
}
