// Find borderline person-cluster pairs by centroid L2 distance.
// O(N²) over a few hundred centroids runs in milliseconds; the
// suggestions feed the Suggested Merges sheet.
import Foundation
import GRDB
import Vision
import FileIDShared

public enum ClusterSuggestions {

    public static let borderlineMin: Float = 0.45
    public static let borderlineMax: Float = 0.70

    public struct Candidate: Sendable, Identifiable, Hashable {
        public let personA: Int64
        public let personB: Int64
        public let distance: Float
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
                    SELECT person_id, print_data
                    FROM face_prints
                    WHERE person_id IS NOT NULL
                      AND LENGTH(print_data) > 0
                    """)
                return r.map { PrintRow(personID: $0["person_id"] ?? 0,
                                         blob: $0["print_data"] ?? Data()) }
            }
        } catch {
            return []
        }
        guard !rows.isEmpty else { return [] }

        // Decode each archived VNFeaturePrintObservation → [Float].
        struct Decoded { let personID: Int64; let vec: [Float] }
        var decoded: [Decoded] = []
        decoded.reserveCapacity(rows.count)
        for r in rows {
            if let v = decodePrint(r.blob) {
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
            let n = Float(vecs.count)
            centroids.append((pid, sum.map { $0 / n }))
        }

        var pairs: [Candidate] = []
        for i in 0..<centroids.count {
            for j in (i+1)..<centroids.count {
                let d = l2(centroids[i].vec, centroids[j].vec)
                if d >= borderlineMin && d <= borderlineMax {
                    let lo = min(centroids[i].personID, centroids[j].personID)
                    let hi = max(centroids[i].personID, centroids[j].personID)
                    pairs.append(Candidate(personA: lo, personB: hi, distance: d))
                }
            }
        }
        return pairs.sorted { $0.distance < $1.distance }
    }

    // MARK: - Math

    private static func l2(_ a: [Float], _ b: [Float]) -> Float {
        guard a.count == b.count else { return .infinity }
        var sum: Float = 0
        for i in 0..<a.count {
            let d = a[i] - b[i]
            sum += d * d
        }
        return sum.squareRoot()
    }

    private static func decodePrint(_ data: Data) -> [Float]? {
        guard let obs = try? NSKeyedUnarchiver.unarchivedObject(
            ofClass: VNFeaturePrintObservation.self, from: data
        ) else { return nil }
        let count = obs.elementCount
        guard count >= 64, count <= 4096 else { return nil }
        let requiredBytes = count * MemoryLayout<Float>.size
        guard obs.data.count >= requiredBytes else { return nil }
        var out = [Float](repeating: 0, count: count)
        obs.data.withUnsafeBytes { ptr in
            let fp = ptr.bindMemory(to: Float.self)
            let n = min(count, fp.count)
            for i in 0..<n { out[i] = fp[i] }
        }
        return out
    }
}
