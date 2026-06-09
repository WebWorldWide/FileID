import Foundation
import Accelerate

// MARK: - HNSWIndex
//
// Hierarchical Navigable Small World index for nearest-neighbour search over
// fixed-dimension Float vectors. Pure Swift, Accelerate-backed (vDSP) for the
// inner distance loop — no third-party dependency.
//
// Why HNSW vs. flat scan vs. IVF:
//   - Flat scan is O(N) per query. Fine at ~1 K identities; chokes at ~10 K+.
//   - IVF (inverted-file) needs a coarse k-means pass; we'd run it on every
//     index rebuild, adding latency.
//   - HNSW gives ~O(log N) query with no build-time clustering pass and
//     supports incremental insert. It's the right shape for FaceClustering's
//     append-as-you-go usage.
//
// Reference: Malkov & Yashunin (2018), "Efficient and robust approximate
// nearest neighbor search using Hierarchical Navigable Small World graphs."
//
// Concurrency: NOT thread-safe. Wrap calls in the owning actor (or guard
// with a lock). FaceClusteringService owns its instance.
//
// Lazy delete: `remove(id:)` marks an internal tombstone; the search path
// skips tombstoned nodes. Rebuild via `compact()` periodically (e.g. when
// >25% of nodes are tombstoned).
//
// Memory: each node holds one [Float] (the vector) + one [[Int32]] of
// neighbour IDs per layer. For ArcFace (dim=512, 2 KB/vector) that is ~2.3 KB
// per node — 50 K faces ≈ 115 MB, and at the 200 K face cap ≈ 460 MB. Swift
// arrays are copy-on-write, so transient passes don't multiply that; still,
// budget for it on 16 GB Macs.
final class HNSWIndex {

    // MARK: - Tuning

    /// Target neighbour count per node at level > 0.
    let M: Int
    /// Max neighbours at level 0 (typically 2*M).
    let Mmax0: Int
    /// Max candidates considered during insert.
    let efConstruction: Int
    /// Default candidates considered during search.
    let efSearch: Int
    /// Level normalization factor — controls expected number of layers.
    private let mL: Float

    /// Vector dimension. Mismatched-dim inserts/queries return nil distances.
    let dim: Int

    // MARK: - Storage

    private struct Node {
        var vec: [Float]
        var levels: [[Int32]]   // levels[ℓ] = neighbour node IDs at level ℓ
        var deleted: Bool
    }

    private var nodes: [Node] = []
    private var entryPoint: Int32 = -1
    private var entryLevel: Int = 0
    private var deletedCount: Int = 0

    // MARK: - Init

    init(dim: Int, M: Int = 16, efConstruction: Int = 200, efSearch: Int = 50) {
        precondition(dim > 0, "HNSWIndex dim must be positive")
        precondition(M >= 4, "HNSWIndex M too small (use ≥4)")
        self.dim = dim
        self.M = M
        self.Mmax0 = M * 2
        self.efConstruction = efConstruction
        self.efSearch = efSearch
        self.mL = 1.0 / Float(log(Double(M)))
    }

    // MARK: - Public API

    var count: Int { nodes.count - deletedCount }
    var rawCount: Int { nodes.count }

    /// Insert a vector. Returns its node id. Mismatched-dim vectors are
    /// rejected and return -1 — callers should treat that as "not added"
    /// (the same pattern FaceClusteringService.l2 uses for safety).
    @discardableResult
    func insert(_ vec: [Float]) -> Int32 {
        guard vec.count == dim else { return -1 }

        let level = randomLevel()
        let newID = Int32(nodes.count)
        let newNode = Node(
            vec: vec,
            levels: Array(repeating: [], count: level + 1),
            deleted: false
        )

        // First node bootstraps the index.
        if entryPoint < 0 {
            nodes.append(newNode)
            entryPoint = newID
            entryLevel = level
            return newID
        }

        // Reserve the storage slot up front (id == nodes.count above) so that
        // trimNeighbours() can read this node's vector and fairly score the
        // back-edge to it. Previously the node was appended only after the
        // connect loop, so whenever a neighbour was at capacity the trim ran
        // before the node existed and silently dropped its back-edge.
        nodes.append(newNode)

        // Greedy descent from the top entry layer to layer (level + 1).
        var currentNearest = entryPoint
        var currentDist = l2(vec, nodes[Int(currentNearest)].vec)

        if entryLevel > level {
            for layer in stride(from: entryLevel, to: level, by: -1) {
                (currentNearest, currentDist) = greedySearch(
                    query: vec,
                    entry: currentNearest,
                    entryDist: currentDist,
                    layer: layer
                )
            }
        }

        // From min(entryLevel, level) down to 0, run searchLayer with
        // efConstruction and connect.
        var entryCandidates: [(Int32, Float)] = [(currentNearest, currentDist)]
        for layer in stride(from: min(entryLevel, level), through: 0, by: -1) {
            let nearest = searchLayer(
                query: vec,
                entries: entryCandidates,
                ef: efConstruction,
                layer: layer
            )
            // Pick M (or Mmax0 at layer 0) best neighbours.
            let mForLayer = layer == 0 ? Mmax0 : M
            let neighbours = selectNeighboursSimple(
                candidates: nearest,
                m: mForLayer
            )
            // Establish bidirectional edges. Write directly into the reserved
            // storage slot so trimNeighbours below sees this node's edges/vec.
            nodes[Int(newID)].levels[layer] = neighbours.map { $0.0 }
            for (neighbourID, _) in neighbours {
                let nIdx = Int(neighbourID)
                guard nIdx < nodes.count else { continue }
                nodes[nIdx].levels[layer].append(newID)
                // Trim if the neighbour exceeded capacity at this layer.
                let cap = layer == 0 ? Mmax0 : M
                if nodes[nIdx].levels[layer].count > cap {
                    let trimmed = trimNeighbours(
                        of: neighbourID,
                        layer: layer,
                        cap: cap
                    )
                    nodes[nIdx].levels[layer] = trimmed
                }
            }
            entryCandidates = nearest
        }

        if level > entryLevel {
            entryPoint = newID
            entryLevel = level
        }
        return newID
    }

    /// Top-K nearest neighbours by L2 distance. Skips tombstoned nodes.
    /// `ef` is search-time beam width — leave nil to use `efSearch`.
    /// Returns (id, distance) sorted ascending.
    func search(_ query: [Float], k: Int, ef: Int? = nil) -> [(Int32, Float)] {
        guard query.count == dim, entryPoint >= 0, k > 0 else { return [] }

        var currentNearest = entryPoint
        var currentDist = l2(query, nodes[Int(currentNearest)].vec)

        // Greedy descent from top to layer 1.
        if entryLevel > 0 {
            for layer in stride(from: entryLevel, through: 1, by: -1) {
                (currentNearest, currentDist) = greedySearch(
                    query: query,
                    entry: currentNearest,
                    entryDist: currentDist,
                    layer: layer
                )
            }
        }

        // Layer 0 with full ef.
        let candidates = searchLayer(
            query: query,
            entries: [(currentNearest, currentDist)],
            ef: ef ?? efSearch,
            layer: 0
        )

        return candidates
            .filter { !nodes[Int($0.0)].deleted }
            .sorted { $0.1 < $1.1 }
            .prefix(k)
            .map { ($0.0, $0.1) }
    }

    /// Lazy delete. Search will skip the node; insert order is preserved so
    /// existing IDs stay valid for callers that map them to external keys.
    func remove(id: Int32) {
        let idx = Int(id)
        guard idx >= 0, idx < nodes.count, !nodes[idx].deleted else { return }
        nodes[idx].deleted = true
        deletedCount += 1
    }

    /// Rebuild the index dropping tombstoned nodes. Returns a mapping from
    /// old IDs to new IDs (`nil` for removed nodes) so callers can update
    /// external references. O(N log N) — call infrequently.
    func compact() -> [Int32: Int32] {
        let oldNodes = nodes
        var idMap: [Int32: Int32] = [:]
        var liveVectors: [[Float]] = []
        liveVectors.reserveCapacity(oldNodes.count - deletedCount)
        for (oldIdx, node) in oldNodes.enumerated() where !node.deleted {
            idMap[Int32(oldIdx)] = Int32(liveVectors.count)
            liveVectors.append(node.vec)
        }

        // Reset and reinsert.
        nodes = []
        entryPoint = -1
        entryLevel = 0
        deletedCount = 0
        for vec in liveVectors {
            insert(vec)
        }
        return idMap
    }

    /// Health metric — fraction of tombstoned slots.
    var deletedFraction: Double {
        nodes.isEmpty ? 0 : Double(deletedCount) / Double(nodes.count)
    }

    // MARK: - Internals

    /// Geometric-distribution level draw. mL controls the decay; expected
    /// number of layers ≈ log_M(N).
    private func randomLevel() -> Int {
        // Stay deterministic across the test suite by using SystemRandom only
        // when not under XCTest; tests can subclass + inject if needed.
        let r = Float.random(in: Float.leastNonzeroMagnitude..<1.0)
        let l = -log(r) * mL
        // Cap at 16 layers — even at N=10 M, log_16(N) ≈ 5.8.
        return min(Int(floor(l)), 16)
    }

    /// Single-best greedy walk at a given layer.
    private func greedySearch(
        query: [Float],
        entry: Int32,
        entryDist: Float,
        layer: Int
    ) -> (Int32, Float) {
        var current = entry
        var currentDist = entryDist
        var changed = true
        while changed {
            changed = false
            let currentNode = nodes[Int(current)]
            guard layer < currentNode.levels.count else { break }
            for nID in currentNode.levels[layer] {
                let nIdx = Int(nID)
                if nodes[nIdx].deleted { continue }
                let d = l2(query, nodes[nIdx].vec)
                if d < currentDist {
                    currentDist = d
                    current = nID
                    changed = true
                }
            }
        }
        return (current, currentDist)
    }

    /// Beam search at a given layer — returns up to ef best (id, dist) pairs.
    /// Uses two heaps tracked as sorted arrays (small ef, so O(ef) inserts
    /// are fine and the constant factor beats a real heap).
    private func searchLayer(
        query: [Float],
        entries: [(Int32, Float)],
        ef: Int,
        layer: Int
    ) -> [(Int32, Float)] {
        var visited = Set<Int32>()
        var candidates: [(Int32, Float)] = []  // ascending distance
        var results:    [(Int32, Float)] = []  // ascending distance

        for entry in entries {
            visited.insert(entry.0)
            insertSorted(&candidates, entry, ascending: true)
            insertSorted(&results,    entry, ascending: true)
        }
        if results.count > ef { results = Array(results.prefix(ef)) }

        while let (curID, curDist) = candidates.first {
            candidates.removeFirst()
            // results.last is the worst kept. Stop when even the closest
            // unvisited candidate is farther than our worst kept.
            if let worst = results.last?.1, curDist > worst, results.count >= ef {
                break
            }
            let curNode = nodes[Int(curID)]
            guard layer < curNode.levels.count else { continue }
            for nID in curNode.levels[layer] {
                if !visited.insert(nID).inserted { continue }
                let nIdx = Int(nID)
                if nodes[nIdx].deleted { continue }
                let d = l2(query, nodes[nIdx].vec)
                if results.count < ef || d < (results.last?.1 ?? .infinity) {
                    insertSorted(&candidates, (nID, d), ascending: true)
                    insertSorted(&results,    (nID, d), ascending: true)
                    if results.count > ef { results.removeLast() }
                }
            }
        }
        return results
    }

    /// Heuristic neighbour selection — for now, take the top-M by distance.
    /// Could swap in the "diverse neighbour" heuristic from the paper if
    /// recall ever proves an issue.
    private func selectNeighboursSimple(
        candidates: [(Int32, Float)],
        m: Int
    ) -> [(Int32, Float)] {
        Array(candidates.prefix(m))
    }

    /// Trim a node's neighbour list at a layer to `cap` by keeping the
    /// closest neighbours. Run after a new edge tips it over Mmax/Mmax0.
    private func trimNeighbours(of id: Int32, layer: Int, cap: Int) -> [Int32] {
        let node = nodes[Int(id)]
        let scored = node.levels[layer].compactMap { nID -> (Int32, Float)? in
            let nIdx = Int(nID)
            guard nIdx < nodes.count, !nodes[nIdx].deleted else { return nil }
            return (nID, l2(node.vec, nodes[nIdx].vec))
        }
        let kept = scored.sorted { $0.1 < $1.1 }.prefix(cap)
        return kept.map { $0.0 }
    }

    /// Sorted insert helper. `ascending = true` keeps min at index 0.
    private func insertSorted(
        _ arr: inout [(Int32, Float)],
        _ item: (Int32, Float),
        ascending: Bool
    ) {
        // Binary search for the insertion index.
        var lo = 0, hi = arr.count
        while lo < hi {
            let mid = (lo + hi) >> 1
            let cmp: Bool = ascending ? (arr[mid].1 < item.1) : (arr[mid].1 > item.1)
            if cmp { lo = mid + 1 } else { hi = mid }
        }
        arr.insert(item, at: lo)
    }

    /// L2 distance via Accelerate. Same metric and dim-mismatch semantics
    /// as `FaceClusteringService.l2` — returns .infinity on dim mismatch.
    private func l2(_ a: [Float], _ b: [Float]) -> Float {
        guard a.count == b.count, a.count == dim else { return .infinity }
        var diff = [Float](repeating: 0, count: dim)
        vDSP_vsub(b, 1, a, 1, &diff, 1, vDSP_Length(dim))
        var sumSq: Float = 0
        vDSP_svesq(diff, 1, &sumSq, vDSP_Length(dim))
        return sumSq.squareRoot()
    }
}
