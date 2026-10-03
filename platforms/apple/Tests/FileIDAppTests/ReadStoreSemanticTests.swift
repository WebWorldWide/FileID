import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileID

@Suite("Semantic search model compatibility", .serialized)
struct ReadStoreSemanticTests {
    private func blob(_ index: Int = 0) -> Data {
        var values = [Float](repeating: 0, count: 512)
        values[index] = 1
        return values.withUnsafeBufferPointer { Data(buffer: $0) }
    }

    @Test("search rejects legacy, failed, malformed and nonfinite candidates")
    func matchingSpace() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDSemantic-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appendingPathComponent("catalog.sqlite")
        let database = try DatabaseQueue(path: url.path)
        try database.write { db in
            try db.execute(sql: """
                CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path_text TEXT NOT NULL DEFAULT 'photo.jpg', size_bytes INTEGER NOT NULL DEFAULT 4,
                    created_at REAL, modified_at REAL, scanned_at REAL NOT NULL DEFAULT 0,
                    kind TEXT NOT NULL DEFAULT 'image', extension TEXT NOT NULL DEFAULT 'jpg', phash BLOB, aesthetic REAL,
                    has_faces INTEGER DEFAULT 0, has_text INTEGER DEFAULT 0, camera_model TEXT, location_lat REAL, location_lon REAL,
                    failed INTEGER DEFAULT 0, error_message TEXT, vlm_description TEXT, vlm_proposed_name TEXT,
                    vlm_model TEXT, vlm_full_model TEXT, vlm_analyzed_at REAL, auto_tags TEXT);
                CREATE TABLE clip_embeddings (file_id INTEGER PRIMARY KEY, embedding BLOB NOT NULL, model TEXT NOT NULL);
                """)
            for id in 1...5 {
                try db.execute(sql: "INSERT INTO files(id,failed) VALUES(?,?)", arguments: [id, id == 3 ? 1 : 0])
                var vector = blob()
                if id == 4 { vector = Data([1, 2, 3, 4]) }
                if id == 5 {
                    var values = [Float](repeating: 0, count: 512)
                    values[0] = .nan
                    vector = values.withUnsafeBufferPointer { Data(buffer: $0) }
                }
                try db.execute(sql: "INSERT INTO clip_embeddings VALUES(?,?,?)", arguments: [id, vector, id == 2 ? "mobileclip_s2" : CLIPEmbeddingSpace.modelID])
            }
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        let query = try #require(CLIPEmbeddingSpace.vector(from: blob()))
        #expect(store.rankByCosine(against: query).map(\.id) == [1])
        #expect(store.similarFiles(toFileID: 2).isEmpty)
        #expect(store.similarFiles(toFileID: 3).isEmpty)
        #expect(store.similarFiles(toFileID: 1).isEmpty)
        #expect(store.rankByCosine(against: [1]).isEmpty)
        #expect(store.rankByCosine(against: [Float](repeating: .nan, count: 512)).isEmpty)
    }
}
