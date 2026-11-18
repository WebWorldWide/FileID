import CryptoKit
import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

/// PT-9: after wipeLibrary the catalog has no roots, yet FileID must still put back what it
/// trashed. The signed Rust Trash log records the library root each file was trashed from.
@Suite("Trash put back after wipeLibrary")
struct TrashPutBackAfterWipeTests {
    private struct Fixture {
        let base: URL
        let library: URL
        let bin: URL
        let journal: URL
        let db: FileIDEngine.Database
    }

    private func makeFixture() throws -> Fixture {
        let base = URL(fileURLWithPath: TrashJournal.canonicalPath(NSTemporaryDirectory()))
            .appendingPathComponent("FileIDWipeRestore-\(UUID().uuidString)")
        let library = base.appendingPathComponent("library")
        let bin = base.appendingPathComponent("FakeTrash")
        for folder in [library.appendingPathComponent("docs"), bin] {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        }
        let db = try FileIDEngine.Database(at: base.appendingPathComponent("catalog.sqlite"))
        return Fixture(base: base, library: library, bin: bin, journal: base.appendingPathComponent("trash-journal.json"), db: db)
    }

    private func addRoot(_ db: FileIDEngine.Database, _ root: URL) async throws {
        try await db.pool.write {
            try $0.execute(sql: "INSERT INTO library_roots(path,display,kind,added_at) VALUES(?,?, 'internal',1)", arguments: [root.path, root.path])
        }
    }

    private func addFile(_ db: FileIDEngine.Database, id: Int64, at url: URL) async throws {
        try Data("file \(id)".utf8).write(to: url)
        let ref = Discovery.inode(of: url).map { Int64(bitPattern: $0) }
        try await db.pool.write {
            try $0.execute(
                sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, file_ref) VALUES (?,?,?,1,0,'doc','txt',?)",
                arguments: [id, url.path, StablePathHash.hash(url.path), ref])
        }
    }

    private func trash(_ f: Fixture, ids: [Int64]) async -> String {
        let result = await FileIDEngineMain.trashFiles(database: f.db, fileIDs: ids, journal: f.journal) { url in
            let landed = f.bin.appendingPathComponent(url.lastPathComponent)
            try FileManager.default.moveItem(at: url, to: landed)
            return landed
        }
        return result.action.split(separator: ":").last.map(String.init) ?? ""
    }

    private func fileCount(_ db: FileIDEngine.Database) async throws -> Int {
        try await db.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM files") ?? -1 }
    }

    private func signedLine(_ json: String, key: Data) -> String {
        let mac = HMAC<SHA256>.authenticationCode(for: Data(json.utf8), using: SymmetricKey(data: key))
        return json + "\t" + Data(mac).map { String(format: "%02x", $0) }.joined() + "\n"
    }

    /// One line as the Rust engine's `trash_log::append` writes it for a catalog-less put back.
    private func rustLine(batch: String, original: URL, trashed: URL, root: String?, key: Data) -> String {
        let rootField = root.map { ",\"library_root\":\"\($0)\"" } ?? ""
        let json = """
        {"batch_id":"\(batch)","timestamp":1700000000.5,"items":[{"file_id":4,"original_path":"\(original.path)"\(rootField),\
        "trashed_path":"\(trashed.path)","rows":[{"table":"files","columns":{"extension":{"text":"txt"},"file_ref":{},\
        "id":{"int":4},"kind":{"text":"doc"},"path_hash":{"int":\(StablePathHash.hash(original.path))},\
        "path_text":{"text":"\(original.path)"},"scanned_at":{"real":2.5},"size_bytes":{"int":1}}}]}]}
        """
        return signedLine(json, key: key)
    }

    private func writeRustLog(_ f: Fixture, _ lines: String, key: Data = Data((0..<32).map { UInt8($0) })) throws {
        try key.write(to: f.base.appendingPathComponent("log-hmac.key"))
        try lines.write(to: RustTrashLog.url(beside: f.journal), atomically: true, encoding: .utf8)
    }

    @Test("A Swift Trash batch records its library root inside the signed line, only when a root contains the file")
    func swiftWritesTheLibraryRoot() async throws {
        let f = try makeFixture()
        defer { try? FileManager.default.removeItem(at: f.base) }
        try await addRoot(f.db, f.library)
        try await addRoot(f.db, f.library.appendingPathComponent("docs"))
        let inside = f.library.appendingPathComponent("docs/a.txt")
        let stray = f.base.appendingPathComponent("stray.txt")
        try await addFile(f.db, id: 1, at: inside)
        try await addFile(f.db, id: 2, at: stray)
        _ = await trash(f, ids: [1, 2])

        let key = try #require(FileManager.default.contents(atPath: f.base.appendingPathComponent("log-hmac.key").path))
        let text = try String(contentsOf: RustTrashLog.url(beside: f.journal), encoding: .utf8)
        let parts = try #require(text.split(separator: "\n").first).split(separator: "\t").map(String.init)
        let mac = HMAC<SHA256>.authenticationCode(for: Data(parts[0].utf8), using: SymmetricKey(data: key))
        #expect(Data(mac).map { String(format: "%02x", $0) }.joined() == parts[1], "the root is inside the signed content")
        let object = try #require(try JSONSerialization.jsonObject(with: Data(parts[0].utf8)) as? [String: Any])
        let items = try #require(object["items"] as? [[String: Any]])
        #expect(items[0]["library_root"] as? String == f.library.appendingPathComponent("docs").path, "the innermost root wins")
        #expect(items[1]["library_root"] == nil, "no root contains the file, so none is recorded")
        let ownJournal = try String(contentsOf: f.journal, encoding: .utf8)
        #expect(!ownJournal.contains("libraryRoot") && !ownJournal.contains("library_root"), "the unsigned Swift journal never stores a root")
    }

    @Test("Swift trashes, wipeLibrary, Swift restores: from its own journal and from the signed log alone")
    func swiftRoundTripSurvivesTheWipe() async throws {
        for ownJournalSurvives in [true, false] {
            let f = try makeFixture()
            defer { try? FileManager.default.removeItem(at: f.base) }
            try await addRoot(f.db, f.library)
            let file = f.library.appendingPathComponent("docs/a.txt")
            try await addFile(f.db, id: 1, at: file)
            let batch = await trash(f, ids: [1])
            #expect(!batch.isEmpty && !FileManager.default.fileExists(atPath: file.path))

            let wiped = await FileIDEngineMain.wipeLibrary(database: f.db)
            #expect(wiped.ok)
            #expect(try await f.db.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM library_roots") } == 0, "the wipe takes the roots with it")
            if !ownJournalSurvives { try FileManager.default.removeItem(at: f.journal) }

            let restored = await TrashJournal.restore(batchID: batch, database: f.db, journal: f.journal)
            #expect(restored.succeeded == 1 && restored.failed == 0, "\(restored)")
            #expect(try String(contentsOf: file, encoding: .utf8) == "file 1")
            #expect(try await fileCount(f.db) == 1, "the catalog row comes back too")
        }
    }

    @Test("A Rust-written signed line with a library_root restores after a wipe, and only inside that root")
    func rustLineWithRecordedRootRestores() async throws {
        let f = try makeFixture()
        defer { try? FileManager.default.removeItem(at: f.base) }
        let original = f.library.appendingPathComponent("docs/rust.txt")
        let trashed = f.bin.appendingPathComponent("rust.txt")
        try Data("rust".utf8).write(to: trashed)
        let outside = f.base.appendingPathComponent("elsewhere/escape.txt")
        let outsideTrashed = f.bin.appendingPathComponent("escape.txt")
        try Data("escape".utf8).write(to: outsideTrashed)
        try writeRustLog(f, rustLine(batch: "rust-ok", original: original, trashed: trashed, root: f.library.path, key: Data((0..<32).map { UInt8($0) }))
            + rustLine(batch: "rust-escape", original: outside, trashed: outsideTrashed, root: f.library.path, key: Data((0..<32).map { UInt8($0) })))
        #expect(try await fileCount(f.db) == 0)

        let restored = await TrashJournal.restore(batchID: "rust-ok", database: f.db, journal: f.journal)
        #expect(restored.succeeded == 1, "\(restored)")
        #expect(try String(contentsOf: original, encoding: .utf8) == "rust")
        #expect(try await fileCount(f.db) == 1)

        let escaped = await TrashJournal.restore(batchID: "rust-escape", database: f.db, journal: f.journal)
        #expect(escaped.succeeded == 0 && escaped.failed == 1, "a path outside the recorded root is refused")
        #expect(!FileManager.default.fileExists(atPath: outside.path))
        #expect(FileManager.default.fileExists(atPath: outsideTrashed.path), "the Trash entry is left alone")
    }

    @Test("A signed line without a recorded root is refused after a wipe, and put back while a catalog root still covers it")
    func legacyRustLineNeedsACatalogRoot() async throws {
        let f = try makeFixture()
        defer { try? FileManager.default.removeItem(at: f.base) }
        let original = f.library.appendingPathComponent("docs/old.txt")
        let trashed = f.bin.appendingPathComponent("old.txt")
        try Data("old".utf8).write(to: trashed)
        try writeRustLog(f, rustLine(batch: "legacy", original: original, trashed: trashed, root: nil, key: Data((0..<32).map { UInt8($0) })))

        let refused = await TrashJournal.restore(batchID: "legacy", database: f.db, journal: f.journal)
        #expect(refused.succeeded == 0 && refused.failed == 1)
        #expect(!FileManager.default.fileExists(atPath: original.path))

        try await addRoot(f.db, f.library)
        let allowed = await TrashJournal.restore(batchID: "legacy", database: f.db, journal: f.journal)
        #expect(allowed.succeeded == 1, "\(allowed)")
        #expect(try String(contentsOf: original, encoding: .utf8) == "old")
    }

    @Test("A tampered library_root breaks the signature and nothing is put back; a recorded root that is gone authorizes nothing")
    func tamperedRootIsRefused() async throws {
        let f = try makeFixture()
        defer { try? FileManager.default.removeItem(at: f.base) }
        let key = Data((0..<32).map { UInt8($0) })
        let original = f.base.appendingPathComponent("elsewhere/victim.txt")
        let trashed = f.bin.appendingPathComponent("victim.txt")
        try Data("victim".utf8).write(to: trashed)

        let honest = rustLine(batch: "tampered", original: original, trashed: trashed, root: f.library.path, key: key)
        let widened = honest.replacingOccurrences(of: "\"library_root\":\"\(f.library.path)\"", with: "\"library_root\":\"\(f.base.path)\"")
        #expect(widened != honest)
        try writeRustLog(f, widened)
        #expect(RustTrashLog.find("tampered", beside: f.journal) == nil, "the edited line fails its HMAC")
        let tampered = await TrashJournal.restore(batchID: "tampered", database: f.db, journal: f.journal)
        #expect(tampered.succeeded == 0 && tampered.failed == 1)
        #expect(!FileManager.default.fileExists(atPath: original.path))

        let missingRoot = f.base.appendingPathComponent("no-such-root")
        let inMissing = missingRoot.appendingPathComponent("a.txt")
        try writeRustLog(f, rustLine(batch: "gone", original: inMissing, trashed: trashed, root: missingRoot.path, key: key))
        let gone = await TrashJournal.restore(batchID: "gone", database: f.db, journal: f.journal)
        #expect(gone.succeeded == 0 && gone.failed == 1)
        #expect(!FileManager.default.fileExists(atPath: inMissing.path))
    }
}
