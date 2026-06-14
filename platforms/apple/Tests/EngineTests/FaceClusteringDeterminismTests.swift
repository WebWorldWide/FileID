// Determinism + lifecycle invariants for the face-clustering algorithm layer
// (pure, no DB / model): HNSW fixed-seed reproducibility (F-C3-006), sorted-root
// cluster-ID stability (F-C3-007), the O(dim) running-sum Pass-2 centroid
// (F-C3-008), and cooperative cancellation (F-C3-042).
import Testing
import Foundation
@testable import FileIDEngine

private func l2norm(_ v: [Float]) -> [Float] {
    var n: Float = 0
    for x in v { n += x * x }
    let inv = Float(1) / max(.leastNonzeroMagnitude, n.squareRoot())
    return v.map { $0 * inv }
}

@Suite("Face clustering determinism + lifecycle")
struct FaceClusteringDeterminismTests {

    // F-C3-006 — the HNSW level draw must use a fixed seed, so two builds from
    // identical input produce identical kNN orderings (entropy RNG made face
    // clustering nondeterministic across engine launches).
    @Test("HNSWIndex: two builds from identical input yield identical kNN orderings")
    func hnswFixedSeedDeterminism() {
        func build() -> HNSWIndex {
            let idx = HNSWIndex(dim: 4, M: 16, efConstruction: 200, efSearch: 50)
            for i in 0..<256 {
                let a = Double(i) * 0.013
                idx.insert(l2norm([Float(cos(a)), Float(sin(a)),
                                   Float(cos(a * 0.5)), Float(sin(a * 0.5))]))
            }
            return idx
        }
        let q = l2norm([1.0, 0.05, 0.9, 0.1])
        let ha = build().search(q, k: 16).map { $0.0 }
        let hb = build().search(q, k: 16).map { $0.0 }
        #expect(!ha.isEmpty)
        #expect(ha == hb, "fixed-seed HNSW must return identical neighbour IDs across builds")
    }

    // F-C3-007 — cluster-ID assignment must follow sorted-root order so it is a
    // deterministic function of the input (not HashMap iteration order). Two
    // identical pairs on orthogonal axes label as [0,0,1,1] every time.
    @Test("IdentityClustering: cluster-ID assignment is stable (sorted-root iteration)")
    func sortedRootDeterminism() {
        let embeddings = [
            l2norm([1, 0, 0]), l2norm([1, 0, 0]),
            l2norm([0, 1, 0]), l2norm([0, 1, 0]),
        ]
        let searcher: (Int) -> [(neighbor: Int, similarity: Float)] = { i in
            (0..<embeddings.count).filter { $0 != i }.map { j in
                var s: Float = 0
                for d in 0..<3 { s += embeddings[i][d] * embeddings[j][d] }
                return (neighbor: j, similarity: s)
            }
        }
        let a = IdentityClustering.cluster(embeddings: embeddings, searcher: searcher)
        let b = IdentityClustering.cluster(embeddings: embeddings, searcher: searcher)
        #expect(a.clusterCount == 2)
        #expect(a.clusterIDs == [0, 0, 1, 1])
        #expect(a.clusterIDs == b.clusterIDs, "clustering must be deterministic across runs")
    }

    // F-C3-008 — the O(dim) running-sum Pass-2 centroid must assign outliers
    // exactly as a full recompute would. Two outliers each join a 3-face core;
    // the second relies on the centroid updated by the first.
    @Test("IdentityClustering: running-sum Pass-2 assigns outliers into one core")
    func runningSumOutlierAssignment() {
        let embeddings = [
            l2norm([1, 0, 0]), l2norm([1, 0, 0]), l2norm([1, 0, 0]), // tight core
            l2norm([0.6, 0.8, 0.0]),  // cosine 0.60 to core: pass-1 singleton, pass-2 join
            l2norm([0.6, 0.0, 0.8]),  // cosine 0.60 to core; 0.36 to the other outlier
        ]
        let searcher: (Int) -> [(neighbor: Int, similarity: Float)] = { i in
            (0..<embeddings.count).filter { $0 != i }.map { j in
                var s: Float = 0
                for d in 0..<3 { s += embeddings[i][d] * embeddings[j][d] }
                return (neighbor: j, similarity: s)
            }
        }
        let r = IdentityClustering.cluster(embeddings: embeddings, searcher: searcher)
        #expect(r.outliersAssigned == 2)
        #expect(r.clusterCount == 1)
        #expect(Set(r.clusterIDs).count == 1, "all five faces land in one cluster")
    }

    // F-C3-042 — a cancellation/shutdown signal polled in the cluster loop must
    // abort the pass and report `cancelled` so the caller skips persisting.
    @Test("IdentityClustering: shouldCancel aborts the pass at a safe boundary")
    func cooperativeCancellation() {
        let embeddings = (0..<50).map { _ in l2norm([1, 0, 0]) }
        let searcher: (Int) -> [(neighbor: Int, similarity: Float)] = { i in
            (0..<50).filter { $0 != i }.map { (neighbor: $0, similarity: Float(1)) }
        }
        var polls = 0
        let cancelled = IdentityClustering.cluster(
            embeddings: embeddings, searcher: searcher,
            shouldCancel: { polls += 1; return polls > 2 }
        )
        #expect(cancelled.cancelled, "the pass must report cancellation")

        let completed = IdentityClustering.cluster(embeddings: embeddings, searcher: searcher)
        #expect(!completed.cancelled)
        #expect(completed.clusterCount == 1, "the uncancelled pass runs to completion")
    }

    // R-07: the scan-cancel mirror is sticky, so a cluster started after a
    // cancelled scan sees current==true at entry (baseline==true) and must NOT
    // abort on that stale signal — but a genuine shutdown (dedicated mirror) must
    // always abort, even with that stale baseline.
    @Test("clusterShouldCancel: stale scan-cancel ignored; fresh cancel + shutdown honored")
    func clusterShouldCancelSemantics() {
        // Stale scan-cancel (true at entry, still true) → keep running.
        #expect(!FaceClustering.clusterShouldCancel(baseline: true, current: true))
        // Cancel that flips true DURING the run → abort.
        #expect(FaceClustering.clusterShouldCancel(baseline: false, current: true))
        // No cancel → keep running.
        #expect(!FaceClustering.clusterShouldCancel(baseline: false, current: false))
        // Shutdown ALWAYS aborts, even under a stale scan-cancel baseline.
        #expect(FaceClustering.clusterShouldCancel(baseline: true, current: true, shuttingDown: true))
        #expect(FaceClustering.clusterShouldCancel(baseline: false, current: false, shuttingDown: true))
    }
}
