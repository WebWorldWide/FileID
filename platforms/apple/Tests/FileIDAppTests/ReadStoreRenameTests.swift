import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileID

@Suite("Read store renames", .serialized)
struct ReadStoreRenameTests {
    @Test("apply and undo keep path_hash in step with path_text")
    func renameRefreshesPathHash() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDRename-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let dbURL = root.appendingPathComponent("catalog.sqlite")
        var originals: [String] = []
        let database = try DatabaseQueue(path: dbURL.path)
        try database.write { db in
            try db.execute(sql: """
                CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, path_hash INTEGER NOT NULL, path_search TEXT,
                    size_bytes INTEGER NOT NULL DEFAULT 4, created_at REAL, modified_at REAL, scanned_at REAL NOT NULL DEFAULT 0,
                    kind TEXT NOT NULL DEFAULT 'image', extension TEXT NOT NULL DEFAULT 'jpg', phash BLOB, aesthetic REAL,
                    has_faces INTEGER DEFAULT 0, has_text INTEGER DEFAULT 0, camera_model TEXT, location_lat REAL, location_lon REAL,
                    failed INTEGER DEFAULT 0, error_message TEXT, vlm_description TEXT, vlm_proposed_name TEXT,
                    vlm_model TEXT, vlm_full_model TEXT, vlm_analyzed_at REAL, auto_tags TEXT)
                """)
            for id in 1...2 {
                let path = root.appendingPathComponent("IMG_000\(id).jpg").path
                try Data("img\(id)".utf8).write(to: URL(fileURLWithPath: path))
                originals.append(path)
                try db.execute(
                    sql: "INSERT INTO files(id, path_text, path_hash, vlm_proposed_name) VALUES(?,?,?,?)",
                    arguments: [id, path, StablePathHash.hash(path), "beach-day-\(id)"])
            }
        }
        try database.close()

        let store = ReadStore(dbURL: dbURL)
        store.openIfPossible()
        defer { store.close() }

        func rows() throws -> [(path: String, hash: Int64)] {
            try DatabaseQueue(path: dbURL.path).read { db in
                try Row.fetchAll(db, sql: "SELECT path_text, path_hash FROM files ORDER BY id")
                    .map { ($0["path_text"], $0["path_hash"]) }
            }
        }

        let result = store.applyProposedNamesBulk(store.filesWithProposedNames())
        #expect(result.renamed.count == 2)
        for row in try rows() {
            #expect(row.path.contains("beach-day-"))
            #expect(row.hash == StablePathHash.hash(row.path))
        }

        let undo = store.undoRenames(result.renamed)
        #expect(undo.undone == 2)
        let restored = try rows()
        #expect(restored.map(\.path) == originals)
        for row in restored {
            #expect(row.hash == StablePathHash.hash(row.path))
        }
    }
}
