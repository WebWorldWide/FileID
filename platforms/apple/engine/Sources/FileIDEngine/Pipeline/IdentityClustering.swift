// Two-pass density clustering + quality validation for face identity.
// Replaces Chinese Whispers, which collapsed bridge faces into
// mega-clusters on real libraries.
//
//   Pass 1 — connected components on a kNN graph at cosine ≥ pass1Cosine.
//            Components of size ≥ 2 become identity "cores".
//   Pass 2 — each unassigned face joins its nearest core iff
//            cosine ≥ pass2Cosine AND (top1 − top2 ≥ pass2Margin).
//            The margin rule blocks the bridge-face merges that
//            sank Chinese Whispers.
//   Pass 3 — any cluster with low mean intra-cosine or high variance
//            is split via 2-means in cosine space and re-validated;
//            hard floor against mega-cluster collapse.
//
// Convergent with Immich (DBSCAN), FaceNet (agglomerative + verification
// net), and InsightFace reference (DBSCAN/HDBSCAN, cosine 0.4–0.5).
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

        // Defaults calibrated on-hardware for SFace (128-d) — the commercial-clean
        // embedder that replaced ArcFace (512-d). SFace's cosine distribution
        // differs: a known single identity (studio portraits) clusters at mean
        // cosine-to-centroid ~0.93, while chained mega-blobs sit ~0.50. Pass 1's
        // single-linkage connected-components chains different people through
        // bridge faces unless its threshold sits well above the verification
        // boundary (OpenCV SFace EER ≈ 0.363), so pass1 is tight (0.66) and the
        // Pass-3 split floor sits in the genuine/chained gap (0.60). Mirrors the
        // Windows `identity_clustering.rs` calibration (largest cluster on a
        // 1475-face library: 90% → 7%). PROVISIONAL — fine-tune on a labeled set.
        public init(
            pass1Cosine: Float = 0.66,
            pass2Cosine: Float = 0.54,
            pass2Margin: Float = 0.10,
            pass3VarianceThreshold: Float = 0.04,
            pass3MinMeanCosine: Float = 0.60,
            pass3MaxSplits: Int = 7,
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
        /// True iff the pass aborted early on a cancellation/shutdown signal.
        /// The caller must discard the (partial) result and skip persisting.
        public let cancelled: Bool
    }

    /// Run the full pipeline.
    /// `embeddings[i]` must be L2-normalized. `searcher(i)` returns the
    /// kNN of face i with cosine similarities; FaceClustering supplies
    /// an HNSW-backed implementation. `shouldCancel` is polled inside the
    /// hot loops so a mid-flight pass can abort cleanly at a safe boundary
    /// (engine shutdown) — when it returns true the pass stops and reports
    /// `cancelled = true` so the caller skips the persist transaction.
    public static func cluster(
        embeddings: [[Float]],
        searcher: (Int) -> [(neighbor: Int, similarity: Float)],
        params: Hyperparameters = Hyperparameters(),
        shouldCancel: () -> Bool = { false }
    ) -> Result {
        let started = Date()
        let n = embeddings.count
        guard n > 0 else {
            return Result(clusterIDs: [], clusterCount: 0, coreCount: 0,
                          outliersAssigned: 0, outliersAsSingletons: 0,
                          splitsApplied: 0, durationSeconds: 0, cancelled: false)
        }
        let dim = embeddings.first?.count ?? 0
        guard dim > 0 else {
            return Result(clusterIDs: [Int](repeating: 0, count: n),
                          clusterCount: 0, coreCount: 0,
                          outliersAssigned: 0, outliersAsSingletons: 0,
                          splitsApplied: 0, durationSeconds: 0, cancelled: false)
        }

        func cancelledResult() -> Result {
            Result(clusterIDs: [Int](repeating: 0, count: n),
                   clusterCount: 0, coreCount: 0,
                   outliersAssigned: 0, outliersAsSingletons: 0,
                   splitsApplied: 0,
                   durationSeconds: Date().timeIntervalSince(started),
                   cancelled: true)
        }

        // ─── Pass 1: connected components above pass1Cosine ───────
        var uf = UnionFind(n: n)
        for i in 0..<n {
            if shouldCancel() { return cancelledResult() }
            for hit in searcher(i) {
                let j = hit.neighbor
                guard j != i, j >= 0, j < n else { continue }
                guard hit.similarity >= params.pass1Cosine else { continue }
                uf.union(i, j)
            }
        }
        var rootMembers: [Int: [Int]] = [:]
        for i in 0..<n { rootMembers[uf.find(i), default: []].append(i) }
        // Iterate in sorted-root order so cluster-ID assignment is deterministic
        // across runs — otherwise HashMap iteration order leaks into People-tab
        // cluster numbers and a re-scan of the same library renumbers everyone.
        // (mirrors the Windows engine's sort_by_key(root); audit F-C3-007)
        let sortedGroups = rootMembers.sorted { $0.key < $1.key }
        var cores: [[Int]] = []
        var outliers: [Int] = []
        for (_, members) in sortedGroups {
            if members.count >= 2 { cores.append(members) }
            else { outliers.append(contentsOf: members) }
        }
        let pass1Cores = cores.count

        // ─── Pass 2: outlier assignment with margin ───────────────
        // Maintain a parallel unnormalized running sum per core alongside the
        // normalized centroid. Recomputing the centroid over the full membership
        // on every outlier add is O(S) per add → O(S²) over a pass; folding the
        // outlier into the running sum is O(dim) per add. Mathematically
        // identical (only floating-point reassociation differs). (audit F-C3-008)
        var coreSums: [[Float]] = cores.map {
            centroidSum(of: $0, embeddings: embeddings, dim: dim)
        }
        var coreCentroids: [[Float]] = coreSums.map { normalizeSum($0, dim: dim) }
        var outliersAssigned = 0
        var outliersAsSingletons = 0
        for outlier in outliers {
            if shouldCancel() { return cancelledResult() }
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
                let m = min(dim, v.count)
                for d in 0..<m { coreSums[c1Idx][d] += v[d] }
                coreCentroids[c1Idx] = normalizeSum(coreSums[c1Idx], dim: dim)
                outliersAssigned += 1
            } else {
                cores.append([outlier])
                // A singleton's unnormalized sum is the embedding itself; its
                // normalized centroid is the (already L2-normalized) embedding.
                coreSums.append(v)
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
            durationSeconds: Date().timeIntervalSince(started),
            cancelled: false
        )
    }

    // MARK: - Internals

    /// L2-normalized mean of the indexed embeddings.
    private static func centroidNormalized(
        of indices: [Int], embeddings: [[Float]], dim: Int
    ) -> [Float] {
        normalizeSum(centroidSum(of: indices, embeddings: embeddings, dim: dim), dim: dim)
    }

    /// Unnormalized component-wise sum of the indexed embeddings. Pass 2 keeps
    /// this alongside the normalized centroid so an outlier add is O(dim), not
    /// O(S). The `.min` is a release-safe backstop against a stray short vector.
    private static func centroidSum(
        of indices: [Int], embeddings: [[Float]], dim: Int
    ) -> [Float] {
        var sum = [Float](repeating: 0, count: dim)
        for i in indices {
            let v = embeddings[i]
            let m = min(dim, v.count)
            for d in 0..<m { sum[d] += v[d] }
        }
        return sum
    }

    /// L2-normalize a running sum vector into a unit centroid.
    private static func normalizeSum(_ sum: [Float], dim: Int) -> [Float] {
        var norm: Float = 0
        for d in 0..<dim { norm += sum[d] * sum[d] }
        let invN = Float(1) / max(.leastNonzeroMagnitude, norm.squareRoot())
        var out = [Float](repeating: 0, count: dim)
        for d in 0..<dim { out[d] = sum[d] * invN }
        return out
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
