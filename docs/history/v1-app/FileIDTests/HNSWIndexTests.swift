import XCTest
@testable import FileID

final class HNSWIndexTests: XCTestCase {

    // Deterministic RNG so test results don't drift across runs.
    private struct DeterministicRNG: RandomNumberGenerator {
        var state: UInt64
        init(seed: UInt64) { self.state = seed }
        mutating func next() -> UInt64 {
            // SplitMix64 — small, fast, good enough for synthetic vectors.
            state &+= 0x9E3779B97F4A7C15
            var z = state
            z = (z ^ (z >> 30)) &* 0xBF58476D1CE4E5B9
            z = (z ^ (z >> 27)) &* 0x94D049BB133111EB
            return z ^ (z >> 31)
        }
    }

    private func randomVector(dim: Int, rng: inout DeterministicRNG) -> [Float] {
        (0..<dim).map { _ in
            let u = Float(rng.next()) / Float(UInt64.max)
            return u * 2 - 1
        }
    }

    private func l2(_ a: [Float], _ b: [Float]) -> Float {
        guard a.count == b.count else { return .infinity }
        var s: Float = 0
        for i in 0..<a.count { let d = a[i] - b[i]; s += d * d }
        return s.squareRoot()
    }

    func testInsertAndSearchOnSmallSet() {
        let index = HNSWIndex(dim: 8)
        let v1: [Float] = [1, 0, 0, 0, 0, 0, 0, 0]
        let v2: [Float] = [0, 1, 0, 0, 0, 0, 0, 0]
        let v3: [Float] = [0, 0, 1, 0, 0, 0, 0, 0]
        let id1 = index.insert(v1)
        let id2 = index.insert(v2)
        let id3 = index.insert(v3)
        XCTAssertGreaterThanOrEqual(id1, 0)
        XCTAssertGreaterThanOrEqual(id2, 0)
        XCTAssertGreaterThanOrEqual(id3, 0)
        XCTAssertEqual(index.count, 3)

        let result = index.search(v1, k: 1)
        XCTAssertFalse(result.isEmpty)
        XCTAssertEqual(result[0].0, id1, "Search must return the exact inserted vector for k=1.")
        XCTAssertEqual(result[0].1, 0, accuracy: 1e-5)
    }

    func testDimensionMismatchRejected() {
        let index = HNSWIndex(dim: 4)
        let bad = index.insert([1.0, 2.0, 3.0])  // dim=3, not 4
        XCTAssertEqual(bad, -1, "Mismatched-dim insert must return -1.")
        let result = index.search([1.0, 2.0, 3.0], k: 1)
        XCTAssertTrue(result.isEmpty, "Mismatched-dim search must return empty.")
    }

    func testRecallVsFlatScan() {
        // 1 000 random 64-d vectors, query 100 random points, ensure HNSW
        // top-1 matches flat-scan top-1 in ≥ 90 % of cases. (Recall@1 of
        // ~95 % is typical for HNSW with default params.)
        let dim = 64
        let n   = 1_000
        let q   = 100
        var rng = DeterministicRNG(seed: 42)

        var vectors: [[Float]] = []
        let index = HNSWIndex(dim: dim, M: 16, efConstruction: 100, efSearch: 50)
        for _ in 0..<n {
            let v = randomVector(dim: dim, rng: &rng)
            vectors.append(v)
            index.insert(v)
        }

        var hits = 0
        for _ in 0..<q {
            let query = randomVector(dim: dim, rng: &rng)
            // Flat-scan ground truth.
            var bestID = -1
            var bestDist: Float = .infinity
            for (i, v) in vectors.enumerated() {
                let d = l2(query, v)
                if d < bestDist { bestDist = d; bestID = i }
            }
            // HNSW top-1.
            let result = index.search(query, k: 1)
            if let first = result.first, Int(first.0) == bestID { hits += 1 }
        }
        let recall = Double(hits) / Double(q)
        XCTAssertGreaterThanOrEqual(recall, 0.90,
            "HNSW recall@1 was \(recall) — below the 0.90 floor.")
    }

    func testRemoveSkipsTombstones() {
        let index = HNSWIndex(dim: 4)
        let vecs: [[Float]] = [
            [1, 0, 0, 0],
            [0, 1, 0, 0],
            [0, 0, 1, 0],
            [0, 0, 0, 1],
        ]
        let ids = vecs.map { index.insert($0) }
        XCTAssertEqual(index.count, 4)

        // Remove the closest match to query [1, 0, 0, 0] (which is id 0).
        index.remove(id: ids[0])
        XCTAssertEqual(index.count, 3)

        let result = index.search([1, 0, 0, 0], k: 1)
        XCTAssertFalse(result.isEmpty)
        XCTAssertNotEqual(result[0].0, ids[0],
                          "Tombstoned id must not appear in search results.")
    }

    func testCompactDropsTombstones() {
        let index = HNSWIndex(dim: 4)
        for i in 0..<10 {
            _ = index.insert([Float(i), 0, 0, 0])
        }
        for i in 0..<5 { index.remove(id: Int32(i)) }
        XCTAssertEqual(index.count, 5)
        XCTAssertEqual(index.rawCount, 10)
        XCTAssertEqual(index.deletedFraction, 0.5, accuracy: 0.01)

        let map = index.compact()
        XCTAssertEqual(index.rawCount, 5)
        XCTAssertEqual(index.count, 5)
        // Old IDs 0..4 (deleted) should not appear in the map.
        for i in 0..<5 { XCTAssertNil(map[Int32(i)]) }
        // Old IDs 5..9 should map to fresh IDs.
        for i in 5..<10 { XCTAssertNotNil(map[Int32(i)]) }
    }

    func testEmptyIndexSearchReturnsEmpty() {
        let index = HNSWIndex(dim: 4)
        XCTAssertTrue(index.search([1, 0, 0, 0], k: 5).isEmpty)
    }

    func testSearchKLargerThanCount() {
        let index = HNSWIndex(dim: 4)
        _ = index.insert([1, 0, 0, 0])
        _ = index.insert([0, 1, 0, 0])
        let result = index.search([1, 0, 0, 0], k: 100)
        XCTAssertEqual(result.count, 2,
                       "k > count should return all available, not pad.")
    }
}
