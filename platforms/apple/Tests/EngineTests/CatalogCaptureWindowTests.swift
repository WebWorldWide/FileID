import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Catalog capture-time window")
struct CatalogCaptureWindowTests {
    private func seeded() async throws -> (FileIDEngine.Database, URL) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            for (id, created, modified) in [(1, 1_000.0, 9_000.0), (2, 2_000.0, 9_000.0), (3, Double?.none, 3_000.0)] as [(Int, Double?, Double)] {
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension,vlm_description) VALUES(?,?,?,10,?,?,0,'image','jpg','sunny beach day')", arguments: [id, "/lib/\(id).jpg", id, created, modified])
            }
        }
        return (database, directory)
    }

    private func ids(_ from: Double?, _ to: Double?, limit: Int? = nil, _ database: FileIDEngine.Database) async throws -> [Int64] {
        let request = CatalogRequest(requestID: "r", action: "search", query: "beach", limit: limit, capturedFrom: from, capturedTo: to)
        return try await CatalogSearch.handle(request, database: database).hits.map(\.fileID).sorted()
    }

    @Test func windowKeepsFilesCapturedInsideIt() async throws {
        let (database, directory) = try await seeded()
        defer { try? FileManager.default.removeItem(at: directory) }
        #expect(try await ids(nil, nil, database) == [1, 2, 3])
        #expect(try await ids(1_500, 2_500, database) == [2])
        #expect(try await ids(nil, 1_500, database) == [1])
        #expect(try await ids(2_500, nil, database) == [3])
    }

    @Test func windowRespectsTheRequestedLimitAndRejectsBackwardsRanges() async throws {
        let (database, directory) = try await seeded()
        defer { try? FileManager.default.removeItem(at: directory) }
        #expect(try await ids(0, 10_000, limit: 2, database).count == 2)
        await #expect(throws: CatalogSearch.RequestError.self) { _ = try await ids(5_000, 1_000, database) }
    }
}

@Suite("Catalog search errors")
struct CatalogSearchErrorTests {
    @Test func hybridWithoutAVectorExplainsTheMissingModel() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        let request = CatalogRequest(requestID: "r", action: "search", query: "beach", searchMode: "hybrid")
        do {
            _ = try await CatalogSearch.handle(request, database: database)
            Issue.record("expected an error")
        } catch {
            #expect(error.localizedDescription.contains("isn't installed"))
            #expect(!error.localizedDescription.contains("RequestError"))
        }
    }
}
