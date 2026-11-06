import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileID

@Suite("Location scope", .serialized)
struct LibraryScopeTests {
    private func root(_ id: Int64, _ path: String, count: Int64 = 1, online: Bool = true) -> LibraryRoot {
        LibraryRoot(id: id, path: path, display: (path as NSString).lastPathComponent, kind: "external", enabled: true, online: online,
                    fileCount: count, totalBytes: 1, lastScanAt: nil, lastStatus: nil)
    }

    private var roots: [LibraryRoot] { [root(1, "/m/Camera/"), root(2, "/m/Camera2"), root(3, "/m/Camera/Synced"), root(4, "/elsewhere")] }

    @Test func aScopeKeepsItsOwnFilesNotLookalikesOrDeeperLocations() throws {
        let scope = try #require(LibraryScope(rootID: 1, roots: roots))
        for inside in ["/m/Camera", "/m/Camera/a.jpg", "/m/Camera/2020/b.jpg", "/m/Camera/Synced2/c.jpg"] { #expect(scope.contains(inside), "\(inside)") }
        for outside in ["/m/Camera2/a.jpg", "/m/Cameras/a.jpg", "/m/Camera/Synced", "/m/Camera/Synced/d.jpg", "/m/Cam", "/elsewhere/e.jpg"] {
            #expect(!scope.contains(outside), "\(outside)")
        }
        #expect(try #require(LibraryScope(rootID: 3, roots: roots)).contains("/m/Camera/Synced/d.jpg"))
        #expect(LibraryScope(rootID: 9, roots: roots) == nil)
        #expect(LibraryScope(rootID: nil, roots: roots) == nil)
    }

    @Test func theChoiceSurvivesLoadingAndFallsBackWhenItsLocationIsGone() {
        #expect(LibraryScope.reconciled(2, roots: []) == 2)
        #expect(LibraryScope.reconciled(2, roots: roots) == 2)
        #expect(LibraryScope.reconciled(9, roots: roots) == nil)
        #expect(LibraryScope.reconciled(nil, roots: roots) == nil)
    }

    @Test func theTitleBarNamesTheLocation() {
        #expect(LibraryScope.title(LibraryScope(rootID: 2, roots: roots), roots: roots, folderName: "Pictures") == "Camera2")
        #expect(LibraryScope.title(nil, roots: roots, folderName: "Pictures") == "All locations")
        #expect(LibraryScope.title(nil, roots: [], folderName: "Pictures") == "Pictures")
        #expect(LibraryScope.title(nil, roots: [], folderName: nil) == "Choose a folder")
        #expect(LibraryScope.spokenLabel(root(5, "/Volumes/Card", count: 1_204, online: false)) == "Card, \(Int64(1_204).formatted()) files, offline")
        #expect(LibraryScope.spokenLabel(root(6, "/Users/a/One", count: 1)) == "One, 1 file")
    }

    @Test func theLibraryListingAndCountersKeepToTheScope() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(component: "fileid-scope-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appending(component: "fileid.sqlite")
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
            try db.execute(sql: """
                INSERT INTO files(id,path_text,kind,failed,error_message,scanned_at) VALUES
                    (1,'/m/Camera/a.jpg','image',0,NULL,1),
                    (2,'/m/Camera/2020/b.jpg','image',0,NULL,2),
                    (3,'/m/Camera2/c.jpg','image',0,NULL,3),
                    (4,'/m/Camera/Synced/d.jpg','image',0,NULL,4),
                    (5,'/m/Camera/e.gif','image',1,'Could not decode image',5),
                    (6,'/m/Cameras/f.zip','other',0,NULL,6)
                """)
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        #expect(store.files().map(\.id) == [6, 4, 3, 2, 1])
        await store.setScope(LibraryScope(rootID: 1, roots: roots))
        #expect(store.files().map(\.id) == [2, 1])
        #expect(store.files(offset: 1, limit: 1).map(\.id) == [1])
        #expect(store.files(kindFilter: ReadStore.unreadableFilter).map(\.id) == [5])
        #expect(await store.catalogFilesAsync(ids: [3, 1, 4]).map(\.id) == [1])
        await store.setScope(LibraryScope(rootID: 3, roots: roots))
        #expect(store.files().map(\.id) == [4])
        await store.setScope(nil)
        #expect(store.files().count == 5)
    }
}
