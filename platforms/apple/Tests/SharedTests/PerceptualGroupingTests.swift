// Perceptual near-duplicate grouping: Hamming distance over 64-bit dHashes +
// union-find clustering. Drives the pure logic behind Cleanup's "Similar" mode
// directly, separate from the GRDB query that feeds it.
import Testing
import Foundation
@testable import FileIDShared

@Suite("PerceptualGrouping — Hamming + union-find")
struct PerceptualGroupingTests {

    private func i(_ u: UInt64) -> Int64 { Int64(bitPattern: u) }

    // MARK: - Hamming distance

    @Test("Hamming distance is popcount of XOR over the raw 64 bits")
    func hamming() {
        #expect(PerceptualGrouping.hammingDistance(i(0x0), i(0x0)) == 0)        // identical
        #expect(PerceptualGrouping.hammingDistance(i(0x0), i(0x1)) == 1)        // one bit
        #expect(PerceptualGrouping.hammingDistance(i(0x0), i(0xFF)) == 8)       // low byte
        // Sign bit is just bit 63 — XOR the bit patterns, not the signed values.
        #expect(PerceptualGrouping.hammingDistance(i(0x0), i(UInt64.max)) == 64)
        #expect(PerceptualGrouping.hammingDistance(i(UInt64.max), i(UInt64.max)) == 0)
    }

    // MARK: - Grouping

    @Test("Exact match (Ham 0) groups; threshold 0 is honored")
    func exactMatch() {
        let items: [(id: Int64, phash: Int64)] = [
            (1, i(0xABCD)), (2, i(0xABCD)), (3, i(0x1234)),
        ]
        let groups = PerceptualGrouping.groupByHamming(items, maxHamming: 0)
        #expect(groups.count == 1)
        #expect(Set(groups[0]) == Set([1, 2]))
    }

    @Test("Within threshold groups, far image stays alone")
    func withinThresholdAndFar() {
        // 0x0 and 0x1 are Hamming 1; 0xFFFF... is far from both.
        let items: [(id: Int64, phash: Int64)] = [
            (1, i(0x0)), (2, i(0x1)), (3, i(UInt64.max)),
        ]
        let groups = PerceptualGrouping.groupByHamming(items, maxHamming: 8)
        #expect(groups.count == 1)
        #expect(Set(groups[0]) == Set([1, 2]))   // id 3 excluded (singleton dropped)
    }

    @Test("Just over the threshold does NOT group")
    func justOverThreshold() {
        // 0x1FF has 9 bits set → Hamming 9 from 0x0, just over a threshold of 8.
        let items: [(id: Int64, phash: Int64)] = [
            (1, i(0x0)), (2, i(0x1FF)),
        ]
        #expect(PerceptualGrouping.hammingDistance(i(0x0), i(0x1FF)) == 9)
        #expect(PerceptualGrouping.groupByHamming(items, maxHamming: 8).isEmpty)
        // Bump the threshold to 9 and they group.
        let grouped = PerceptualGrouping.groupByHamming(items, maxHamming: 9)
        #expect(grouped.count == 1)
        #expect(Set(grouped[0]) == Set([1, 2]))
    }

    @Test("Transitivity: A~B, B~C ⇒ one group even when A and C are not direct")
    func transitivity() {
        // a=0x00, b=0x0F (Ham 4 from a), c=0xFF (Ham 4 from b, but Ham 8 from a).
        let a = i(0x00), b = i(0x0F), c = i(0xFF)
        #expect(PerceptualGrouping.hammingDistance(a, b) == 4)
        #expect(PerceptualGrouping.hammingDistance(b, c) == 4)
        #expect(PerceptualGrouping.hammingDistance(a, c) == 8)   // not within 5 directly

        let items: [(id: Int64, phash: Int64)] = [(1, a), (2, b), (3, c)]
        let groups = PerceptualGrouping.groupByHamming(items, maxHamming: 5)
        #expect(groups.count == 1)
        #expect(Set(groups[0]) == Set([1, 2, 3]))   // unioned transitively
    }

    @Test("Two independent clusters form two groups")
    func twoClusters() {
        let items: [(id: Int64, phash: Int64)] = [
            (1, i(0x0)),  (2, i(0x1)),                 // cluster A
            (3, i(UInt64.max)), (4, i(UInt64.max ^ 0x1)), // cluster B (Ham 1 apart)
            (5, i(0x00FF00FF)),                        // loner
        ]
        let groups = PerceptualGrouping.groupByHamming(items, maxHamming: 4)
        #expect(groups.count == 2)
        let sets = groups.map { Set($0) }
        #expect(sets.contains(Set([1, 2])))
        #expect(sets.contains(Set([3, 4])))
    }

    @Test("Empty / single-item input yields no groups")
    func degenerate() {
        #expect(PerceptualGrouping.groupByHamming([], maxHamming: 8).isEmpty)
        #expect(PerceptualGrouping.groupByHamming([(1, i(0x0))], maxHamming: 8).isEmpty)
    }
}
