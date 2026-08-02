// Find borderline person-cluster pairs by centroid cosine similarity.
// Operates on SFace embeddings — same space the clusterer uses, so
// the borderline band stays consistent. One vector is retained per person,
// and the review surface is bounded independently of the library size.
import Foundation
import GRDB
import FileIDShared

public enum ClusterSuggestions {

    /// Cosine similarity band worth presenting for explicit review. Suggestions
    /// never merge automatically, including pairs above this display band.
    public static let borderlineMin: Float = 0.55
    public static let borderlineMax: Float = 0.97
    public static let resultLimit = 50

    public struct Candidate: Sendable, Identifiable, Hashable {
        public let personA: Int64
        public let personB: Int64
        /// Cosine similarity (1 - cosine_distance). Higher = more similar.
        public let similarity: Float
        public var id: String { "\(personA):\(personB)" }
    }

    /// Run on a Task.detached. ~100 ms for 200 clusters.
    public static func findCandidates(dbPath: String) -> [Candidate] {
        struct VectorRow: Sendable { let personID: Int64; let blob: Data }
        struct MembershipRow: Sendable { let personID: Int64; let fileID: Int64 }
        let vectors: [VectorRow]
        let memberships: [MembershipRow]
        do {
            var config = Configuration()
            config.readonly = true
            let q = try DatabaseQueue(path: dbPath, configuration: config)
            (vectors, memberships) = try q.read { db in
                let vectorRows = try Row.fetchAll(db, sql: """
                    SELECT p.id AS person_id,
                           COALESCE(p.centroid, rep.arcface_embedding) AS embedding
                    FROM persons p
                    LEFT JOIN face_prints rep
                      ON rep.id = p.representative_face_id
                     AND COALESCE(rep.excluded, 0) = 0
                    WHERE COALESCE(p.is_unknown, 0) = 0
                      AND LENGTH(COALESCE(p.centroid, rep.arcface_embedding)) > 0
                    ORDER BY p.id
                    """)
                let membershipRows = try Row.fetchAll(db, sql: """
                    SELECT person_id, file_id
                    FROM face_prints
                    WHERE person_id IS NOT NULL
                      AND COALESCE(excluded, 0) = 0
                    ORDER BY person_id, file_id
                    """)
                return (
                    vectorRows.map {
                        VectorRow(personID: $0["person_id"] ?? 0,
                                  blob: $0["embedding"] ?? Data())
                    },
                    membershipRows.map {
                        MembershipRow(personID: $0["person_id"] ?? 0,
                                      fileID: $0["file_id"] ?? 0)
                    }
                )
            }
        } catch {
            return []
        }
        guard !vectors.isEmpty else { return [] }

        var centroids: [(personID: Int64, vec: [Float])] = []
        centroids.reserveCapacity(vectors.count)
        var dimension: Int?
        for row in vectors {
            var vector = blobToFloats(row.blob)
            guard !vector.isEmpty else { continue }
            let dim = dimension ?? vector.count
            guard vector.count == dim else { continue }
            dimension = dim
            var norm: Float = 0
            for value in vector { norm += value * value }
            guard norm.isFinite, norm > .leastNonzeroMagnitude else { continue }
            let inverse = 1 / norm.squareRoot()
            for index in vector.indices { vector[index] *= inverse }
            centroids.append((row.personID, vector))
        }
        guard centroids.count >= 2 else { return [] }
        centroids.sort { $0.personID < $1.personID }

        var filesByPerson: [Int64: Set<Int64>] = [:]
        for row in memberships {
            filesByPerson[row.personID, default: []].insert(row.fileID)
        }

        var pairs: [Candidate] = []
        for i in 0..<centroids.count {
            for j in (i+1)..<centroids.count {
                guard let leftFiles = filesByPerson[centroids[i].personID],
                      let rightFiles = filesByPerson[centroids[j].personID],
                      leftFiles.isDisjoint(with: rightFiles) else {
                    continue
                }
                let s = dotProduct(centroids[i].vec, centroids[j].vec)
                if s >= borderlineMin && s < borderlineMax {
                    let lo = min(centroids[i].personID, centroids[j].personID)
                    let hi = max(centroids[i].personID, centroids[j].personID)
                    pairs.append(Candidate(personA: lo, personB: hi, similarity: s))
                    if pairs.count == resultLimit * 8 {
                        pairs.sort(by: candidatePrecedes)
                        pairs.removeLast(pairs.count - resultLimit)
                    }
                }
            }
        }
        // Most similar first — those are the most-likely-true merges.
        let sorted = pairs.sorted(by: candidatePrecedes)
        return Array(sorted.prefix(resultLimit))
    }

    // MARK: - Math

    private static func dotProduct(_ a: [Float], _ b: [Float]) -> Float {
        let n = min(a.count, b.count)
        var s: Float = 0
        for i in 0..<n { s += a[i] * b[i] }
        return s
    }

    private static func candidatePrecedes(_ lhs: Candidate, _ rhs: Candidate) -> Bool {
        if lhs.similarity != rhs.similarity { return lhs.similarity > rhs.similarity }
        if lhs.personA != rhs.personA { return lhs.personA < rhs.personA }
        return lhs.personB < rhs.personB
    }

    private static func blobToFloats(_ data: Data) -> [Float] {
        let count = data.count / MemoryLayout<Float>.stride
        guard count > 0 else { return [] }
        return data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> [Float] in
            let base = raw.baseAddress!.assumingMemoryBound(to: Float.self)
            return Array(UnsafeBufferPointer(start: base, count: count))
        }
    }
}
