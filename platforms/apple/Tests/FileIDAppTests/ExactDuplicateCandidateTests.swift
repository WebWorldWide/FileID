import Foundation
import GRDB
import Testing
@testable import FileID

@Suite("Exact duplicate candidates", .serialized)
struct ExactDuplicateCandidateTests {
    private func store(_ rows: [(Int, String, Int64, String, String)]) throws -> (ReadStore, URL) {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-exact-\(UUID().uuidString)", directoryHint: .isDirectory)
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
                    vlm_inherited_from INTEGER, auto_tags TEXT, content_hash TEXT)
                """)
            for r in rows {
                try db.execute(sql: "INSERT INTO files(id,path_text,size_bytes,kind,extension,content_hash) VALUES (?,?,?,'image',?,?)",
                               arguments: [r.0, r.1, r.2, r.3, r.4])
            }
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        return (store, root)
    }

    @Test("tiny sidecar files are never offered as copies")
    func sidecarsAreNotCandidates() throws {
        let (store, root) = try store([
            (1, "/a/1.xmp", 800, "xmp", "h1"), (2, "/a/2.xmp", 800, "xmp", "h1"),
            (3, "/a/3.dat", 800, "dat", "h2"), (4, "/a/4.dat", 800, "dat", "h2"),
            (5, "/a/5.jpg", 5000, "jpg", "h3"), (6, "/a/6.jpg", 5000, "jpg", "h3"),
            (7, "/a/7.json", 9000, "json", "h4"), (8, "/a/8.JSON", 9000, "JSON", "h4"),
        ])
        defer { store.close(); try? FileManager.default.removeItem(at: root) }
        let load = store.exactDuplicateCandidates()
        #expect(Set(load.files.map(\.id)) == [5, 6])
    }

    @Test("the biggest reclaimable group loads first and a file too large to read is disclosed")
    func rankedByReclaimableBytes() throws {
        let big = ExactDuplicateVerifier.maxFileBytes + 1
        let (store, root) = try store([
            (1, "/a/1.jpg", 2000, "jpg", "s"), (2, "/a/2.jpg", 2000, "jpg", "s"), (3, "/a/3.jpg", 2000, "jpg", "s"),
            (4, "/a/4.mov", 900_000, "mov", "m"), (5, "/a/5.mov", 900_000, "mov", "m"),
            (6, "/a/6.zip", big, "zip", "z"), (7, "/a/7.zip", big, "zip", "z"),
        ])
        defer { store.close(); try? FileManager.default.removeItem(at: root) }
        let load = store.exactDuplicateCandidates()
        #expect(load.files.map(\.id).prefix(2).sorted() == [4, 5])
        #expect(!load.files.contains { $0.sizeBytes == big })
        #expect(load.partial)
    }

    @Test("an empty list is not called final while the check runs")
    func emptyListWaitsForTheCheck() {
        #expect(CleanupView.exactEmptyPhase(checking: true, partial: false) == .checking)
        #expect(CleanupView.exactEmptyPhase(checking: false, partial: false) == .none)
        #expect(CleanupView.exactEmptyPhase(checking: false, partial: true) == .partial)
    }
}
