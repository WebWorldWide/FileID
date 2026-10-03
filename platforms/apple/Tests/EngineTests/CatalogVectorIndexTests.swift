import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Persistent catalog vector index")
struct CatalogVectorIndexTests {
    private func blob(_ index: Int) -> Data {
        var values = [Float](repeating: 0, count: 512)
        values[index] = 1
        return values.withUnsafeBufferPointer { Data(buffer: $0) }
    }

    private func makeRoot() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDVector-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: false)
        return url
    }

    private func seed(_ database: FileIDEngine.Database, id: Int64, vector: Data, model: String = CLIPEmbeddingSpace.modelID) async throws {
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,kind,extension,size_bytes,modified_at,scanned_at) VALUES(?,?,?,'image','jpg',10,100,100) ON CONFLICT(id) DO NOTHING", arguments: [id, "fixture-\(id).jpg", id])
            try db.execute(sql: "INSERT OR REPLACE INTO clip_embeddings(file_id,embedding,model) VALUES(?,?,?)", arguments: [id, vector, model])
        }
    }

    @Test("cache restart and incremental replacement preserve entity mappings")
    func restartAndDelta() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let directory = root.appendingPathComponent("indexes")
        try await seed(database, id: 1, vector: blob(0))
        try await seed(database, id: 2, vector: blob(1))
        try await seed(database, id: 3, vector: blob(0), model: "legacy")
        let query = try #require(CLIPEmbeddingSpace.vector(from: blob(0)))
        let first = CatalogVectorIndex(pool: database.pool, directory: directory)
        let checkpoint = try await first.synchronize()
        let initial = try await first.matches(query, limit: 2)
        #expect(initial.map(\.fileID) == [1, 2])
        let restarted = CatalogVectorIndex(pool: database.pool, directory: directory)
        #expect(try await restarted.synchronize() == checkpoint)
        #expect(try await restarted.matches(query, limit: 2) == initial)
        try await database.pool.write { db in try db.execute(sql: "DELETE FROM clip_embeddings WHERE file_id=1") }
        try await seed(database, id: 2, vector: blob(0))
        try await seed(database, id: 4, vector: blob(2))
        let updated = try await restarted.synchronize()
        #expect(updated.generation > checkpoint.generation)
        let matches = try await restarted.matches(query, limit: 2)
        #expect(matches.map(\.fileID) == [2, 4])
        #expect(matches.first?.fingerprint == CatalogVectorIndex.digest(blob(0)))
        let latest = CatalogVectorIndex(pool: database.pool, directory: directory)
        #expect(try await latest.synchronize() == updated)
        #expect(try await latest.matches(query, limit: 2) == matches)
    }

    @Test("corrupt caches and missing change history rebuild from SQLite")
    func recovery() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let directory = root.appendingPathComponent("indexes")
        try await seed(database, id: 1, vector: blob(0))
        let index = CatalogVectorIndex(pool: database.pool, directory: directory)
        _ = try await index.synchronize()
        let graph = try #require(FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil).first { $0.pathExtension == "graph" })
        try Data("corrupt".utf8).write(to: graph)
        let recovered = CatalogVectorIndex(pool: database.pool, directory: directory)
        _ = try await recovered.synchronize()
        let query = try #require(CLIPEmbeddingSpace.vector(from: blob(0)))
        #expect(try await recovered.matches(query, limit: 1).first?.fileID == 1)
        try await seed(database, id: 2, vector: blob(0))
        try await database.pool.write { db in try db.execute(sql: "DELETE FROM catalog_vector_changes WHERE namespace='clip'") }
        _ = try await recovered.synchronize()
        #expect(Set(try await recovered.matches(query, limit: 2).map(\.fileID)) == [1, 2])
    }

    @Test("changed files invalidate derived vectors but preserve user evidence")
    func sourceChanges() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await seed(database, id: 1, vector: blob(0))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO tags(file_id,tag,source) VALUES(1,'Favorite','user')")
            try db.execute(sql: "UPDATE files SET size_bytes=11 WHERE id=1")
        }
        let counts = try await database.pool.read { db in
            (try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM clip_embeddings")!, try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM tags WHERE source='user'")!)
        }
        #expect(counts.0 == 0)
        #expect(counts.1 == 1)
    }

    @Test("capacity limits and protected cache paths fail without changing source rows")
    func limits() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await seed(database, id: 1, vector: blob(0))
        try await seed(database, id: 2, vector: blob(1))
        let limited = CatalogVectorIndex(pool: database.pool, directory: root.appendingPathComponent("limited"), maximumFiles: 1)
        await #expect(throws: CatalogVectorIndex.IndexError.self) { try await limited.synchronize() }
        let protected = CatalogVectorIndex(pool: database.pool, directory: URL(fileURLWithPath: "/Volumes/Adlon/FileID-vector-test"))
        await #expect(throws: (any Error).self) { try await protected.synchronize() }
        let count = try await database.pool.read { db in try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM clip_embeddings") }
        #expect(count == 2)
    }

    @Test("a divergent database with the same generation rebuilds the graph")
    func divergentCheckpoint() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await seed(database, id: 1, vector: blob(0))
        try await seed(database, id: 2, vector: blob(1))
        let directory = root.appendingPathComponent("indexes")
        let first = CatalogVectorIndex(pool: database.pool, directory: directory)
        let revision = try await first.synchronize()
        try await database.pool.write { db in
            try db.execute(sql: "UPDATE clip_embeddings SET embedding=? WHERE file_id=1", arguments: [blob(1)])
            try db.execute(sql: "UPDATE clip_embeddings SET embedding=? WHERE file_id=2", arguments: [blob(0)])
            try db.execute(sql: "UPDATE catalog_vector_state SET generation=?,nonce=lower(hex(randomblob(16))) WHERE namespace='clip'", arguments: [revision.generation])
        }
        let restarted = CatalogVectorIndex(pool: database.pool, directory: directory)
        let rebuilt = try await restarted.synchronize()
        #expect(rebuilt.generation == revision.generation)
        #expect(rebuilt.nonce != revision.nonce)
        let matches = try await restarted.matches(try #require(CLIPEmbeddingSpace.vector(from: blob(0))), limit: 2)
        #expect(matches.map(\.fileID) == [2, 1])
    }

    @Test("rolled back embedding writes do not advance the index checkpoint")
    func rollback() async throws {
        enum Rollback: Error { case requested }
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await seed(database, id: 1, vector: blob(0))
        let index = database.vectorIndex
        let before = try await index.synchronize()
        do {
            try await database.pool.write { db in
                try db.execute(sql: "UPDATE clip_embeddings SET embedding=? WHERE file_id=1", arguments: [blob(1)])
                throw Rollback.requested
            }
        } catch Rollback.requested {}
        let after = try await index.synchronize()
        #expect(after == before)
        let matches = try await index.matches(try #require(CLIPEmbeddingSpace.vector(from: blob(0))), limit: 1)
        #expect(matches.first?.cosine == 1)
    }

    @Test("hybrid retrieval keeps timestamp evidence and excludes failed files")
    func catalogRetrieval() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await seed(database, id: 1, vector: blob(0))
        try await seed(database, id: 2, vector: blob(0))
        try await seed(database, id: 3, vector: blob(1))
        let chapter = CatalogChapter(id: "hit", fileID: 3, startSeconds: 2, endSeconds: 3, title: "Baseball hit", summary: "", sourceRevision: "", modelVersion: "user", confidence: 1, userEdited: true, stale: false)
        let saved = await CatalogStore.handle(CatalogRequest(requestID: "save", action: "saveChapter", chapter: chapter), database: database)
        #expect(saved.status == "ok")
        let vector = try #require(CLIPEmbeddingSpace.vector(from: blob(0)))
        let request = CatalogRequest(requestID: "hybrid", action: "search", query: "baseball", searchMode: "hybrid", queryVector: vector, embeddingModel: CLIPEmbeddingSpace.modelID, limit: 10)
        let initial = await CatalogStore.handle(request, database: database)
        #expect(initial.status == "indexing")
        _ = try await database.vectorIndex.synchronize()
        try await database.pool.write { db in try db.execute(sql: "UPDATE files SET failed=1 WHERE id=1") }
        let result = await CatalogStore.handle(request, database: database)
        #expect(result.status == "ok")
        #expect(!result.hits.contains { $0.fileID == 1 })
        #expect(result.hits.contains { $0.fileID == 2 })
        #expect(result.hits.contains { $0.evidenceID == "hit" && $0.startSeconds == 2 })
        let similar = await CatalogStore.handle(CatalogRequest(requestID: "similar", action: "search", fileID: 2, searchMode: "semantic"), database: database)
        #expect(similar.status == "ok")
        #expect(!similar.hits.contains { $0.fileID == 2 || $0.fileID == 1 })
        try await database.pool.write { db in try db.execute(sql: "UPDATE files SET modified_at=101 WHERE id=3") }
        let changed = await CatalogStore.handle(request, database: database)
        #expect(changed.status == "ok")
        #expect(!changed.hits.contains { $0.fileID == 3 })
    }

    @Test("semantic requests reject incompatible vectors and malformed filters")
    func requestValidation() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let vector = try #require(CLIPEmbeddingSpace.vector(from: blob(0)))
        let requests = [
            CatalogRequest(requestID: "legacy", action: "search", searchMode: "semantic", queryVector: vector, embeddingModel: "mobileclip_s2"),
            CatalogRequest(requestID: "dimension", action: "search", searchMode: "semantic", queryVector: [1], embeddingModel: CLIPEmbeddingSpace.modelID),
            CatalogRequest(requestID: "norm", action: "search", searchMode: "semantic", queryVector: [Float](repeating: 0, count: 512), embeddingModel: CLIPEmbeddingSpace.modelID),
            CatalogRequest(requestID: "mode", action: "search", query: "test", searchMode: "invented"),
            CatalogRequest(requestID: "limit", action: "search", query: "test", limit: 101)
        ]
        for request in requests {
            let response = await CatalogStore.handle(request, database: database)
            #expect(response.status == "error")
            #expect(response.hits.isEmpty)
        }
    }

    @Test("failed vectors cannot crowd out eligible files")
    func eligibilityBeforeRanking() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        for id in 1...20 { try await seed(database, id: Int64(id), vector: blob(0)) }
        try await seed(database, id: 21, vector: blob(1))
        _ = try await database.vectorIndex.synchronize()
        try await database.pool.write { db in try db.execute(sql: "UPDATE files SET failed=1 WHERE id<=20") }
        let vector = try #require(CLIPEmbeddingSpace.vector(from: blob(0)))
        let request = CatalogRequest(requestID: "filtered", action: "search", searchMode: "semantic", queryVector: vector, embeddingModel: CLIPEmbeddingSpace.modelID, limit: 1)
        let response = await CatalogStore.handle(request, database: database)
        #expect(response.status == "ok")
        #expect(response.hits.map(\.fileID) == [21])
        try await database.pool.write { db in try db.execute(sql: "UPDATE files SET failed=0 WHERE id=1") }
        let recovered = await CatalogStore.handle(request, database: database)
        #expect(recovered.hits.map(\.fileID) == [1])
    }

    @Test("matching moments cannot crowd other files out of a Library result")
    func fileScope() {
        let chapters = (0..<20).map { CatalogHit(fileID: 1, path: "one.mov", kind: "chapter", text: "Hit", evidenceID: "chapter-\($0)", startSeconds: Double($0)) }
        let visual = [CatalogHit(fileID: 1, path: "one.mov", kind: "video", text: ""), CatalogHit(fileID: 2, path: "two.mov", kind: "video", text: "")]
        let files = CatalogSearch.merge(keyword: chapters, visual: visual, limit: 2, filesOnly: true)
        #expect(files.map(\.fileID) == [1, 2])
        #expect(files.first?.evidenceID == "chapter-0")
        let all = CatalogSearch.merge(keyword: chapters, visual: visual, limit: 30)
        #expect(all.filter { $0.fileID == 1 }.count == 21)
    }

    @Test("concurrent refreshed queries retain complete graph ownership")
    func concurrentQueries() async throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await seed(database, id: 1, vector: blob(0))
        let vector = try #require(CLIPEmbeddingSpace.vector(from: blob(0)))
        let results = try await withThrowingTaskGroup(of: [CatalogVectorIndex.Match].self) { group in
            for _ in 0..<40 {
                group.addTask { try await database.vectorIndex.refreshedMatches(vector, limit: 1) }
            }
            var collected: [[CatalogVectorIndex.Match]] = []
            for try await result in group { collected.append(result) }
            return collected
        }
        #expect(results.count == 40)
        #expect(results.allSatisfy { $0.map(\.fileID) == [1] && $0.first?.cosine == 1 })
    }
}
