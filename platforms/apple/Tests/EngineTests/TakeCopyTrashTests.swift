import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Trashing a take that has a byte-identical copy")
struct TakeCopyTrashTests {
    @Test("the kept copy survives when its identical copy is moved to the Trash")
    func keptCopySurvives() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDTakeCopy-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let bin = root.appendingPathComponent("FakeTrash")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        let kept = root.appendingPathComponent("kept.jpg"), copy = root.appendingPathComponent("copy.jpg")
        try Data("same bytes".utf8).write(to: kept)
        try Data("same bytes".utf8).write(to: copy)
        for (id, url) in [(Int64(1), kept), (Int64(2), copy)] {
            let fileRef = Discovery.inode(of: url).map { Int64(bitPattern: $0) }
            try await db.pool.write { sql in
                try sql.execute(
                    sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, file_ref, content_hash) VALUES (?,?,?,10,0,'image','jpg',?,?)",
                    arguments: [id, url.path, StablePathHash.hash(url.path), fileRef, Data(repeating: 7, count: 32)])
            }
        }
        let journal = root.appendingPathComponent("trash-journal.json")
        let result = await FileIDEngineMain.trashFiles(database: db, fileIDs: [2], journal: journal) { url in
            let landed = bin.appendingPathComponent(url.lastPathComponent)
            try FileManager.default.moveItem(at: url, to: landed)
            return landed
        }
        #expect(result.succeeded == 1)
        #expect(!FileManager.default.fileExists(atPath: copy.path))
        #expect(try String(contentsOf: kept, encoding: .utf8) == "same bytes")
        let remaining = try await db.pool.read { sql in try Int.fetchOne(sql, sql: "SELECT COUNT(*) FROM files WHERE id=1") }
        #expect(remaining == 1)
    }
}
