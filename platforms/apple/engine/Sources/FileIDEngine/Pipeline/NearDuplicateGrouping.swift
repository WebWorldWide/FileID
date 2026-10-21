import Foundation
import GRDB

/// Groups burst frames and RAW+JPEG pairs so Deep Analyze runs the VLM once per
/// group. Mirrored in the Rust engine (pipeline/near_duplicate_grouping.rs) with
/// the same thresholds; change both together.
enum NearDuplicateGrouping {
    struct Item: Sendable, Equatable {
        let id: Int64
        let path: String
        let time: Double?
        let phash: UInt64?
        let embedding: [Float]?
    }

    struct Group: Sendable, Equatable {
        let representative: Int64
        /// Every file in the group, representative included, in capture order.
        let members: [Int64]
    }

    static let maxGapSeconds = 2.0
    static let maxSpanSeconds = 12.0
    static let minCosine: Float = 0.97
    static let minSeedCosine: Float = 0.955
    static let maxHamming = 8
    static let maxGroupSize = 24
    /// A same-named file (RAW+JPEG, or a later edited JPEG export of the RAW) this similar
    /// counts as the same shot even when its file dates differ.
    static let sidecarCosine: Float = 0.90

    static let rawExtensions: Set<String> = [
        "3fr", "arw", "cr2", "cr3", "dng", "iiq", "nef", "nrw", "orf", "pef", "raf", "raw", "rw2", "srw", "x3f",
    ]

    static func stem(_ path: String) -> String {
        ((path as NSString).lastPathComponent as NSString).deletingPathExtension.lowercased()
    }

    static func isRaw(_ path: String) -> Bool {
        rawExtensions.contains((path as NSString).pathExtension.lowercased())
    }

    static func cosine(_ a: [Float], _ b: [Float]) -> Float {
        guard a.count == b.count, !a.isEmpty else { return 0 }
        var dot: Float = 0, na: Float = 0, nb: Float = 0
        for i in a.indices {
            dot += a[i] * b[i]
            na += a[i] * a[i]
            nb += b[i] * b[i]
        }
        guard na > 0, nb > 0 else { return 0 }
        return dot / (na.squareRoot() * nb.squareRoot())
    }

    static func hamming(_ a: UInt64, _ b: UInt64) -> Int { (a ^ b).nonzeroBitCount }

    /// Same-stem twins that look alike share the earlier capture time, so a JPEG
    /// exported hours after the RAW still sorts beside it.
    static func alignSidecarTimes(_ items: [Item]) -> [Item] {
        var byStem: [String: [Int]] = [:]
        for (i, item) in items.enumerated() { byStem[stem(item.path), default: []].append(i) }
        var result = items
        for indices in byStem.values where indices.count > 1 {
            for a in indices {
                for b in indices where b != a {
                    guard let ea = items[a].embedding, let eb = items[b].embedding,
                          cosine(ea, eb) >= sidecarCosine, let ta = result[a].time, let tb = items[b].time, tb < ta else { continue }
                    result[a] = Item(id: items[a].id, path: items[a].path, time: tb, phash: items[a].phash, embedding: items[a].embedding)
                }
            }
        }
        return result
    }

    static func groups(_ items: [Item]) -> [Group] {
        let ordered = alignSidecarTimes(items).sorted { lhs, rhs in
            switch (lhs.time, rhs.time) {
            case let (l?, r?) where l != r: return l < r
            case (nil, _?): return false
            case (_?, nil): return true
            default: break
            }
            let ls = stem(lhs.path), rs = stem(rhs.path)
            if ls != rs { return ls < rs }
            if lhs.path != rhs.path { return lhs.path < rhs.path }
            return lhs.id < rhs.id
        }
        var result: [Group] = []
        var current: [Item] = []
        for item in ordered {
            if let last = current.last, joins(item, current: current, last: last) {
                current.append(item)
            } else {
                if !current.isEmpty { result.append(group(current)) }
                current = [item]
            }
        }
        if !current.isEmpty { result.append(group(current)) }
        return result
    }

    private static func joins(_ item: Item, current: [Item], last: Item) -> Bool {
        guard current.count < maxGroupSize,
              let time = item.time, let lastTime = last.time, let firstTime = current[0].time,
              time - lastTime <= maxGapSeconds, time - firstTime <= maxSpanSeconds else { return false }
        let itemStem = stem(item.path)
        if current.contains(where: { stem($0.path) == itemStem && $0.path != item.path }) { return true }
        guard let embedding = item.embedding, let lastEmbedding = last.embedding,
              let seedEmbedding = current[0].embedding else { return false }
        let toLast = cosine(embedding, lastEmbedding)
        guard cosine(embedding, seedEmbedding) >= minSeedCosine else { return false }
        if toLast >= minCosine { return true }
        if let p = item.phash, let q = last.phash, p != 0, q != 0, hamming(p, q) <= maxHamming {
            return toLast >= minSeedCosine
        }
        return false
    }

    private static func group(_ items: [Item]) -> Group {
        var best = 0
        var bestScore = -Float.infinity
        for (i, candidate) in items.enumerated() {
            var score: Float = 0
            if let e = candidate.embedding {
                for other in items where other.id != candidate.id {
                    if let o = other.embedding { score += cosine(e, o) }
                }
            }
            if isRaw(candidate.path) { score -= 0.001 }
            if score > bestScore + 1e-6 {
                best = i
                bestScore = score
            }
        }
        return Group(representative: items[best].id, members: items.map(\.id))
    }

    /// ` 1`, ` 2`… per distinct shot (RAW+JPEG of one shot share a number),
    /// zero-padded when the group has ten or more shots. Empty for a single shot.
    static func sequenceSuffixes(paths: [String]) -> [String] {
        var order: [String: Int] = [:]
        for path in paths where order[stem(path)] == nil { order[stem(path)] = order.count + 1 }
        guard order.count > 1 else { return paths.map { _ in "" } }
        let width = order.count >= 10 ? 2 : 1
        return paths.map { path in
            let n = order[stem(path)] ?? 0
            return " " + String(repeating: "0", count: max(0, width - String(n).count)) + String(n)
        }
    }

    /// Image metadata for `ids`; CLIP vectors only for files within `maxGapSeconds`
    /// of another, so a large library never loads every embedding.
    static func loadItems(_ db: GRDB.Database, ids: [Int64]) throws -> [Item] {
        var meta: [(id: Int64, path: String, time: Double?, phash: UInt64?)] = []
        for start in stride(from: 0, to: ids.count, by: 500) {
            let slice = Array(ids[start..<min(ids.count, start + 500)])
            let marks = slice.map { _ in "?" }.joined(separator: ",")
            for row in try Row.fetchAll(db, sql: """
                SELECT id, path_text, created_at, phash FROM files
                WHERE id IN (\(marks)) AND kind = 'image'
                """, arguments: StatementArguments(slice)) {
                let phash: Int64? = row["phash"]
                meta.append((row["id"], row["path_text"], row["created_at"], phash.map { UInt64(bitPattern: $0) }))
            }
        }
        let timed = meta.compactMap { item in item.time.map { (item.id, $0) } }.sorted { $0.1 < $1.1 }
        var burst = Set<Int64>()
        var stems: [String: [Int64]] = [:]
        for item in meta { stems[stem(item.path), default: []].append(item.id) }
        for ids in stems.values where ids.count > 1 { burst.formUnion(ids) }
        for (a, b) in zip(timed, timed.dropFirst()) where b.1 - a.1 <= maxGapSeconds {
            burst.insert(a.0)
            burst.insert(b.0)
        }
        return try meta.map { item in
            var embedding: [Float]?
            if burst.contains(item.id),
               let blob = try Data.fetchOne(db, sql: "SELECT embedding FROM clip_embeddings WHERE file_id = ?", arguments: [item.id]) {
                embedding = decodeEmbedding(blob)
            }
            return Item(id: item.id, path: item.path, time: item.time, phash: item.phash, embedding: embedding)
        }
    }

    static func decodeEmbedding(_ data: Data) -> [Float]? {
        guard data.count >= 4, data.count % 4 == 0 else { return nil }
        return data.withUnsafeBytes { raw in
            (0..<(data.count / 4)).map { Float(bitPattern: UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: $0 * 4, as: UInt32.self))) }
        }
    }
}
