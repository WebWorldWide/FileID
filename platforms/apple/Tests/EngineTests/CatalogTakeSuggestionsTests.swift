import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Related-take suggestions")
struct CatalogTakeSuggestionsTests {
    @Test func groupsCurrentVisualEvidenceWithoutChangingEvents() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        let near = Float(0.95)
        let side = sqrt(1 - near * near)
        try await database.pool.write { db in
            for (id, time, hash) in [(1, 1_000.0, 1), (2, 1_060.0, 2), (3, 1_090.0, 3),
                                     (4, 1_120.0, 1), (5, 3_000.0, 5), (6, 1_080.0, 6), (7, 1_070.0, 7)] {
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension,content_hash) VALUES(?,?,?,?,?,?,?,'video','mov',?)",
                               arguments: [id, "/internal/\(id).mov", id, 100, time, time, 0, Data([UInt8(hash)])])
            }
            for (id, vector) in [(1, [Float(1), 0]), (2, [near, side]), (3, [Float(0), 1]),
                                 (4, [Float(1), 0]), (5, [Float(1), 0]), (6, [Float(1), 0]), (7, [Float(1), 0])] {
                try db.execute(sql: "INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?,?,?)",
                               arguments: [id, blob(vector), id == 7 ? "old-model" : CLIPEmbeddingSpace.modelID])
            }
            try db.execute(sql: "INSERT INTO catalog_assets(original_id,derived_id,role,recipe_json) VALUES(1,6,'export','{}')")
        }
        let request = CatalogRequest(requestID: "suggest", action: "suggestTakeGroups", fileIDs: [1, 2, 3, 4, 5, 6, 7])
        let response = await CatalogStore.handle(request, database: database)
        #expect(response.status == "ok")
        #expect(response.suggestedTakeGroups?.count == 1)
        #expect(response.suggestedTakeGroups?.first?.members.map(\.fileID) == [1, 2])
        #expect((response.suggestedTakeGroups?.first?.similarity ?? 0) >= 0.90)
        let events = await CatalogStore.handle(CatalogRequest(requestID: "events", action: "listEvents"), database: database)
        #expect(events.events?.isEmpty == true)
    }

    private func blob(_ leading: [Float]) -> Data {
        var vector = [Float](repeating: 0, count: CLIPEmbeddingSpace.dimension)
        for (index, value) in leading.enumerated() { vector[index] = value }
        var result = Data()
        for value in vector {
            var bits = value.bitPattern.littleEndian
            withUnsafeBytes(of: &bits) { result.append(contentsOf: $0) }
        }
        return result
    }
}
