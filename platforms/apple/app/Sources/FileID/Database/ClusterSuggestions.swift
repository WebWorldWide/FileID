// Find borderline person-cluster pairs by centroid cosine similarity.
// Operates on ArcFace embeddings — same space the clusterer uses, so
// the borderline band stays consistent. O(N²) over a few hundred
// centroids runs in milliseconds; suggestions feed the Suggested
// Merges sheet on the People tab.
import Foundation
import GRDB
import FileIDShared

public enum ClusterSuggestions {

    /// Cosine similarity band considered "borderline" (might be the
    /// same person; might not). Below this we trust the clusterer's
    /// decision to keep them separate; above this we trust auto-merge.
    public static let borderlineMin: Float = 0.45
    public static let borderlineMax: Float = 0.65

    public struct Candidate: Sendable, Identifiable, Hashable {
        public let personA: Int64
        public let personB: Int64
        /// Cosine similarity (1 - cosine_distance). Higher = more similar.
        public let similarity: Float
        public var id: String { "\(personA):\(personB)" }
    }

    /// Run on a Task.detached. ~100 ms for 200 clusters.
    public static func findCandidates(dbPath: String) -> [Candidate] {
        struct PrintRow: Sendable { let personID: Int64; let blob: Data }
        let rows: [PrintRow]
        do {
            var config = Configuration()
            config.readonly = true
            let q = try DatabaseQueue(path: dbPath, configuration: config)
            rows = try q.read { db in
                let r = try Row.fetchAll(db, sql: """
                    SELECT person_id, arcface_embedding
                    FROM face_prints
                    WHERE person_id IS NOT NULL
                      AND LENGTH(arcface_embedding) > 0
                    """)
                return r.map { PrintRow(personID: $0["person_id"] ?? 0,
                                         blob: $0["arcface_embedding"] ?? Data()) }
            }
        } catch {
            return []
        }
        guard !rows.isEmpty else { return [] }

        struct Decoded { let personID: Int64; let vec: [Float] }
        var decoded: [Decoded] = []
        decoded.reserveCapacity(rows.count)
        for r in rows {
            let v = blobToFloats(r.blob)
            if !v.isEmpty {
                decoded.append(Decoded(personID: r.personID, vec: v))
            }
        }
        guard let dim = decoded.first?.vec.count, dim > 0 else { return [] }

        var byPerson: [Int64: [[Float]]] = [:]
        for d in decoded where d.vec.count == dim {
            byPerson[d.personID, default: []].append(d.vec)
        }
        guard byPerson.count >= 2 else { return [] }

        var centroids: [(personID: Int64, vec: [Float])] = []
        centroids.reserveCapacity(byPerson.count)
        for (pid, vecs) in byPerson {
            var sum = [Float](repeating: 0, count: dim)
            for v in vecs {
                for i in 0..<dim { sum[i] += v[i] }
            }
            // L2-normalize so cosine = dot product downstream.
            var norm: Float = 0
            for x in sum { norm += x * x }
            let invN = Float(1) / max(.leastNonzeroMagnitude, norm.squareRoot())
            for i in 0..<dim { sum[i] *= invN }
            centroids.append((pid, sum))
        }

        var pairs: [Candidate] = []
        for i in 0..<centroids.count {
            for j in (i+1)..<centroids.count {
                let s = dotProduct(centroids[i].vec, centroids[j].vec)
                if s >= borderlineMin && s <= borderlineMax {
                    let lo = min(centroids[i].personID, centroids[j].personID)
                    let hi = max(centroids[i].personID, centroids[j].personID)
                    pairs.append(Candidate(personA: lo, personB: hi, similarity: s))
                }
            }
        }
        // Most similar first — those are the most-likely-true merges.
        return pairs.sorted { $0.similarity > $1.similarity }
    }

    // MARK: - Math

    private static func dotProduct(_ a: [Float], _ b: [Float]) -> Float {
        let n = min(a.count, b.count)
        var s: Float = 0
        for i in 0..<n { s += a[i] * b[i] }
        return s
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
