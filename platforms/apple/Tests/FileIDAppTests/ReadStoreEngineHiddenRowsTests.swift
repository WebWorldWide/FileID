import Foundation
import GRDB
import Testing
@testable import FileID

@Suite("Rows the Rust engine hides", .serialized)
@MainActor
struct ReadStoreEngineHiddenRowsTests {
    private func seed(_ root: URL) throws -> URL {
        let url = root.appending(component: "fileid.sqlite")
        try DatabaseQueue(path: url.path).write { db in
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
            try db.execute(sql: """
                INSERT INTO files(id,path_text,kind,failed,error_message,scanned_at) VALUES
                    (1,'/lib/a.jpg','image',0,NULL,1),
                    (2,'/lib/gone.jpg','image',1,'File is no longer present under the completed scan root.',2),
                    (3,'/lib/empty.txt','other',1,'File is zero bytes and has no indexable content.',3),
                    (4,'/lib/bad.gif','image',1,'Could not decode image',4),
                    (5,'/lib/slow.pdf','pdf',1,'Could not open PDF (will retry next scan)',5)
                """)
        }
        return url
    }

    @Test("removed and empty rows are not unreadable, not listed and not in the totals")
    func hiddenRowsStayOut() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-hidden-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = try seed(root)
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        for _ in 0..<200 where store.totalFiles != 3 { try await Task.sleep(nanoseconds: 25_000_000) }
        #expect(store.totalFiles == 3)
        #expect(store.unreadableFiles == 1)
        #expect(store.files().map(\.id) == [1])
        #expect(store.files(kindFilter: ReadStore.unreadableFilter).map(\.id) == [4])
        #expect(store.files(search: "gone").isEmpty)
        #expect(store.files(search: "empty").isEmpty)
    }

    @Test("an Organize Folders plan counts only the files under its own folder")
    func filesUnderAPlanRoot() throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-under-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = try seed(root)
        try DatabaseQueue(path: url.path).write { db in
            try db.execute(sql: """
                INSERT INTO files(id,path_text,kind) VALUES
                    (10,'/lib/2021/a.pdf','pdf'), (11,'/lib/2021/x/b.pdf','pdf'), (12,'/lib/2021-old/c.pdf','pdf'),
                    (13,'/lib/20210/d.pdf','pdf'), (14,'/other/e.jpg','image')
                """)
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        #expect(store.countFiles(under: "/lib/2021") == 2)
        #expect(store.countFiles(under: "/lib/2021/") == 2)
        #expect(store.countFiles(under: "/lib") == 7)
        #expect(store.countFiles(under: "/nowhere") == 0)
        #expect(store.countFiles(under: "/") == 8)
    }

    @Test("the file count is unknown until the counters load, a real zero for a missing database, and unknown again when the reader fails")
    func catalogedFileCountKnownness() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-known-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }

        let seeded = try seed(root)
        let store = ReadStore(dbURL: seeded)
        #expect(store.catalogedFileCount == nil)
        store.openIfPossible()
        defer { store.close() }
        for _ in 0..<200 where store.catalogedFileCount == nil { try await Task.sleep(nanoseconds: 25_000_000) }
        #expect(store.catalogedFileCount == 3)

        let missing = ReadStore(dbURL: root.appending(component: "none.sqlite"))
        missing.openIfPossible()
        #expect(missing.catalogedFileCount == 0)

        let broken = root.appending(component: "broken.sqlite")
        try Data("this is not a database".utf8).write(to: broken)
        let unreadable = ReadStore(dbURL: broken)
        unreadable.openIfPossible()
        defer { unreadable.close() }
        try await Task.sleep(nanoseconds: 400_000_000)
        #expect(unreadable.catalogedFileCount == nil)
    }

    @Test("the hidden messages match what the engine writes")
    nonisolated func messages() {
        #expect(ReadStore.engineHiddenMessages.count == 2)
        #expect(ReadStore.notEngineHiddenSQL.contains("File is no longer present under the completed scan root."))
    }

    @Test("look-alikes still group on a library with far more than 20,000 hashed photos")
    func lookAlikesBeyondTwentyThousandPhotos() throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-lookalike-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = try seed(root)
        try DatabaseQueue(path: url.path).write { db in
            var state: UInt64 = 0x9E37_79B9_7F4A_7C15
            for id in 100..<30_100 {
                state = state &* 6364136223846793005 &+ 1442695040888963407
                let hash = Int64(bitPattern: state | 1)
                try db.execute(sql: "INSERT INTO files(id,path_text,kind,phash,content_hash) VALUES (?,?,'image',?,?)",
                               arguments: [id, "/lib/p\(id).jpg", hash, "h\(id)"])
                if id == 200 {
                    try db.execute(sql: "INSERT INTO files(id,path_text,kind,phash,content_hash) VALUES (?,?,'image',?,?)",
                                   arguments: [90, "/lib/near.jpg", hash ^ 0b11, "other"])
                }
            }
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        let groups = store.similarImageGroups(maxHamming: 8)
        #expect(groups.count == 1)
        #expect(Set(groups.first?.files.map(\.id) ?? []) == [90, 200])
    }
}
