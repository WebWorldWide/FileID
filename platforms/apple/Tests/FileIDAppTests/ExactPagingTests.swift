import Foundation
import FileIDShared
import GRDB
import Testing
@testable import FileID

@Suite("Exact duplicates paging and totals", .serialized)
struct ExactPagingTests {
    private func makeStore(pairs: Int) throws -> (ReadStore, URL) {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-paging-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
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
                    vlm_inherited_from INTEGER, auto_tags TEXT, content_hash BLOB)
                """)
            for i in 0..<pairs {
                var hash = Data(count: 32)
                hash[0] = UInt8(i / 256); hash[1] = UInt8(i % 256)
                for copy in 0..<2 {
                    try db.execute(sql: "INSERT INTO files(id,path_text,size_bytes,kind,extension,content_hash) VALUES (?,?,?,'image','jpg',?)",
                                   arguments: [i * 2 + copy + 1, "/gone/g\(i)-\(copy).jpg", 2_000 + i, hash])
                }
            }
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        return (store, root)
    }

    @Test("every group is reachable by paging: pages do not overlap and together cover the totals")
    func pagesCoverEveryGroup() throws {
        let (store, root) = try makeStore(pairs: 450)
        defer { store.close(); try? FileManager.default.removeItem(at: root) }
        var seen = Set<Int64>()
        var offset = 0
        var pages = 0
        while pages < 10 {
            let page = store.exactStoredGroups(offset: offset, limit: ExactDuplicateVerifier.groupCap)
            #expect(page.totalGroups == 450)
            if page.groups.isEmpty { break }
            for g in page.groups { #expect(seen.insert(g.id).inserted) }
            offset += page.groups.count
            pages += 1
        }
        #expect(seen.count == 450)
        #expect(pages == 3)
        let first = store.exactStoredGroups()
        let ninth = store.exactStoredGroups(offset: 400, limit: 200)
        #expect(first.groups.first?.files.first?.sizeBytes == 2_449)
        #expect(ninth.groups.count == 50)
    }

    @Test("totals follow the byte check: a group that fell apart leaves the count, the extra copies and the freed space")
    func totalsFollowVerification() {
        func file(_ id: Int64) -> FileRow {
            FileRow(id: id, pathText: "/x/\(id).jpg", sizeBytes: 3_000, createdAt: nil, modifiedAt: nil, scannedAt: Date(timeIntervalSince1970: 0), kind: "image", extension: "jpg", phash: nil,
                    aesthetic: nil, hasFaces: false, hasText: false, cameraModel: nil, locationLat: nil, locationLon: nil, failed: false, errorMessage: nil)
        }
        let kept = DuplicateGroup(id: 1, files: [file(1), file(2), file(3)], totalFileCount: 3, totalBytes: 9_000)
        let dropped = DuplicateGroup(id: 2, files: [file(4), file(5)], totalFileCount: 2, totalBytes: 6_000)
        let shrunk = DuplicateGroup(id: 1, files: [file(1), file(2)], totalFileCount: 2, totalBytes: 6_000)
        let base = ExactDuplicateVerifier.Totals(groups: 10, extra: 25, bytes: 90_000)
        let after = ExactDuplicateVerifier.adjustedTotals(base: base, stored: [kept, dropped], current: [shrunk])
        #expect(after == ExactDuplicateVerifier.Totals(groups: 9, extra: 23, bytes: 84_000))
        #expect(ExactDuplicateVerifier.adjustedTotals(base: base, stored: [kept, dropped], current: [kept, dropped]) == base)
    }
}
