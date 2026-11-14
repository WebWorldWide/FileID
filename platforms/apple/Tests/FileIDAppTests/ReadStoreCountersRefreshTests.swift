import Foundation
import GRDB
import Testing
@testable import FileID

@Suite("Library counters", .serialized)
@MainActor
struct ReadStoreCountersRefreshTests {
    @Test("notifyChanged brings the file count up to rows written after the store opened")
    func countFollowsNotify() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-counters-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appending(component: "fileid.sqlite")
        let writer = try DatabaseQueue(path: url.path)
        try await writer.write { db in
            try db.execute(sql: """
                CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, size_bytes INTEGER NOT NULL DEFAULT 1,
                    created_at REAL, modified_at REAL, scanned_at REAL NOT NULL DEFAULT 0,
                    kind TEXT NOT NULL, extension TEXT NOT NULL DEFAULT '', phash INTEGER, aesthetic REAL,
                    has_faces INTEGER, has_text INTEGER, camera_model TEXT, location_lat REAL, location_lon REAL,
                    failed INTEGER NOT NULL DEFAULT 0, error_message TEXT, vlm_description TEXT,
                    vlm_proposed_name TEXT, vlm_model TEXT, vlm_full_model TEXT, vlm_analyzed_at REAL,
                    vlm_inherited_from INTEGER, auto_tags TEXT, content_hash TEXT)
                """)
            try db.execute(sql: "CREATE TABLE face_prints (id INTEGER PRIMARY KEY, file_id INTEGER, person_id INTEGER)")
            try db.execute(sql: """
                CREATE TABLE persons (id INTEGER PRIMARY KEY, is_unknown INTEGER, name TEXT, title TEXT,
                    first_name TEXT, middle_name TEXT, last_name TEXT, suffix TEXT)
                """)
            try db.execute(sql: "INSERT INTO files(id,path_text,kind) VALUES (1,'/lib/a.jpg','image')")
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        try await waitFor { store.totalFiles == 1 }
        try await writer.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,kind) VALUES (2,'/lib/b.jpg','image'),(3,'/lib/c.pdf','pdf')")
        }
        store.notifyChanged()
        try await waitFor { store.totalFiles == 3 }
        #expect(store.lastError == nil)
        #expect(store.totalFiles == 3)
    }

    @Test("a catalog created after launch is counted once the store is told to open it")
    func lateCatalog() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-counters-late-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appending(component: "fileid.sqlite")
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        #expect(store.totalFiles == 0)
        let writer = try DatabaseQueue(path: url.path)
        try await writer.write { db in
            try db.execute(sql: "CREATE TABLE files (id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, size_bytes INTEGER NOT NULL DEFAULT 1, kind TEXT NOT NULL, failed INTEGER NOT NULL DEFAULT 0, error_message TEXT, content_hash TEXT, vlm_proposed_name TEXT)")
            try db.execute(sql: "CREATE TABLE face_prints (id INTEGER PRIMARY KEY, file_id INTEGER, person_id INTEGER)")
            try db.execute(sql: "CREATE TABLE persons (id INTEGER PRIMARY KEY, is_unknown INTEGER, name TEXT, title TEXT, first_name TEXT, middle_name TEXT, last_name TEXT, suffix TEXT)")
            try db.execute(sql: "INSERT INTO files(id,path_text,kind) VALUES (1,'/lib/a.jpg','image'),(2,'/lib/b.pdf','pdf')")
        }
        store.openIfPossible()
        store.notifyChanged()
        try await waitFor { store.totalFiles == 2 }
        #expect(store.lastError == nil, "\(store.lastError ?? "")")
        #expect(store.totalFiles == 2)
    }

    private func waitFor(_ condition: () async -> Bool) async throws {
        for _ in 0..<80 where !(await condition()) { try await Task.sleep(for: .milliseconds(100)) }
    }
}
