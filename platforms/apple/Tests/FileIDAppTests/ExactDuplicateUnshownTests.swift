import Foundation
import GRDB
import Testing
@testable import FileID

@Suite("Exact copies: sets past the group cap are counted", .serialized)
struct ExactDuplicateUnshownTests {
    @Test("sets of same-size files beyond the cap are counted with the space they could free")
    func unshownSetsAreCounted() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-dupcap-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appending(component: "fileid.sqlite")
        let extra = 10
        let classes = ExactDuplicateVerifier.groupCap + extra
        try await DatabaseQueue(path: url.path).write { db in
            try db.execute(sql: """
                CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, size_bytes INTEGER NOT NULL DEFAULT 1,
                    created_at REAL, modified_at REAL, scanned_at REAL NOT NULL DEFAULT 0,
                    kind TEXT NOT NULL, extension TEXT NOT NULL DEFAULT '', phash INTEGER, aesthetic REAL,
                    has_faces INTEGER, has_text INTEGER, camera_model TEXT, location_lat REAL, location_lon REAL,
                    failed INTEGER NOT NULL DEFAULT 0, error_message TEXT, vlm_description TEXT,
                    vlm_proposed_name TEXT, vlm_model TEXT, vlm_full_model TEXT, vlm_analyzed_at REAL,
                    vlm_inherited_from INTEGER, auto_tags TEXT, content_hash TEXT, path_search TEXT,
                    name_sort TEXT NOT NULL DEFAULT '')
                """)
            for index in 0..<classes {
                for copy in 0..<2 {
                    try db.execute(sql: "INSERT INTO files(path_text,size_bytes,kind) VALUES (?,?,'image')",
                                   arguments: ["/missing/\(index)-\(copy).jpg", 1_000 + index])
                }
            }
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        let snapshot = await store.exactDuplicateSnapshotAsync()
        #expect(snapshot.unshownClasses == extra)
        let hiddenBytes = (ExactDuplicateVerifier.groupCap..<classes).reduce(Int64(0)) { $0 + Int64(1_000 + $1) }
        #expect(snapshot.unshownPotentialBytes == hiddenBytes)
        #expect(ExactDuplicateVerifier.unshownNotice(classes: extra, bytes: hiddenBytes).contains("10 more sets"))
        #expect(ExactDuplicateVerifier.unshownNotice(classes: 0, bytes: 0).isEmpty)
    }
}
