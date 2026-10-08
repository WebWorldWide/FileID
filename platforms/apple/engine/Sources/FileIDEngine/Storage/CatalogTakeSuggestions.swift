import Foundation
import GRDB
import FileIDShared

enum CatalogTakeSuggestions {
    private static let maximumGap: Double = 20 * 60
    private static let minimumSimilarity: Float = 0.90
    private static let maximumActiveGroups = 32

    private struct Group {
        let anchorTime: Double
        let anchor: [Float]
        var latestTime: Double
        var members: [CatalogTakeGroupMember]
        var lowestSimilarity: Float
    }

    static func discover(_ db: GRDB.Database, fileIDs: [Int64], limit: Int) throws -> [CatalogTakeGroupSuggestion] {
        let ids = Array(Set(fileIDs)).sorted()
        guard (2...10_000).contains(ids.count), ids.allSatisfy({ $0 > 0 }), (1...100).contains(limit) else {
            throw CatalogStore.InvalidRequest()
        }
        let scope = String(decoding: try JSONEncoder().encode(ids), as: UTF8.self)
        let rows = try Row.fetchCursor(db, sql: """
            SELECT f.id, f.path_text, f.created_at, f.content_hash, e.embedding
            FROM files f
            JOIN clip_embeddings e ON e.file_id=f.id AND e.model=?
            WHERE f.id IN (SELECT value FROM json_each(?))
              AND f.kind IN ('image','video') AND f.failed=0 AND f.created_at IS NOT NULL
              AND NOT EXISTS (SELECT 1 FROM catalog_assets a WHERE a.derived_id=f.id)
              AND NOT EXISTS (SELECT 1 FROM catalog_event_files ef WHERE ef.file_id=f.id)
            ORDER BY f.created_at, f.id
            """, arguments: [CLIPEmbeddingSpace.modelID, scope])

        var active: [Group] = []
        var finished: [Group] = []
        var seenHashes: Set<Data> = []
        while let row = try rows.next() {
            let time: Double = row["created_at"]
            let blob: Data = row["embedding"]
            guard time.isFinite, let vector = CLIPEmbeddingSpace.vector(from: blob) else { continue }
            let member = CatalogTakeGroupMember(fileID: row["id"], path: row["path_text"])
            let hash: Data? = row["content_hash"]
            if let hash, !seenHashes.insert(hash).inserted { continue }
            while let first = active.first, time - first.anchorTime > maximumGap {
                finished.append(active.removeFirst())
            }

            var bestIndex: Int?
            var bestSimilarity = minimumSimilarity
            for index in active.indices where active[index].members.count < 100 {
                let similarity = dot(vector, active[index].anchor)
                if similarity >= bestSimilarity {
                    bestIndex = index
                    bestSimilarity = similarity
                }
            }
            if let index = bestIndex {
                active[index].members.append(member)
                active[index].latestTime = time
                active[index].lowestSimilarity = min(active[index].lowestSimilarity, bestSimilarity)
            } else {
                if active.count >= maximumActiveGroups { finished.append(active.removeFirst()) }
                active.append(Group(anchorTime: time, anchor: vector, latestTime: time,
                                    members: [member],
                                    lowestSimilarity: 1))
            }
        }
        finished.append(contentsOf: active)
        return finished.filter { $0.members.count >= 2 }
            .sorted { $0.latestTime == $1.latestTime ? $0.members[0].fileID < $1.members[0].fileID : $0.latestTime > $1.latestTime }
            .prefix(limit)
            .map { group in
                CatalogTakeGroupSuggestion(members: group.members,
                                           similarity: Double(group.lowestSimilarity),
                                           reason: "Similar visual content and file dates. Review the files and desired outcome before saving.")
            }
    }

    private static func dot(_ left: [Float], _ right: [Float]) -> Float {
        zip(left, right).reduce(0) { $0 + $1.0 * $1.1 }
    }
}
