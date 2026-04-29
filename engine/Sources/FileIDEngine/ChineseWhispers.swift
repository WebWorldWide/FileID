// Chinese Whispers — randomized graph-clustering algorithm.
// Linear-time in the number of edges, threshold-free in the global
// sense (cluster boundaries are decided by local neighborhood majority,
// not a fixed L2 threshold), and benchmarks best-in-class for face
// clustering specifically (Biemann 2006; dlib's reference impl).
//
// Algorithm:
//   1. kNN graph: each node has edges to its top-k nearest neighbors
//      whose similarity is above `cosineThreshold`. Edge weight = sim.
//   2. Init: each node is its own cluster (id = node index).
//   3. Iterate up to `maxIter`:
//      - shuffle node order (with seeded RNG for reproducibility)
//      - for each node, sum incoming edge weights per neighbor's cluster
//      - reassign self to the cluster with the highest summed weight
//      - if no node changed: converged
//   4. Output: cluster id per node.
//
// Complexity: O(N · k · iter). At N=50k, k=20, iter=20 → ~20M ops; runs
// in single-digit seconds in pure Swift on M-series.
import Foundation

public enum ChineseWhispers {

    public struct Hyperparameters: Sendable {
        public let kNN: Int
        public let cosineThreshold: Float
        public let maxIter: Int
        public let seed: UInt64
        public init(kNN: Int = 20,
                    cosineThreshold: Float = 0.40,
                    maxIter: Int = 20,
                    seed: UInt64 = 0xC0FFEEC0FFEEC0FF) {
            self.kNN = kNN
            self.cosineThreshold = cosineThreshold
            self.maxIter = maxIter
            self.seed = seed
        }
    }

    /// Edge in the kNN graph. Symmetric — each (a, b) appears as both a→b
    /// and b→a in the adjacency list.
    public struct Edge: Sendable, Hashable {
        public let other: Int
        public let weight: Float
    }

    /// Run Chinese Whispers over a precomputed adjacency list.
    /// `adjacency.count` must equal the number of nodes.
    /// Returns `clusterID[i]` per node — values are arbitrary integers
    /// (NOT contiguous from 0) but stable across the same input + seed.
    public static func cluster(
        adjacency: [[Edge]],
        params: Hyperparameters = Hyperparameters()
    ) -> (clusterIDs: [Int], iterations: Int, changes: [Int]) {
        let n = adjacency.count
        guard n > 0 else { return ([], 0, []) }

        // Each node starts as its own cluster.
        var clusterID = [Int](0..<n)

        // Seeded LCG for reproducibility — std lib's `shuffle` is fine
        // but uses SystemRandomNumberGenerator which isn't reproducible
        // across runs. We want deterministic outputs given the same data.
        var rng = SeededRNG(seed: params.seed)

        // Pre-allocate per-iteration scratch.
        var order = [Int](0..<n)
        var changes: [Int] = []
        changes.reserveCapacity(params.maxIter)
        var converged = false
        var iter = 0

        // Reusable cluster→weight scratch dictionary. Cleared per node.
        var weights = [Int: Float]()

        while iter < params.maxIter && !converged {
            iter += 1
            shuffle(&order, rng: &rng)
            var changedThisRound = 0

            for node in order {
                let edges = adjacency[node]
                if edges.isEmpty { continue }

                weights.removeAll(keepingCapacity: true)
                for e in edges {
                    let cid = clusterID[e.other]
                    weights[cid, default: 0] += e.weight
                }
                guard let (best, _) = weights.max(by: { $0.value < $1.value }) else {
                    continue
                }
                if clusterID[node] != best {
                    clusterID[node] = best
                    changedThisRound += 1
                }
            }
            changes.append(changedThisRound)
            if changedThisRound == 0 { converged = true }
        }
        return (clusterID, iter, changes)
    }

    // MARK: - kNN graph build

    /// Build a kNN graph from an array of L2-normalized embeddings.
    /// Returns symmetric adjacency: each (a, b) edge appears in both
    /// `adjacency[a]` and `adjacency[b]`. Cosine similarity is computed
    /// as a dot product (embeddings are pre-normalized) — values in
    /// [-1, 1] but for ArcFace face embeddings essentially in [0, 1].
    ///
    /// `searcher` is a closure that, given an embedding, returns the
    /// indices of its k nearest neighbors plus their cosine similarities.
    /// Caller supplies an HNSW-backed implementation; the algorithm here
    /// stays pure.
    public static func buildKNNGraph(
        nodeCount n: Int,
        params: Hyperparameters,
        searcher: (Int) -> [(neighbor: Int, similarity: Float)]
    ) -> [[Edge]] {
        var adjacency = [[Edge]](repeating: [], count: n)
        // We add each undirected edge twice (a→b, b→a). Use a set per
        // node to suppress duplicates from the symmetric add.
        var seen = [Set<Int>](repeating: Set<Int>(), count: n)
        for i in 0..<n {
            let neighbors = searcher(i)
            for (j, sim) in neighbors {
                guard j != i, sim >= params.cosineThreshold else { continue }
                if seen[i].insert(j).inserted {
                    adjacency[i].append(Edge(other: j, weight: sim))
                }
                if seen[j].insert(i).inserted {
                    adjacency[j].append(Edge(other: i, weight: sim))
                }
            }
        }
        return adjacency
    }
}

// MARK: - Seeded RNG (xorshift64*)

private struct SeededRNG: RandomNumberGenerator {
    private var state: UInt64
    init(seed: UInt64) { self.state = seed == 0 ? 1 : seed }
    mutating func next() -> UInt64 {
        var x = state
        x ^= x >> 12
        x ^= x << 25
        x ^= x >> 27
        state = x
        return x &* 0x2545F4914F6CDD1D
    }
}

private func shuffle<T>(_ array: inout [T], rng: inout SeededRNG) {
    let n = array.count
    guard n > 1 else { return }
    for i in stride(from: n - 1, to: 0, by: -1) {
        let j = Int(rng.next() % UInt64(i + 1))
        if i != j { array.swapAt(i, j) }
    }
}
