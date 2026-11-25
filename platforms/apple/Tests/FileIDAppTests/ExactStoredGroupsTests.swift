import Foundation
import GRDB
import Testing
@testable import FileID

@Suite("Exact duplicates from stored hashes", .serialized)
struct ExactStoredGroupsTests {
    private func makeStore(_ rows: [(Int, String, Int64, String, UInt8)]) throws -> (ReadStore, URL) {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-stored-\(UUID().uuidString)", directoryHint: .isDirectory)
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
            for r in rows {
                try db.execute(sql: "INSERT INTO files(id,path_text,size_bytes,kind,extension,content_hash) VALUES (?,?,?,'image',?,?)",
                               arguments: [r.0, r.1, r.2, r.3, Data(repeating: r.4, count: 32)])
            }
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        return (store, root)
    }

    @Test("every group the stored hashes show is listed at once, biggest first, with totals for all of them and no file read")
    func listsWithoutReadingFiles() throws {
        let giant: Int64 = 3 * 1024 * 1024 * 1024
        var rows: [(Int, String, Int64, String, UInt8)] = [
            (1, "/gone/1.zip", giant, "zip", 1), (2, "/gone/2.zip", giant, "zip", 1),
            (3, "/gone/3.xmp", 800, "xmp", 2), (4, "/gone/4.xmp", 800, "xmp", 2),
            (5, "/gone/5.jpg", 5000, "jpg", 3), (6, "/gone/6.jpg", 5000, "jpg", 3), (7, "/gone/7.jpg", 5000, "jpg", 3),
        ]
        for i in 0..<ExactDuplicateVerifier.groupCap + 5 {
            rows.append((100 + 2 * i, "/gone/a\(i).png", 2000, "png", UInt8(10 + i % 200)))
            rows.append((101 + 2 * i, "/gone/b\(i).png", 2000, "png", UInt8(10 + i % 200)))
        }
        let (store, root) = try makeStore(rows)
        defer { store.close(); try? FileManager.default.removeItem(at: root) }
        let load = store.exactStoredGroups()
        #expect(load.groups.first?.files.count == 2)
        #expect(load.groups.first?.files.first?.sizeBytes == giant)
        #expect(load.groups.count == ExactDuplicateVerifier.groupCap)
        #expect(load.totalGroups == 2 + distinctSmall(ExactDuplicateVerifier.groupCap + 5))
        #expect(load.totalBytes >= giant)
        #expect(!load.groups.contains { $0.files.contains { $0.pathText.hasSuffix(".xmp") } })
    }

    private func distinctSmall(_ n: Int) -> Int { Set((0..<n).map { 10 + $0 % 200 }).count }

    @Test("a group whose copies differ is dropped by the byte check and a large group stays marked unconfirmed")
    func verifiesWhatIsShown() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appending(component: "fileid-verify-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: dir) }
        func make(_ name: String, _ bytes: [UInt8]) throws -> String {
            let url = dir.appending(component: name)
            try Data(bytes).write(to: url)
            return url.path
        }
        let same = [UInt8](repeating: 7, count: 300_000)
        var other = same; other[150_000] = 9
        let big = [UInt8](repeating: 5, count: 500_000)
        let rows: [(Int, String, Int64, String, UInt8)] = [
            (1, try make("a1.jpg", same), 300_000, "jpg", 1), (2, try make("a2.jpg", same), 300_000, "jpg", 1),
            (3, try make("b1.jpg", same), 300_000, "jpg", 2), (4, try make("b2.jpg", other), 300_000, "jpg", 2),
            (5, try make("c1.mov", big), 500_000, "mov", 3), (6, try make("c2.mov", big), 500_000, "mov", 3),
        ]
        let (store, root) = try makeStore(rows)
        defer { store.close(); try? FileManager.default.removeItem(at: root) }
        let load = store.exactStoredGroups()
        #expect(load.groups.count == 3)
        let result = await ExactDuplicateVerifier.verifyStored(load.groups, fullReadLimit: 400_000) { _ in }
        #expect(result.finished)
        let ids = result.groups.map { Set($0.files.map(\.id)) }
        #expect(ids.contains([1, 2]))
        #expect(ids.contains([5, 6]))
        #expect(!ids.contains { $0.contains(4) })
        let big56 = try #require(result.groups.first { $0.files.first?.id == 5 || $0.files.first?.id == 6 })
        #expect(result.unverified == [big56.id])
    }
}
