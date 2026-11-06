import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Library roots")
struct LibraryRootsTests {
    @Test func countsFilesUnderEachRootAndNotLookAlikeSiblings() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO library_roots(path,display,kind,added_at,last_scan_at,last_status) VALUES('/Volumes/__absent__/Camera','Camera','external',1,5,'completed'),('/Volumes/__absent__/Camera2','Camera2','external',2,NULL,NULL)")
            for (id, path, size, failed) in [(1, "/Volumes/__absent__/Camera/a.jpg", 100, 0), (2, "/Volumes/__absent__/Camera/sub/b.jpg", 200, 0),
                                             (3, "/Volumes/__absent__/Camera/c.jpg", 400, 1), (4, "/Volumes/__absent__/Camera2/d.jpg", 800, 0),
                                             (5, "/Volumes/__absent__/Cameras/e.jpg", 1600, 0)] {
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed) VALUES(?,?,?,?,0,'image','jpg',?)", arguments: [id, path, id, size, failed])
            }
        }
        let roots = try await database.pool.read { try LibraryRoots.list($0) }
        #expect(roots.count == 2)
        #expect(roots[0].fileCount == 2 && roots[0].totalBytes == 300)
        #expect(roots[1].fileCount == 1 && roots[1].totalBytes == 800)
        #expect(roots[0].lastStatus == "completed" && roots[1].lastScanAt == nil)
        #expect(!roots[0].online)
    }

    @Test func aMissingOrEmptyFolderIsOfflineAndAPopulatedOneOnline() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        #expect(!ScanRootGuard.isReachable(dir.path))
        try Data("x".utf8).write(to: dir.appendingPathComponent("a.jpg"))
        #expect(ScanRootGuard.isReachable(dir.path))
        #expect(!ScanRootGuard.isReachable(dir.appendingPathComponent("missing").path))
    }

    @Test func boundsHandleTrailingSeparatorsAndWindowsPaths() {
        #expect(LibraryRoots.subtreeBounds("/Volumes/X/").low == "/Volumes/X/")
        #expect(LibraryRoots.subtreeBounds("/Volumes/X/").high == "/Volumes/X0")
        let windows = LibraryRoots.subtreeBounds(#"D:\Photos\"#)
        #expect(windows.exact == #"D:\Photos"# && windows.low == #"D:\Photos\"# && windows.high == #"D:\Photos]"#)
    }

    @Test func aScopeKeepsItsOwnFilesAndLeavesLookalikesAndDeeperLocationsOut() {
        let scope = LibraryRoots.Scope(root: "/m/Camera/", others: ["/m/Camera2", "/m/Camera/Synced/", "/elsewhere"])
        for inside in ["/m/Camera", "/m/Camera/a.jpg", "/m/Camera/2020/b.jpg", "/m/Camera/Synced2/c.jpg"] { #expect(scope.contains(inside), "\(inside)") }
        for outside in ["/m/Camera2/a.jpg", "/m/Cameras/a.jpg", "/m/Camera/Synced", "/m/Camera/Synced/d.jpg", "/m/Cam", "/elsewhere/e.jpg"] {
            #expect(!scope.contains(outside), "\(outside)")
        }
        #expect(LibraryRoots.Scope(root: "/", others: ["/m/Camera2"]).contains("/elsewhere/e.jpg"))
        #expect(!LibraryRoots.Scope(root: "/", others: ["/m/Camera2"]).contains("/m/Camera2/a.jpg"))
    }

    @Test func scopedSearchKeepsOneLocationAndUnknownLocationsAreErrors() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO library_roots(id,path,display,kind,added_at) VALUES(1,'/m/Camera','Camera','external',1),(2,'/m/Camera/Synced','Synced','external',2)")
            for (id, path) in [(1, "/m/Camera/a.jpg"), (2, "/m/Camera/Synced/b.jpg")] + (100..<230).map({ ($0, "/m/Cameras/\($0).jpg") }) {
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(?,?,?,10,0,'image','jpg','a dog')", arguments: [id, path, id])
            }
        }
        func ids(_ root: Int64?) async throws -> [Int64] {
            try await CatalogSearch.handle(CatalogRequest(requestID: "r", action: "search", query: "dog", rootID: root), database: database).hits.map(\.fileID)
        }
        #expect(try await ids(nil).count == 100)
        #expect(try await ids(1) == [1])
        #expect(try await ids(2) == [2])
        let unknown = await CatalogStore.handle(CatalogRequest(requestID: "r", action: "search", query: "dog", rootID: 9), database: database)
        #expect(unknown.status == "error" && unknown.hits.isEmpty)
    }

    @Test func rootsListRoundTripsThroughTheWireFormat() throws {
        let list = RootsList(requestID: "roots-1", roots: [LibraryRoot(id: 1, path: "/a", display: "a", kind: "internal", enabled: true, online: true, fileCount: 3, totalBytes: 9, lastScanAt: nil, lastStatus: nil)])
        let data = try JSONEncoder().encode(IPCEvent.Payload.rootsList(list))
        let text = String(decoding: data, as: UTF8.self)
        #expect(text.contains("\"rootsList\""))
        #expect(!text.contains("lastScanAt"), "absent optionals are not written as null")
        guard case .rootsList(let decoded) = try JSONDecoder().decode(IPCEvent.Payload.self, from: data) else {
            Issue.record("rootsList did not decode back to rootsList"); return
        }
        #expect(decoded == list)
    }
}
