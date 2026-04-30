// IdentityClustering — production face-identity clustering.
//
// Two-pass density clustering + Pass 3 quality validation. Replaces
// Chinese Whispers, which collapsed into mega-clusters on real
// libraries: kNN's fixed-k neighbour list plus CW's plurality-vote
// reassignment chains "bridge faces" (people who look slightly like
// multiple identities) into one giant connected component.
//
// Convergent design across the production reference systems:
//   - Immich   — DBSCAN on ArcFace cosine, density-based
//   - FaceNet  — agglomerative average-link, with verification net
//   - InsightFace ref impl — DBSCAN/HDBSCAN, cosine 0.4-0.5
// All three rely on density + a hard cutoff that bridge faces can't
// breach, plus a way to keep ambiguous faces from merging two
// identities. Adapted to our scale + pure-Swift constraints:
//
//   Pass 1 — Identity cores (high precision, low recall)
//     Connected components on a kNN graph at cosine ≥ pass1Cosine.
//     Two faces in the same component are linked through a chain of
//     high-confidence same-person edges. Components of size ≥ 2 are
//     "cores"; size-1 nodes go to Pass 2 as outliers.
//
//   Pass 2 — Outlier assignment (high recall, with margin)
//     Each unassigned face finds its nearest core by cosine to that
//     core's centroid. Assigned iff cosine ≥ pass2Cosine AND
//     (cosine_1 - cosine_2 ≥ pass2Margin). The margin rule prevents
//     bridge faces from collapsing two distinct identities — the
//     exact failure mode CW exhibited. Faces that don't pass become
//     their own singleton cluster (one-photo people).
//
//   Pass 3 — Quality validation
//     Any cluster whose intra-cluster mean cosine to centroid is below
//     pass3MinMeanCosine OR whose variance exceeds pass3VarianceThreshold
//     is split via 2-means in cosine space, then re-validated. Up to
//     pass3MaxSplits splits per origin cluster. Hard correctness floor
//     against mega-cluster collapse: if Pass 1 / Pass 2 ever produce a
//     single 10K-face "person", validation breaks it apart.
//
// Complexity:
//   Pass 1: n calls to searcher; union-find with path-compression is
//           O(n · k · α(n)) total — linear-ish.
//   Pass 2: O(outliers · cores) cosine evals. At ≤ ~1000 cores trivial.
//           For libraries with very high core counts (rare in personal
//           photo libraries), this dominates wall time.
//   Pass 3: O(n · 2-means iterations · splits). Bounded by maxSplits.
import Foundation

public enum IdentityClustering {

    public struct Hyperparameters: Sendable {
        /// Minimum cosine similarity for a Pass 1 edge. Above this we
        /// believe two faces are the same person with high confidence.
        /// Default 0.55 — strict per ArcFace literature for clustering
        /// (vs 0.40 for verification at FAR=10⁻⁴, where errors don't
        /// compound across a chain).
        public let pass1Cosine: Float

        /// Minimum cosine to a core's centroid for Pass 2 assignment.
        /// Default 0.45 — looser than Pass 1 because we're comparing to
        /// a denoised centroid, not a single noisy face.
        public let pass2Cosine: Float

        /// Minimum gap between nearest and second-nearest core. The
        /// margin rule. If a face is ambiguous (cosine 0.50 to Adam
        /// AND 0.49 to Brother), neither gets it — it stays a singleton.
        public let pass2Margin: Float

        /// Pass 3 splits a cluster if its variance of cosines-to-centroid
        /// exceeds this. 0.05 is empirically tight: well-formed clusters
        /// from a real library have variance ~0.01-0.03.
        public let pass3VarianceThreshold: Float

        /// Pass 3 splits a cluster if its mean cosine to centroid drops
        /// below this. 0.50 catches mega-clusters where a chain of
        /// borderline edges has glued multiple identities together.
        public let pass3MinMeanCosine: Float

        /// Recursive split depth cap. With 3 splits a single origin
        /// cluster can become up to 8 sub-clusters — ample for any
        /// realistic mixed-identity case.
        public let pass3MaxSplits: Int

        /// kNN k for Pass 1. Lower than CW's 20 because we no longer
        /// need plurality voting; we just need enough true-edge
        /// coverage to connect each face to its same-person neighbors.
        public let kNN: Int

        public init(
            pass1Cosine: Float = 0.55,
            pass2Cosine: Float = 0.45,
            pass2Margin: Float = 0.05,
            pass3VarianceThreshold: Float = 0.05,
            pass3MinMeanCosine: Float = 0.50,
            pass3MaxSplits: Int = 3,
            kNN: Int = 10
        ) {
            self.pass1Cosine = pass1Cosine
            self.pass2Cosine = pass2Cosine
            self.pass2Margin = pass2Margin
            self.pass3VarianceThreshold = pass3VarianceThreshold
            self.pass3MinMeanCosine = pass3MinMeanCosine
            self.pass3MaxSplits = pass3MaxSplits
            self.kNN = kNN
        }
    }

    public struct Result: Sendable {
        /// `clusterIDs[i]` = dense cluster ID for embedding i (0..clusterCount-1).
        /// Always non-negative; every face gets a cluster (singleton if alone).
        public let clusterIDs: [Int]
        public let clusterCount: Int
        /// Components of size ≥ 2 from Pass 1 — pre-Pass-2-merging.
        public let coreCount: Int
        /// Outliers that were merged into an existing core in Pass 2.
        public let outliersAssigned: Int
        /// Outliers that became their own singleton clusters (passed
        /// neither the cosine floor nor the margin rule).
        public let outliersAsSingletons: Int
        /// Total Pass-3 splits applied across all clusters.
        public let splitsApplied: Int
        public let durationSeconds: Double
    }

    /// Run the full pipeline.
    /// `embeddings[i]` must be L2-normalized. `searcher(i)` returns the
    /// kNN of face i with cosine similarities; FaceClustering supplies
    /// an HNSW-backed implementation.
    public static func cluster(
        embeddings: [[Float]],
        searcher: (Int) -> [(neighbor: Int, similarity: Float)],
        params: Hyperparameters = Hyperparameters()
    ) -> Result {
        let started = Date()
        let n = embeddings.count
        guard n > 0 else {
            return Result(clusterIDs: [], clusterCount: 0, coreCount: 0,
                          outliersAssigned: 0, outliersAsSingletons: 0,
                          splitsApplied: 0, durationSeconds: 0)
        }
        let dim = embeddings.first?.count ?? 0
        guard dim > 0 else {
            return Result(clusterIDs: [Int](repeating: 0, count: n),
                          clusterCount: 0, coreCount: 0,
                          outliersAssigned: 0, outliersAsSingletons: 0,
                          splitsApplied: 0, durationSeconds: 0)
        }

        // ─── Pass 1: connected components above pass1Cosine ───────
        var uf = UnionFind(n: n)
        for i in 0..<n {
            for hit in searcher(i) {
                let j = hit.neighbor
                guard j != i, j >= 0, j < n else { continue }
                guard hit.similarity >= params.pass1Cosine else { continue }
                uf.union(i, j)
            }
        }
        var rootMembers: [Int: [Int]] = [:]
        for i in 0..<n { rootMembers[uf.find(i), default: []].append(i) }
        var cores: [[Int]] = []
        var outliers: [Int] = []
        for (_, members) in rootMembers {
            if members.count >= 2 { cores.append(members) }
            else { outliers.append(contentsOf: members) }
        }
        let pass1Cores = cores.count

        // ─── Pass 2: outlier assignment with margin ───────────────
        var coreCentroids: [[Float]] = cores.map {
            centroidNormalized(of: $0, embeddings: embeddings, dim: dim)
        }
        var outliersAssigned = 0
        var outliersAsSingletons = 0
        for outlier in outliers {
            let v = embeddings[outlier]
            var c1Idx = -1, c2Idx = -1
            var c1Sim: Float = -2, c2Sim: Float = -2
            for (idx, centroid) in coreCentroids.enumerated() {
                let s = dot(v, centroid)
                if s > c1Sim {
                    c2Sim = c1Sim; c2Idx = c1Idx
                    c1Sim = s; c1Idx = idx
                } else if s > c2Sim {
                    c2Sim = s; c2Idx = idx
                }
            }
            let passesFloor = c1Idx >= 0 && c1Sim >= params.pass2Cosine
            let passesMargin = c2Idx < 0 || (c1Sim - c2Sim) >= params.pass2Margin
            if passesFloor && passesMargin {
                cores[c1Idx].append(outlier)
                coreCentroids[c1Idx] = centroidNormalized(
                    of: cores[c1Idx], embeddings: embeddings, dim: dim
                )
                outliersAssigned += 1
            } else {
                cores.append([outlier])
                coreCentroids.append(v)
                outliersAsSingletons += 1
            }
        }

        // ─── Pass 3: quality validation + 2-means split ──────────
        var splitsApplied = 0
        var refined: [[Int]] = []
        refined.reserveCapacity(cores.count)
        for cluster in cores {
            let parts = validateAndSplit(
                cluster, embeddings: embeddings, dim: dim, params: params,
                splitsRemaining: params.pass3MaxSplits
            )
            if parts.count > 1 { splitsApplied += parts.count - 1 }
            refined.append(contentsOf: parts)
        }

        // ─── Materialize result ──────────────────────────────────
        var clusterIDs = [Int](repeating: 0, count: n)
        for (cid, members) in refined.enumerated() {
            for m in members { clusterIDs[m] = cid }
        }
        return Result(
            clusterIDs: clusterIDs,
            clusterCount: refined.count,
            coreCount: pass1Cores,
            outliersAssigned: outliersAssigned,
            outliersAsSingletons: outliersAsSingletons,
            splitsApplied: splitsApplied,
            durationSeconds: Date().timeIntervalSince(started)
        )
    }

    // MARK: - Internals

    /// L2-normalized mean of the indexed embeddings.
    private static func centroidNormalized(
        of indices: [Int], embeddings: [[Float]], dim: Int
    ) -> [Float] {
        var sum = [Float](repeating: 0, count: dim)
        for i in indices {
            let v = embeddings[i]
            for d in 0..<dim { sum[d] += v[d] }
        }
        var norm: Float = 0
        for d in 0..<dim { norm += sum[d] * sum[d] }
        let invN = Float(1) / max(.leastNonzeroMagnitude, norm.squareRoot())
        for d in 0..<dim { sum[d] *= invN }
        return sum
    }

    /// Cosine on pre-normalized vectors = dot product.
    @inline(__always)
    private static func dot(_ a: [Float], _ b: [Float]) -> Float {
        let n = min(a.count, b.count)
        var s: Float = 0
        for i in 0..<n { s += a[i] * b[i] }
        return s
    }

    /// Recursively split a cluster if it fails the variance / mean-cosine
    /// quality bar. Returns one or more sub-clusters.
    private static func validateAndSplit(
        _ cluster: [Int],
        embeddings: [[Float]], dim: Int,
        params: Hyperparameters,
        splitsRemaining: Int
    ) -> [[Int]] {
        guard cluster.count >= 2 else { return [cluster] }
        let centroid = centroidNormalized(of: cluster, embeddings: embeddings, dim: dim)
        var sims = [Float](); sims.reserveCapacity(cluster.count)
        var sumS: Float = 0
        for i in cluster {
            let s = dot(embeddings[i], centroid)
            sims.append(s); sumS += s
        }
        let mean = sumS / Float(cluster.count)
        var variance: Float = 0
        for s in sims { let d = s - mean; variance += d * d }
        variance /= Float(cluster.count)

        let meanOK = mean >= params.pass3MinMeanCosine
        let varOK = variance <= params.pass3VarianceThreshold
        if meanOK && varOK { return [cluster] }
        if splitsRemaining <= 0 { return [cluster] }

        // 2-means seeds: face farthest from centroid (lowest cosine), and
        // the face farthest from THAT seed.
        var seedAIdx = cluster[0]
        var seedASim = sims[0]
        for (k, s) in sims.enumerated() {
            if s < seedASim { seedASim = s; seedAIdx = cluster[k] }
        }
        let aVec = embeddings[seedAIdx]
        var seedBIdx = -1
        var seedBSim: Float = 2
        for i in cluster {
            guard i != seedAIdx else { continue }
            let s = dot(embeddings[i], aVec)
            if s < seedBSim { seedBSim = s; seedBIdx = i }
        }
        guard seedBIdx >= 0 else { return [cluster] }

        var groupA: [Int] = []
        var groupB: [Int] = []
        var centA = embeddings[seedAIdx]
        var centB = embeddings[seedBIdx]
        for _ in 0..<10 {
            groupA.removeAll(keepingCapacity: true)
            groupB.removeAll(keepingCapacity: true)
            for i in cluster {
                let v = embeddings[i]
                if dot(v, centA) >= dot(v, centB) {
                    groupA.append(i)
                } else {
                    groupB.append(i)
                }
            }
            if groupA.isEmpty || groupB.isEmpty { break }
            let newA = centroidNormalized(of: groupA, embeddings: embeddings, dim: dim)
            let newB = centroidNormalized(of: groupB, embeddings: embeddings, dim: dim)
            // Convergence: both centroids barely moved.
            if dot(newA, centA) > 0.999 && dot(newB, centB) > 0.999 {
                centA = newA; centB = newB
                break
            }
            centA = newA; centB = newB
        }
        if groupA.isEmpty || groupB.isEmpty { return [cluster] }
        let leftParts = validateAndSplit(
            groupA, embeddings: embeddings, dim: dim, params: params,
            splitsRemaining: splitsRemaining - 1
        )
        let rightParts = validateAndSplit(
            groupB, embeddings: embeddings, dim: dim, params: params,
            splitsRemaining: splitsRemaining - 1
        )
        return leftParts + rightParts
    }
}

// MARK: - Union-Find with path compression + union by rank

private struct UnionFind {
    var parent: [Int]
    var rank: [Int]
    init(n: Int) {
        parent = Array(0..<n)
        rank = [Int](repeating: 0, count: n)
    }
    mutating func find(_ x: Int) -> Int {
        var r = x
        while parent[r] != r { r = parent[r] }
        var cur = x
        while parent[cur] != r {
            let next = parent[cur]
            parent[cur] = r
            cur = next
        }
        return r
    }
    mutating func union(_ a: Int, _ b: Int) {
        let ra = find(a), rb = find(b)
        guard ra != rb else { return }
        if rank[ra] < rank[rb] {
            parent[ra] = rb
        } else if rank[ra] > rank[rb] {
            parent[rb] = ra
        } else {
            parent[rb] = ra
            rank[ra] += 1
        }
    }
}
