import Foundation
import Accelerate
import CryptoKit
import FileIDShared

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

    /// Fixed seed for the geometric level-draw RNG. Pins the HNSW topology —
    /// and thus the approximate kNN neighbour sets — so face clustering derives
    /// the same cluster IDs and inherited People names on every re-cluster of an
    /// unchanged library. `Float.random` (system entropy) made identities hop
    /// run-to-run. Same constant the Windows engine pins. (audit F-C3-006)
    private static let levelSeed: UInt64 = 0xF11E_1D00
    private var rngState: UInt64 = HNSWIndex.levelSeed

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

    func isActive(id: Int32) -> Bool {
        let position = Int(id)
        return position >= 0 && position < nodes.count && !nodes[position].deleted
    }

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
            let neighbours = selectNeighbours(
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
        compact(checkpoint: {})
    }

    // A cancelled compaction leaves a partial graph that its worker must discard.
    func compact(checkpoint: () throws -> Void) rethrows -> [Int32: Int32] {
        let oldNodes = nodes
        var idMap: [Int32: Int32] = [:]
        var liveVectors: [[Float]] = []
        liveVectors.reserveCapacity(oldNodes.count - deletedCount)
        for (oldIdx, node) in oldNodes.enumerated() where !node.deleted {
            try checkpoint()
            idMap[Int32(oldIdx)] = Int32(liveVectors.count)
            liveVectors.append(node.vec)
        }

        // Reset and reinsert. Rewind the level-draw RNG to its fixed seed so the
        // rebuilt topology is identical to building fresh from these survivors —
        // otherwise compact() would keep drawing from the advanced stream and the
        // determinism contract above (stable cluster IDs / inherited names across
        // re-clusters) would silently break. (audit F-C3-006)
        nodes = []
        entryPoint = -1
        entryLevel = 0
        deletedCount = 0
        rngState = HNSWIndex.levelSeed
        for vec in liveVectors {
            try checkpoint()
            insert(vec)
        }
        return idMap
    }

    /// Health metric — fraction of tombstoned slots.
    var deletedFraction: Double {
        nodes.isEmpty ? 0 : Double(deletedCount) / Double(nodes.count)
    }

    // MARK: - Heap helpers

    private struct MinHeap {
        private var storage: [(Int32, Float)] = []
        var isEmpty: Bool { storage.isEmpty }
        var count: Int { storage.count }
        var min: (Int32, Float)? { storage.first }
        mutating func reserveCapacity(_ n: Int) { storage.reserveCapacity(n) }
        mutating func insert(_ item: (Int32, Float)) {
            storage.append(item)
            var i = storage.count - 1
            while i > 0 {
                let p = (i - 1) >> 1
                if storage[p].1 <= storage[i].1 { break }
                storage.swapAt(i, p)
                i = p
            }
        }
        @discardableResult
        mutating func extractMin() -> (Int32, Float)? {
            guard !storage.isEmpty else { return nil }
            if storage.count == 1 { return storage.removeLast() }
            let top = storage[0]
            storage[0] = storage.removeLast()
            var i = 0
            let n = storage.count
            while true {
                let l = 2 * i + 1, r = l + 1
                var s = i
                if l < n && storage[l].1 < storage[s].1 { s = l }
                if r < n && storage[r].1 < storage[s].1 { s = r }
                if s == i { break }
                storage.swapAt(i, s)
                i = s
            }
            return top
        }
    }

    private struct MaxHeap {
        private var storage: [(Int32, Float)] = []
        var isEmpty: Bool { storage.isEmpty }
        var count: Int { storage.count }
        var max: (Int32, Float)? { storage.first }
        mutating func reserveCapacity(_ n: Int) { storage.reserveCapacity(n) }
        mutating func insert(_ item: (Int32, Float)) {
            storage.append(item)
            var i = storage.count - 1
            while i > 0 {
                let p = (i - 1) >> 1
                if storage[p].1 >= storage[i].1 { break }
                storage.swapAt(i, p)
                i = p
            }
        }
        @discardableResult
        mutating func extractMax() -> (Int32, Float)? {
            guard !storage.isEmpty else { return nil }
            if storage.count == 1 { return storage.removeLast() }
            let top = storage[0]
            storage[0] = storage.removeLast()
            var i = 0
            let n = storage.count
            while true {
                let l = 2 * i + 1, r = l + 1
                var s = i
                if l < n && storage[l].1 > storage[s].1 { s = l }
                if r < n && storage[r].1 > storage[s].1 { s = r }
                if s == i { break }
                storage.swapAt(i, s)
                i = s
            }
            return top
        }
        func sortedAscending() -> [(Int32, Float)] {
            var copy = self
            var out: [(Int32, Float)] = []
            out.reserveCapacity(storage.count)
            while let item = copy.extractMax() { out.append(item) }
            out.reverse()
            return out
        }
    }

    // MARK: - Internals

    /// Geometric-distribution level draw. mL controls the decay; expected
    /// number of layers ≈ log_M(N). Draws from a fixed-seed SplitMix64 stream
    /// (not system entropy) so the built graph is identical across runs.
    private func randomLevel() -> Int {
        let r = nextUniform()
        let l = -log(r) * mL
        // Cap at 16 layers — even at N=10 M, log_16(N) ≈ 5.8.
        return min(Int(floor(l)), 16)
    }

    /// SplitMix64 → a Float in (0, 1]. Deterministic given `rngState`; the
    /// `+1` keeps it strictly positive so `-log(r)` is finite.
    private func nextUniform() -> Float {
        rngState = rngState &+ 0x9E37_79B9_7F4A_7C15
        var z = rngState
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        z = z ^ (z >> 31)
        // Top 24 bits → [0, 2²⁴); (+1)/2²⁴ → (0, 1].
        let mantissa = Float(z >> 40)
        return (mantissa + 1) / Float(1 << 24)
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

    /// Beam search at a given layer — returns up to ef best (id, dist) pairs
    /// sorted ascending. MinHeap for the candidate frontier (O(log ef) extract-min)
    /// and MaxHeap for the bounded result window (O(1) peek-max, O(log ef) evict).
    /// O(ef·M·log ef) per call — replaces the O(ef²·M) sorted-array path that
    /// caused the HNSW build to stall on large face libraries (efConstruction=200,
    /// N=200K → ~30 min with sorted arrays; <1 min with heaps).
    private func searchLayer(
        query: [Float],
        entries: [(Int32, Float)],
        ef: Int,
        layer: Int
    ) -> [(Int32, Float)] {
        var visited = Set<Int32>()
        var candidates = MinHeap()
        var results = MaxHeap()
        candidates.reserveCapacity(ef * 2)
        results.reserveCapacity(ef + 1)

        for entry in entries {
            visited.insert(entry.0)
            candidates.insert(entry)
            results.insert(entry)
        }
        while results.count > ef { results.extractMax() }

        while let (curID, curDist) = candidates.extractMin() {
            if let worst = results.max?.1, curDist > worst, results.count >= ef {
                break
            }
            let curNode = nodes[Int(curID)]
            guard layer < curNode.levels.count else { continue }
            for nID in curNode.levels[layer] {
                if !visited.insert(nID).inserted { continue }
                let nIdx = Int(nID)
                if nodes[nIdx].deleted { continue }
                let d = l2(query, nodes[nIdx].vec)
                if results.count < ef || d < (results.max?.1 ?? .infinity) {
                    candidates.insert((nID, d))
                    results.insert((nID, d))
                    if results.count > ef { results.extractMax() }
                }
            }
        }
        return results.sortedAscending()
    }

    // Closest-only pruning loses routes between dense clusters.
    private func selectNeighbours(
        candidates: [(Int32, Float)],
        m: Int
    ) -> [(Int32, Float)] {
        let ordered = candidates.sorted { $0.1 == $1.1 ? $0.0 < $1.0 : $0.1 < $1.1 }
        guard ordered.count > m else { return ordered }
        var selected: [(Int32, Float)] = []
        selected.reserveCapacity(m)
        for candidate in ordered {
            let vector = nodes[Int(candidate.0)].vec
            if selected.allSatisfy({ l2(vector, nodes[Int($0.0)].vec) >= candidate.1 }) {
                selected.append(candidate)
                if selected.count == m { break }
            }
        }
        return selected
    }

    /// Trim overfull layers using the same diversity rule as insertion.
    private func trimNeighbours(of id: Int32, layer: Int, cap: Int) -> [Int32] {
        let node = nodes[Int(id)]
        let scored = node.levels[layer].compactMap { nID -> (Int32, Float)? in
            let nIdx = Int(nID)
            guard nIdx < nodes.count, !nodes[nIdx].deleted else { return nil }
            return (nID, l2(node.vec, nodes[nIdx].vec))
        }
        return selectNeighbours(candidates: scored, m: cap).map { $0.0 }
    }

    /// L2 distance via Accelerate. Same metric and dim-mismatch semantics
    /// as `FaceClusteringService.l2` — returns .infinity on dim mismatch.
    private func l2(_ a: [Float], _ b: [Float]) -> Float {
        guard a.count == b.count, a.count == dim else { return .infinity }
        // R3-09: vDSP_distancesq computes Σ(a−b)² in one pass with no temporary,
        // dropping the per-call dim-sized `[Float]` alloc + zero-fill that ran in
        // the innermost HNSW distance loop.
        var sumSq: Float = 0
        vDSP_distancesq(a, 1, b, 1, &sumSq, vDSP_Length(dim))
        return sumSq.squareRoot()
    }
}


extension HNSWIndex {
    enum SnapshotError: Error {
        case invalidFormat, incompatibleSpace, staleRevision
    }

    func writeSnapshot(to url: URL, modelID: String, sourceRevision: String) throws {
        guard url.isFileURL else { throw SnapshotError.invalidFormat }
        try ReadOnlyLocations.requireWritable(url)
        try snapshot(modelID: modelID, sourceRevision: sourceRevision).write(to: url, options: .atomic)
    }

    static func readSnapshot(from url: URL, modelID: String, sourceRevision: String) throws -> HNSWIndex {
        guard url.isFileURL,
              let properties = try? url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey]),
              properties.isRegularFile == true, let size = properties.fileSize,
              size > 32, size <= 512 * 1024 * 1024 else { throw SnapshotError.invalidFormat }
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        guard let data = try handle.read(upToCount: size + 1),
              data.count <= 512 * 1024 * 1024 else { throw SnapshotError.invalidFormat }
        guard try handle.read(upToCount: 1)?.isEmpty != false else { throw SnapshotError.invalidFormat }
        return try restoreSnapshot(data, modelID: modelID, sourceRevision: sourceRevision)
    }

    func snapshot(modelID: String, sourceRevision: String) throws -> Data {
        try Task.checkCancellation()
        guard (1...200).contains(modelID.utf8.count), (1...200).contains(sourceRevision.utf8.count),
              (1...4096).contains(dim), (4...64).contains(M),
              (1...4096).contains(efConstruction), (1...4096).contains(efSearch),
              nodes.count <= 250_000 else { throw SnapshotError.invalidFormat }
        var result = Data("FileID-HNSW-2".utf8)
        func append<T: FixedWidthInteger>(_ value: T) {
            var littleEndian = value.littleEndian
            withUnsafeBytes(of: &littleEndian) { result.append(contentsOf: $0) }
        }
        func appendString(_ value: String) {
            let bytes = Data(value.utf8)
            append(UInt32(bytes.count))
            result.append(bytes)
        }
        appendString(modelID)
        appendString(sourceRevision)
        append(UInt32(dim))
        append(UInt32(M))
        append(UInt32(efConstruction))
        append(UInt32(efSearch))
        append(rngState)
        append(entryPoint)
        append(UInt32(entryLevel))
        append(UInt32(nodes.count))
        for node in nodes {
            try Task.checkCancellation()
            guard node.vec.count == dim, node.vec.allSatisfy(\.isFinite),
                  (1...33).contains(node.levels.count) else { throw SnapshotError.invalidFormat }
            append(UInt8(node.deleted ? 1 : 0))
            append(UInt8(node.levels.count))
            for value in node.vec { append(value.bitPattern) }
            for (level, neighbours) in node.levels.enumerated() {
                guard neighbours.count <= (level == 0 ? Mmax0 : M) else { throw SnapshotError.invalidFormat }
                append(UInt32(neighbours.count))
                for neighbour in neighbours { append(neighbour) }
            }
            guard result.count <= 512 * 1024 * 1024 - 32 else { throw SnapshotError.invalidFormat }
        }
        result.append(contentsOf: SHA256.hash(data: result))
        return result
    }

    static func restoreSnapshot(_ data: Data, modelID: String, sourceRevision: String) throws -> HNSWIndex {
        try Task.checkCancellation()
        guard data.count > 32, data.count <= 512 * 1024 * 1024 else { throw SnapshotError.invalidFormat }
        let payload = Data(data.dropLast(32))
        guard Data(SHA256.hash(data: payload)) == Data(data.suffix(32)) else { throw SnapshotError.invalidFormat }
        var reader = SnapshotReader(data: payload)
        guard try reader.bytes(13) == Data("FileID-HNSW-2".utf8) else { throw SnapshotError.invalidFormat }
        guard try reader.string() == modelID else { throw SnapshotError.incompatibleSpace }
        guard try reader.string() == sourceRevision else { throw SnapshotError.staleRevision }
        let dim = Int(try reader.integer(UInt32.self))
        let m = Int(try reader.integer(UInt32.self))
        let construction = Int(try reader.integer(UInt32.self))
        let search = Int(try reader.integer(UInt32.self))
        let rng = try reader.integer(UInt64.self)
        let entry = try reader.integer(Int32.self)
        let entryLevel = Int(try reader.integer(UInt32.self))
        let count = Int(try reader.integer(UInt32.self))
        guard (1...4096).contains(dim), (4...64).contains(m),
              (1...4096).contains(construction), (1...4096).contains(search),
              entryLevel <= 32, count <= 250_000, rng != 0,
              count <= reader.remaining / (dim * 4 + 6) else { throw SnapshotError.invalidFormat }
        let index = HNSWIndex(dim: dim, M: m, efConstruction: construction, efSearch: search)
        index.nodes.reserveCapacity(count)
        for _ in 0..<count {
            try Task.checkCancellation()
            let deleted = try reader.integer(UInt8.self)
            let levelCount = Int(try reader.integer(UInt8.self))
            guard deleted <= 1, (1...33).contains(levelCount) else { throw SnapshotError.invalidFormat }
            var vector: [Float] = []
            vector.reserveCapacity(dim)
            for _ in 0..<dim {
                let value = Float(bitPattern: try reader.integer(UInt32.self))
                guard value.isFinite else { throw SnapshotError.invalidFormat }
                vector.append(value)
            }
            var levels: [[Int32]] = []
            for level in 0..<levelCount {
                let neighboursCount = Int(try reader.integer(UInt32.self))
                guard neighboursCount <= (level == 0 ? m * 2 : m) else { throw SnapshotError.invalidFormat }
                var neighbours: [Int32] = []
                for _ in 0..<neighboursCount {
                    let neighbour = try reader.integer(Int32.self)
                    guard neighbour >= 0, Int(neighbour) < count else { throw SnapshotError.invalidFormat }
                    neighbours.append(neighbour)
                }
                guard Set(neighbours).count == neighbours.count else { throw SnapshotError.invalidFormat }
                levels.append(neighbours)
            }
            index.nodes.append(Node(vec: vector, levels: levels, deleted: deleted == 1))
        }
        guard reader.remaining == 0 else { throw SnapshotError.invalidFormat }
        if count == 0 {
            guard entry == -1, entryLevel == 0 else { throw SnapshotError.invalidFormat }
        } else {
            guard entry >= 0, Int(entry) < count,
                  index.nodes[Int(entry)].levels.count == entryLevel + 1,
                  index.nodes.allSatisfy({ $0.levels.count <= entryLevel + 1 }) else { throw SnapshotError.invalidFormat }
        }
        for (id, node) in index.nodes.enumerated() {
            try Task.checkCancellation()
            for (level, neighbours) in node.levels.enumerated() {
                guard neighbours.allSatisfy({ Int($0) != id && index.nodes[Int($0)].levels.count > level }) else {
                    throw SnapshotError.invalidFormat
                }
            }
        }
        index.rngState = rng
        index.entryPoint = entry
        index.entryLevel = entryLevel
        index.deletedCount = index.nodes.filter(\.deleted).count
        return index
    }

    private struct SnapshotReader {
        let data: Data
        var offset = 0
        var remaining: Int { data.count - offset }

        mutating func bytes(_ count: Int) throws -> Data {
            guard count >= 0, count <= remaining else { throw SnapshotError.invalidFormat }
            defer { offset += count }
            return data.subdata(in: offset..<(offset + count))
        }

        mutating func integer<T: FixedWidthInteger>(_ type: T.Type) throws -> T {
            let count = MemoryLayout<T>.size
            guard count <= remaining else { throw SnapshotError.invalidFormat }
            defer { offset += count }
            return data.withUnsafeBytes { T(littleEndian: $0.loadUnaligned(fromByteOffset: offset, as: T.self)) }
        }

        mutating func string() throws -> String {
            let count = Int(try integer(UInt32.self))
            guard (1...200).contains(count), let value = String(data: try bytes(count), encoding: .utf8) else {
                throw SnapshotError.invalidFormat
            }
            return value
        }
    }
}
