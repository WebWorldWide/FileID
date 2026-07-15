import Foundation
import FileIDShared

struct ExactDuplicateSnapshot: Sendable {
    let groups: [DuplicateGroup]
    let partial: Bool
    let candidateCount: Int
    let skipped: Int
}

enum ExactDuplicateVerifier {
    static let candidateCap = 5_000
    static let readBudgetBytes: Int64 = 64 * 1024 * 1024 * 1024
    static let groupCap = 200
    static let memberCap = 500

    private struct Key: Hashable {
        let digest: Data
        let size: Int64
    }

    private struct CachedDigest {
        let digest: Data
        let verifiedAt: TimeInterval
        var age: UInt64
    }

    private final class CacheState: @unchecked Sendable {
        var values: [ExactFileDigest.CacheKey: CachedDigest] = [:]
        var age: UInt64 = 0
    }

    private static let cacheCapacity = 10_000
    private static let cacheState = CacheState()
    private static let queue = DispatchQueue(
        label: "com.fileid.exact-duplicate-verifier", qos: .userInitiated)
    private static let destructiveQueue = DispatchQueue(
        label: "com.fileid.exact-duplicate-delete", qos: .userInitiated,
        attributes: .concurrent)

    static func verify(
        candidates: [FileRow], candidateCount: Int, inputPartial: Bool
    ) async -> ExactDuplicateSnapshot {
        await withCheckedContinuation { continuation in
            queue.async {
                var grouped: [Key: [FileRow]] = [:]
                var skipped = 0
                for file in candidates {
                    guard let digest = digest(for: file) else {
                        skipped += 1
                        continue
                    }
                    grouped[Key(digest: digest, size: file.sizeBytes), default: []]
                        .append(file)
                }
                var groups = grouped.compactMap { key, files -> DuplicateGroup? in
                    guard files.count > 1 else { return nil }
                    let sorted = files.sorted(by: keeperComesFirst)
                    let visible = Array(sorted.prefix(memberCap))
                    return DuplicateGroup(
                        id: groupID(key.digest), files: visible,
                        totalFileCount: files.count,
                        totalBytes: Int64(files.count) * key.size)
                }
                groups.sort {
                    if $0.totalFileCount != $1.totalFileCount {
                        return $0.totalFileCount > $1.totalFileCount
                    }
                    return $0.id < $1.id
                }
                let groupTruncated = groups.count > groupCap
                groups = Array(groups.prefix(groupCap))
                let memberTruncated = groups.contains(where: \.isTruncated)
                continuation.resume(returning: ExactDuplicateSnapshot(
                    groups: groups,
                    partial: inputPartial || skipped > 0 || groupTruncated || memberTruncated,
                    candidateCount: candidateCount,
                    skipped: skipped))
            }
        }
    }

    private static func digest(for file: FileRow) -> Data? {
        guard let key = ExactFileDigest.cacheKey(
            url: file.url, expectedSize: file.sizeBytes) else { return nil }
        cacheState.age &+= 1
        let now = Date().timeIntervalSinceReferenceDate
        if var cached = cacheState.values[key], now - cached.verifiedAt < 30 {
            cached.age = cacheState.age
            cacheState.values[key] = cached
            return cached.digest
        }
        guard let digest = ExactFileDigest.compute(
            url: file.url, expectedSize: file.sizeBytes),
              ExactFileDigest.cacheKey(
                url: file.url, expectedSize: file.sizeBytes) == key else { return nil }
        cacheState.values[key] = CachedDigest(
            digest: digest, verifiedAt: now, age: cacheState.age)
        if cacheState.values.count > cacheCapacity {
            let oldest = cacheState.values.sorted { $0.value.age < $1.value.age }
                .prefix(cacheCapacity / 10)
                .map(\.key)
            for key in oldest { cacheState.values.removeValue(forKey: key) }
        }
        return digest
    }

    static func matchesImmediately(
        keeper: URL, victim: URL, expectedSize: Int64
    ) async -> ExactFileDigest.CacheKey? {
        await withCheckedContinuation { continuation in
            destructiveQueue.async {
                let match = ExactFileDigest.matchWithIdentity(
                    first: keeper, second: victim, expectedSize: expectedSize)
                continuation.resume(returning: match?.second)
            }
        }
    }

    private static func keeperComesFirst(_ lhs: FileRow, _ rhs: FileRow) -> Bool {
        let la = lhs.aesthetic ?? 0
        let ra = rhs.aesthetic ?? 0
        if la != ra { return la > ra }
        if lhs.sizeBytes != rhs.sizeBytes { return lhs.sizeBytes > rhs.sizeBytes }
        let ld = lhs.createdAt?.timeIntervalSince1970 ?? .greatestFiniteMagnitude
        let rd = rhs.createdAt?.timeIntervalSince1970 ?? .greatestFiniteMagnitude
        if ld != rd { return ld < rd }
        if lhs.pathText.count != rhs.pathText.count {
            return lhs.pathText.count < rhs.pathText.count
        }
        return lhs.pathText < rhs.pathText
    }

    private static func groupID(_ digest: Data) -> Int64 {
        var value: UInt64 = 0
        for (index, byte) in digest.prefix(8).enumerated() {
            value |= UInt64(byte) << (8 * index)
        }
        return Int64(bitPattern: value)
    }
}
