import Foundation
import GRDB
import Testing
@testable import FileID

@Suite("Unreadable files in the Library", .serialized)
struct ReadStoreUnreadableTests {
    @Test("unreadable files stay out of the normal listing and appear only under their filter")
    func unreadableFilter() throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-unreadable-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
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
                    vlm_inherited_from INTEGER, auto_tags TEXT)
                """)
            try db.execute(sql: """
                INSERT INTO files(id,path_text,kind,failed,error_message,scanned_at) VALUES
                    (1,'/lib/a.jpg','image',0,NULL,1),
                    (2,'/lib/b.gif','image',1,'Could not decode image',2),
                    (3,'/lib/c.pdf','pdf',0,NULL,3),
                    (4,'/lib/d.heic','image',1,'Could not decode image',4)
                """)
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        #expect(store.files().map(\.id) == [3, 1])
        #expect(store.files(kindFilter: "image").map(\.id) == [1])
        let unreadable = store.files(kindFilter: ReadStore.unreadableFilter)
        #expect(unreadable.map(\.id) == [4, 2])
        #expect(unreadable.allSatisfy { $0.failed })
    }

    @MainActor
    @Test("only rows that ran out of time count as timed out")
    func timedOutRows() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-timedout-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appending(component: "fileid.sqlite")
        try await DatabaseQueue(path: url.path).write { db in
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
                INSERT INTO files(id,path_text,kind,failed,error_message) VALUES
                    (1,'/lib/a.png','image',1,'per-file timeout after 60s'),
                    (2,'/lib/b.png','image',1,'Vision pass timed out'),
                    (3,'/lib/c.jpg','image',1,'Could not decode image'),
                    (4,'/lib/d.jpg','image',0,NULL)
                """)
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        store.refreshCounters()
        for _ in 0..<200 where store.unreadableFiles != 3 { try await Task.sleep(nanoseconds: 25_000_000) }
        #expect(store.unreadableFiles == 3)
        #expect(store.unreadableTimedOutFiles == 2)
        #expect(ReadStore.isTimedOut(errorMessage: "per-file timeout after 60s"))
        #expect(ReadStore.isTimedOut(errorMessage: "FileID ran out of time reading this file; it will try again"))
        #expect(!ReadStore.isTimedOut(errorMessage: "Could not decode image"))
        #expect(!ReadStore.isTimedOut(errorMessage: nil))
    }

    @MainActor
    @Test("files waiting for a retry are in no Library count, since the listing shows none of them")
    func retryPendingRowsAreNotCounted() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-retry-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appending(component: "fileid.sqlite")
        try await DatabaseQueue(path: url.path).write { db in
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
                INSERT INTO files(id,path_text,kind,failed,error_message) VALUES
                    (1,'/lib/a.pdf','pdf',1,'Could not read; will retry next scan'),
                    (2,'/lib/b.pdf','pdf',1,'Could not read; will retry next scan'),
                    (3,'/lib/c.png','image',1,'Could not decode image'),
                    (4,'/lib/d.jpg','image',0,NULL)
                """)
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        store.refreshCounters()
        for _ in 0..<200 where store.scopedUnreadableFiles != 1 { try await Task.sleep(nanoseconds: 25_000_000) }
        #expect(store.scopedUnreadableFiles == 1)
        #expect(store.scopedTotalFiles - store.scopedUnreadableFiles == store.files().count)
    }

    @MainActor
    @Test("a re-check by id returns the rows still in the listing and a count that matches the listing")
    func recheckByIDMatchesTheListing() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-recheck-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appending(component: "fileid.sqlite")
        try await DatabaseQueue(path: url.path).write { db in
            try db.execute(sql: """
                CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, size_bytes INTEGER NOT NULL DEFAULT 1,
                    created_at REAL, modified_at REAL, scanned_at REAL NOT NULL DEFAULT 0,
                    kind TEXT NOT NULL, extension TEXT NOT NULL DEFAULT '', phash INTEGER, aesthetic REAL,
                    has_faces INTEGER, has_text INTEGER, camera_model TEXT, location_lat REAL, location_lon REAL,
                    failed INTEGER NOT NULL DEFAULT 0, error_message TEXT, vlm_description TEXT,
                    vlm_proposed_name TEXT, vlm_model TEXT, vlm_full_model TEXT, vlm_analyzed_at REAL,
                    vlm_inherited_from INTEGER, auto_tags TEXT)
                """)
            try db.execute(sql: """
                INSERT INTO files(id,path_text,kind,failed,error_message,size_bytes) VALUES
                    (1,'/lib/a.jpg','image',0,NULL,10),
                    (2,'/lib/b.mov','video',0,NULL,20),
                    (3,'/lib/c.png','image',1,'Could not decode image',30),
                    (4,'/lib/d.jpg','image',0,NULL,40)
                """)
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        let all = await store.listedRowsAsync(ids: [1, 2, 3, 4, 99])
        #expect(Set(all.keys) == [1, 2, 4])
        #expect(all[4]?.sizeBytes == 40)
        let images = await store.listedRowsAsync(ids: [1, 2, 3, 4], kindFilter: "image")
        #expect(Set(images.keys) == [1, 4])
        #expect(await store.listingCountAsync() == store.files().count)
        #expect(await store.listingCountAsync(kindFilter: "image") == 2)
        #expect(await store.listingCountAsync(kindFilter: ReadStore.unreadableFilter) == 1)
    }
}
