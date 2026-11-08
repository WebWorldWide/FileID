import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Search strength (shared fixture)")
struct SearchStrengthTests {
    struct Fixture: Decodable {
        let cases: [Case]
        let summaries: [Summary]
        let similar: [Similar]
        let chat: [Chat]
    }
    struct Case: Decodable {
        let name: String
        let query: [String: Float]
        let photos: [Photo]
        let order: [Int64]
        let strong: [Int64]
    }
    struct Photo: Decodable {
        let id: Int64
        let vector: [String: Float]
        let margin: Float
    }
    struct Summary: Decodable { let good: Int; let weak: Int; let text: String }
    struct Similar: Decodable { let text: String; let similar: Bool }
    struct Chat: Decodable {
        let found: Int
        let weak: Int
        let similar: Bool
        let subject: String
        let qualifiers: String
        let text: String
    }

    static var root: URL {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        return root
    }

    static func fixture() throws -> Fixture {
        try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/search-strength.json")))
    }

    static func dense(_ sparse: [String: Float]) -> [Float] {
        var vector = [Float](repeating: 0, count: CLIPEmbeddingSpace.dimension)
        for (key, value) in sparse { vector[Int(key)!] = value }
        return vector
    }

    @Test func baselineMatchesTheSharedFile() throws {
        let data = try Data(contentsOf: Self.root.appendingPathComponent("shared/models/clip-search-baseline.json"))
        let object = try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        let values = try #require(object["baseline"] as? [Double])
        #expect(values.count == SearchStrength.baseline.count)
        for (shared, literal) in zip(values, SearchStrength.baseline) { #expect(abs(Float(shared) - literal) <= 1e-9) }
        #expect(object["model"] as? String == SearchStrength.model)
        #expect(abs((object["strongMargin"] as? Double ?? 0) - Double(SearchStrength.margin)) < 1e-9)
        #expect(SearchStrength.applies)
    }

    @Test func sharedCasesRankAndJudge() throws {
        for item in try Self.fixture().cases {
            let query = Self.dense(item.query)
            let scored = item.photos.map { photo -> (id: Int64, margin: Float) in
                let margin = SearchStrength.score(query, Self.dense(photo.vector)).margin
                #expect(abs(margin - photo.margin) < 1e-4, "\(item.name) photo \(photo.id)")
                return (photo.id, margin)
            }.sorted { $0.margin != $1.margin ? $0.margin > $1.margin : $0.id < $1.id }
            #expect(scored.map(\.id) == item.order, "\(item.name)")
            #expect(scored.filter { SearchStrength.isStrong($0.margin) }.map(\.id) == item.strong, "\(item.name)")
        }
    }

    @Test func sharedWording() throws {
        let fixture = try Self.fixture()
        for item in fixture.summaries { #expect(SearchStrength.summary(good: item.good, weak: item.weak) == item.text) }
        for item in fixture.similar { #expect(SearchStrength.wantsSimilar(item.text) == item.similar, "\(item.text)") }
        for item in fixture.chat {
            let text = ChatService.weakSummary(found: item.found, weak: item.weak, similar: item.similar, subject: item.subject, qualifiers: item.qualifiers)
            #expect(text == item.text)
        }
    }

    @Test func scoringFallsBackToTheClientVector() async {
        let supplied = Self.dense(["0": 1])
        let prompted = Self.dense(["1": 1])
        await SearchStrength.$encoder.withValue({ _ in nil }) {
            #expect(SearchStrength.scoringVector(query: "dog", supplied: supplied) == supplied)
        }
        await SearchStrength.$encoder.withValue({ text in text == "a photo of dog." ? prompted : nil }) {
            #expect(SearchStrength.scoringVector(query: " dog ", supplied: supplied) == prompted)
            #expect(SearchStrength.scoringVector(query: "  ", supplied: supplied) == supplied)
        }
    }

    @Test("text search puts good matches first, counts weaker ones, and keyword evidence makes a file good")
    func judgedSearch() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDStrength-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        let item = try #require(try Self.fixture().cases.first)
        try await database.pool.write { db in
            for photo in item.photos {
                let blob = Self.dense(photo.vector).withUnsafeBufferPointer { Data(buffer: $0) }
                let description = photo.id == 2 ? "a red dog" : ""
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,kind,extension,size_bytes,modified_at,scanned_at,vlm_description) VALUES(?,?,?,'image','jpg',10,100,100,?)",
                               arguments: [photo.id, "/lib/\(photo.id).jpg", photo.id, description])
                try db.execute(sql: "INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?,?,?)", arguments: [photo.id, blob, CLIPEmbeddingSpace.modelID])
            }
        }
        _ = try await database.vectorIndex.synchronize()
        let query = Self.dense(item.query)
        try await SearchStrength.$encoder.withValue({ _ in nil }) {
            let semantic = CatalogRequest(requestID: "s", action: "search", query: "zebra", searchMode: "semantic", queryVector: query, embeddingModel: CLIPEmbeddingSpace.modelID, limit: 10)
            let found = try await CatalogSearch.judged(semantic, database: database)
            #expect(found.response.hits.map(\.fileID) == item.order)
            #expect(found.weak == Set(item.order).subtracting(item.strong))
            #expect(found.response.message == "3 good matches, 1 weaker.")
            let hybrid = CatalogRequest(requestID: "h", action: "search", query: "dog", searchMode: "hybrid", queryVector: query, embeddingModel: CLIPEmbeddingSpace.modelID, limit: 10, resultScope: "files")
            let mixed = try await CatalogSearch.judged(hybrid, database: database)
            #expect(mixed.weak?.isEmpty == true)
            #expect(mixed.response.message == "4 good matches.")
            let seeded = try await CatalogSearch.judged(CatalogRequest(requestID: "f", action: "search", fileID: 1, searchMode: "semantic"), database: database)
            #expect(seeded.weak == nil)
            #expect(seeded.response.message == "Visual similarity results.")
        }
    }
}
