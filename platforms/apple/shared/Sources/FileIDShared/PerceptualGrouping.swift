// Perceptual near-duplicate grouping — pure + testable. The engine stores a
// 64-bit difference-hash (dHash) per image in `files.phash` (the Int64
// bit-pattern of a UInt64). Two images are "visually similar" when the Hamming
// distance of their dHashes is small; union-find clusters any transitive chain
// of within-threshold pairs into one group. Kept dependency-free so SharedTests
// can drive it directly, separate from the GRDB query that feeds it.
import Foundation

public enum PerceptualGrouping {
    /// Hamming distance of two 64-bit dHashes: popcount(a XOR b) over the raw
    /// 64 bits. We XOR the *bit patterns*, not the signed Int64 values — the
    /// sign bit is just bit 63 of the hash.
    public static func hammingDistance(_ a: Int64, _ b: Int64) -> Int {
        (UInt64(bitPattern: a) ^ UInt64(bitPattern: b)).nonzeroBitCount
    }

    /// Union-find clustering: items whose dHashes are within `maxHamming` of one
    /// another — transitively (A~B, B~C ⇒ {A,B,C}) — form a group. Returns groups
    /// of size >= 2, each as its member ids in first-seen order, with the groups
    /// themselves in first-seen order. O(N²) pairwise; callers guard input size.
    public static func groupByHamming(
        _ items: [(id: Int64, phash: Int64)],
        maxHamming: Int
    ) -> [[Int64]] {
        let n = items.count
        guard n > 1 else { return [] }

        var parent = Array(0..<n)
        func find(_ x: Int) -> Int {
            var r = x
            while parent[r] != r { parent[r] = parent[parent[r]]; r = parent[r] }
            return r
        }
        func union(_ a: Int, _ b: Int) {
            let ra = find(a), rb = find(b)
            // Point the higher index at the lower one so every component's root
            // is its smallest member index — keeps group order deterministic.
            if ra != rb { parent[max(ra, rb)] = min(ra, rb) }
        }

        for i in 0..<n {
            for j in (i + 1)..<n where hammingDistance(items[i].phash, items[j].phash) <= maxHamming {
                union(i, j)
            }
        }

        var order: [Int] = []
        var membersByRoot: [Int: [Int64]] = [:]
        for i in 0..<n {
            let r = find(i)
            if membersByRoot[r] == nil { order.append(r) }
            membersByRoot[r, default: []].append(items[i].id)
        }
        return order.compactMap { root in
            let ids = membersByRoot[root]!
            return ids.count >= 2 ? ids : nil
        }
    }
}
