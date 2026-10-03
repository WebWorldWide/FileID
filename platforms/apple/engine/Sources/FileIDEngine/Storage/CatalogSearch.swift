import Foundation
import CryptoKit
import GRDB
import FileIDShared

enum CatalogSearch {
    enum RequestError: Error { case invalid }

    static func handle(_ request: CatalogRequest, database: Database) async throws -> CatalogResponse {
        let mode = request.searchMode ?? "keyword"
        let limit = request.limit ?? 100
        let scope = request.resultScope ?? "all"
        guard ["all", "files"].contains(scope) else { throw RequestError.invalid }
        let query = request.query?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard ["keyword", "semantic", "hybrid"].contains(mode), (1...100).contains(limit), query.count <= 2000 else { throw RequestError.invalid }
        if mode == "keyword" {
            guard !query.isEmpty, request.queryVector == nil, request.embeddingModel == nil else { throw RequestError.invalid }
            let hits = try await database.pool.read { db in select(try CatalogStore.search(db, query: query), limit: limit, filesOnly: scope == "files") }
            return CatalogResponse(requestID: request.requestID, status: "ok", hits: hits)
        }
        guard mode != "hybrid" || !query.isEmpty else { throw RequestError.invalid }
        let vector: [Float]
        if let supplied = request.queryVector {
            guard request.fileID == nil, request.embeddingModel == CLIPEmbeddingSpace.modelID,
                  supplied.count == CLIPEmbeddingSpace.dimension, supplied.allSatisfy(\.isFinite),
                  abs(supplied.reduce(0.0) { $0 + Double($1) * Double($1) } - 1) <= 0.02 else { throw RequestError.invalid }
            vector = supplied
        } else if let seed = request.fileID, mode == "semantic", request.embeddingModel == nil {
            vector = try await database.pool.read { db in
                guard let blob = try Data.fetchOne(db, sql: "SELECT e.embedding FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE e.file_id=? AND e.model=? AND f.failed=0", arguments: [seed, CLIPEmbeddingSpace.modelID]),
                      let value = CLIPEmbeddingSpace.vector(from: blob) else { throw RequestError.invalid }
                return value
            }
        } else { throw RequestError.invalid }
        let index = database.vectorIndex
        guard await index.currentRevision() != nil else {
            if await index.failed { return CatalogResponse(requestID: request.requestID, status: "error", message: "The visual search index could not be prepared. Keyword search remains available.") }
            await index.prepare()
            let hits = mode == "hybrid" ? try await database.pool.read { db in select(try CatalogStore.search(db, query: query), limit: limit, filesOnly: scope == "files") } : []
            return CatalogResponse(requestID: request.requestID, status: "indexing", message: "Preparing the local visual search index.", hits: hits)
        }
        let candidates = try await index.refreshedMatches(vector, limit: min(1000, limit * 4 + 1))
        let candidateIDs = String(decoding: try JSONEncoder().encode(candidates.map(\.fileID)), as: UTF8.self)
        let hits = try await database.pool.read { db -> [CatalogHit] in
            let rows = try Row.fetchAll(db, sql: "SELECT f.id,f.path_text,f.kind,COALESCE(f.vlm_description,'') AS description,e.embedding FROM files f JOIN clip_embeddings e ON e.file_id=f.id WHERE f.failed=0 AND e.model=? AND f.id IN (SELECT value FROM json_each(?))", arguments: [CLIPEmbeddingSpace.modelID, candidateIDs])
            let byID = Dictionary(uniqueKeysWithValues: rows.map { (($0["id"] as Int64), $0) })
            let visual = candidates.compactMap { match -> CatalogHit? in
                guard match.fileID != request.fileID, let row = byID[match.fileID] else { return nil }
                let blob: Data = row["embedding"]
                let fingerprint = SHA256.hash(data: blob).map { String(format: "%02x", $0) }.joined()
                guard fingerprint == match.fingerprint else { return nil }
                return CatalogHit(fileID: match.fileID, path: row["path_text"], kind: row["kind"], text: row["description"])
            }
            if mode == "semantic" { return Array(visual.prefix(limit)) }
            return merge(keyword: try CatalogStore.search(db, query: query), visual: visual, limit: limit, filesOnly: scope == "files")
        }
        return CatalogResponse(requestID: request.requestID, status: "ok", message: mode == "hybrid" ? "Keyword, catalog evidence, and visual similarity results." : "Visual similarity results.", hits: hits)
    }

    static func merge(keyword: [CatalogHit], visual: [CatalogHit], limit: Int, filesOnly: Bool = false) -> [CatalogHit] {
        var hits: [String: CatalogHit] = [:]
        var scores: [String: Double] = [:]
        var order: [String: Int] = [:]
        for list in [keyword, visual] {
            for (rank, hit) in list.enumerated() {
                let key = hit.evidenceID.map { "evidence:" + $0 } ?? "file:\(hit.fileID)"
                if hits[key] == nil { order[key] = order.count; hits[key] = hit }
                scores[key, default: 0] += 1 / Double(60 + rank + 1)
            }
        }
        let ranked = scores.keys.sorted {
            let left = scores[$0]!, right = scores[$1]!
            return left == right ? order[$0]! < order[$1]! : left > right
        }.compactMap { hits[$0] }
        return select(ranked, limit: limit, filesOnly: filesOnly)
    }

    static func select(_ hits: [CatalogHit], limit: Int, filesOnly: Bool) -> [CatalogHit] {
        var seen = Set<Int64>()
        var selected: [CatalogHit] = []
        for hit in hits {
            if filesOnly, !seen.insert(hit.fileID).inserted { continue }
            selected.append(hit)
            if selected.count == limit { break }
        }
        return selected
    }

}
