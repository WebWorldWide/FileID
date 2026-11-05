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
}
