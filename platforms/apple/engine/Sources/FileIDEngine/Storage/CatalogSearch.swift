import Foundation
import CryptoKit
import GRDB
import FileIDShared

enum CatalogSearch {
    enum RequestError: Error, LocalizedError {
        case invalid, needsVisualModel
        var errorDescription: String? {
            switch self {
            case .invalid: "That search request wasn't valid."
            case .needsVisualModel: "Searching by what is in photos needs the Search by description model, which isn't installed. Use Keyword search, or install the model in Settings."
            }
        }
    }

    static func handle(_ request: CatalogRequest, database: Database) async throws -> CatalogResponse {
        guard request.capturedFrom != nil || request.capturedTo != nil else { return try await run(request, database: database) }
        let from = request.capturedFrom ?? 0
        let to = request.capturedTo ?? Double.greatestFiniteMagnitude
        guard from.isFinite, from >= 0, to >= from, request.capturedTo.map({ $0.isFinite }) ?? true else { throw RequestError.invalid }
        let limit = request.limit ?? 100
        guard (1...100).contains(limit) else { throw RequestError.invalid }
        var wide = request
        wide.limit = 100
        var response = try await run(wide, database: database)
        let ids = String(decoding: try JSONEncoder().encode(Set(response.hits.map(\.fileID)).sorted()), as: UTF8.self)
        let inside = response.hits.isEmpty ? [] : try await database.pool.read { db in
            Set(try Int64.fetchAll(db, sql: "SELECT id FROM files WHERE id IN (SELECT value FROM json_each(?)) AND COALESCE(created_at,modified_at) BETWEEN ? AND ?", arguments: [ids, from, to]))
        }
        response.hits = Array(response.hits.filter { inside.contains($0.fileID) }.prefix(limit))
        // Ranking keeps the best hundred overall; files in the window beyond them
        // come from every file the words describe, newest first.
        let query = request.query?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if response.hits.count < limit, !query.isEmpty, (request.searchMode ?? "keyword") != "semantic" {
            let seen = Set(response.hits.map(\.fileID))
            let more = try await database.pool.read { db -> [CatalogHit] in
                guard let subject = try ChatFacts.subjectIDs(db, query) else { return [] }
                let wanted = String(decoding: try JSONEncoder().encode(subject), as: UTF8.self)
                return try Row.fetchAll(db, sql: """
                    SELECT id,path_text,kind,COALESCE(vlm_description,'') AS description FROM files
                    WHERE failed=0 AND id IN (SELECT value FROM json_each(?)) AND COALESCE(created_at,modified_at) BETWEEN ? AND ?
                    ORDER BY COALESCE(created_at,modified_at) DESC,id DESC LIMIT ?
                    """, arguments: [wanted, from, to, limit + seen.count]).compactMap { row in
                    let id: Int64 = row["id"]
                    return seen.contains(id) ? nil : CatalogHit(fileID: id, path: row["path_text"], kind: row["kind"], text: row["description"])
                }
            }
            response.hits += more.prefix(limit - response.hits.count)
        }
        return response
    }

    private static func run(_ request: CatalogRequest, database: Database) async throws -> CatalogResponse {
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
        guard request.queryVector != nil || request.fileID != nil else { throw RequestError.needsVisualModel }
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
