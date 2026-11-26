import Foundation
import GRDB
import Testing
@testable import FileID

@Suite("Scenes shown beside the chapters", .serialized)
@MainActor
struct MomentScenesTests {
    private func seed(_ root: URL) throws -> URL {
        let url = root.appending(component: "fileid.sqlite")
        try DatabaseQueue(path: url.path).write { db in
            try db.execute(sql: """
                CREATE TABLE files (id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, kind TEXT NOT NULL)
                """)
            try db.execute(sql: """
                CREATE TABLE catalog_passages (
                    id TEXT PRIMARY KEY, file_id INTEGER NOT NULL, start_seconds REAL, end_seconds REAL,
                    text TEXT NOT NULL, stale INTEGER NOT NULL DEFAULT 0)
                """)
            try db.execute(sql: """
                CREATE TABLE catalog_coverage (
                    file_id INTEGER, start_seconds REAL, end_seconds REAL, status TEXT, model_version TEXT)
                """)
            try db.execute(sql: """
                INSERT INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,stale) VALUES
                    ('scene:1:r:v:0', 1, 0, 12, 'A pumpkin falls in slow motion', 0),
                    ('scene:1:r:v:9000', 1, 9, 12, 'An old scene', 1),
                    ('speech:1:r:v:500', 1, 0.5, 2, 'spoken words', 0)
                """)
        }
        return url
    }

    @Test("the stored scene text of a video is read next to its speech and sampled frames")
    func storedScenesAreRead() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-scenes-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let store = ReadStore(dbURL: try seed(root))
        store.openIfPossible()
        defer { store.close() }
        var tracks = store.timelineTracks(forFileID: 1)
        for _ in 0..<200 where tracks.scenes.isEmpty {
            try await Task.sleep(nanoseconds: 25_000_000)
            tracks = store.timelineTracks(forFileID: 1)
        }
        #expect(tracks.scenes.map(\.text) == ["A pumpkin falls in slow motion"])
        #expect(tracks.speech.map(\.text) == ["spoken words"])
    }

    @Test("an analyzed video with one scene says so instead of asking for Find moments again")
    func oneSceneIsExplained() {
        let message = CatalogWorkbench.noChaptersMessage(sceneCount: 1, analyzed: true)
        #expect(message.contains("one scene"))
        #expect(!message.contains("Press Find moments"))
        #expect(CatalogWorkbench.noChaptersMessage(sceneCount: 0, analyzed: false).contains("Press Find moments"))
        #expect(CatalogWorkbench.noChaptersMessage(sceneCount: 3, analyzed: true).contains("3 scenes"))
    }
}
