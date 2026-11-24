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
    /// themselves in first-seen order. Multi-index hashing keeps it near-linear on real libraries.
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

        // Identical hashes join first; only one representative per distinct hash is compared.
        var representative: [Int64: Int] = [:]
        var distinct: [Int] = []
        for i in 0..<n {
            if let rep = representative[items[i].phash] { union(rep, i) } else {
                representative[items[i].phash] = i
                distinct.append(i)
            }
        }

        if distinct.count <= 512 || maxHamming < 0 {
            for a in 0..<distinct.count {
                for b in (a + 1)..<distinct.count
                where hammingDistance(items[distinct[a]].phash, items[distinct[b]].phash) <= maxHamming {
                    union(distinct[a], distinct[b])
                }
            }
        } else {
            // Multi-index hashing: split the 64 bits into maxHamming + 1 slices. Two hashes within
            // maxHamming bits differ in at most that many slices, so they agree exactly on at least one.
            let slices = min(64, maxHamming + 1)
            for slice in 0..<slices {
                let lo = slice * 64 / slices
                let hi = (slice + 1) * 64 / slices
                let width = hi - lo
                let mask: UInt64 = width >= 64 ? ~0 : ((1 << UInt64(width)) - 1)
                var buckets: [UInt64: [Int]] = [:]
                for i in distinct {
                    let key = (UInt64(bitPattern: items[i].phash) >> UInt64(lo)) & mask
                    buckets[key, default: []].append(i)
                }
                for members in buckets.values where members.count > 1 {
                    for a in 0..<members.count {
                        let ia = members[a]
                        let ha = items[ia].phash
                        for b in (a + 1)..<members.count {
                            let ib = members[b]
                            if hammingDistance(ha, items[ib].phash) <= maxHamming { union(ia, ib) }
                        }
                    }
                }
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

    /// A RAW file whose same-named rendered twin (JPEG/HEIC/…, any folder, within `maxHamming` bits) sits in the same
    /// group is the same shot, not a look-alike of it: the RAW leaves the group so it can never be offered as a copy
    /// to trash. Groups left with fewer than two files disappear. Lockstep with Rust `read/dupes.rs` `without_raw_twins`.
    public static func droppingRawTwins(
        _ groups: [[Int64]], paths: [Int64: String], hashes: [Int64: Int64], maxHamming: Int
    ) -> [[Int64]] {
        func parts(_ path: String) -> (stem: String, isRaw: Bool) {
            let url = URL(fileURLWithPath: path)
            return (url.deletingPathExtension().lastPathComponent.lowercased(), SmartNameCompanions.role(url.pathExtension) == .raw)
        }
        return groups.compactMap { ids in
            let rendered: [(stem: String, hash: Int64)] = ids.compactMap { id in
                guard let path = paths[id], let hash = hashes[id], !parts(path).isRaw else { return nil }
                return (parts(path).stem, hash)
            }
            let kept = ids.filter { id in
                guard let path = paths[id], let hash = hashes[id] else { return true }
                let part = parts(path)
                guard part.isRaw else { return true }
                return !rendered.contains { $0.stem == part.stem && hammingDistance($0.hash, hash) <= maxHamming }
            }
            return kept.count > 1 ? kept : nil
        }
    }
}
