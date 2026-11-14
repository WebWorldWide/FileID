import CryptoKit
import Foundation
import FileIDShared

struct ExactDuplicateSnapshot: Sendable {
    let groups: [DuplicateGroup]
    let partial: Bool
    let candidateCount: Int
    let skipped: Int
    var capped = false
    var cancelled = false
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
        var prefilter: Data
        var digest: Data?
        var age: UInt64
    }

    // Only touched on `queue`.
    private final class CacheState: @unchecked Sendable {
        var values: [ExactFileDigest.CacheKey: CachedDigest] = [:]
        var age: UInt64 = 0
    }

    private final class CancelFlag: @unchecked Sendable {
        private let lock = NSLock()
        private var value = false
        var isSet: Bool { lock.lock(); defer { lock.unlock() }; return value }
        func set() { lock.lock(); value = true; lock.unlock() }
    }

    private struct Prefiltered {
        let file: FileRow
        let key: ExactFileDigest.CacheKey
        let prefilter: Data
    }

    private static let cacheCapacity = 20_000
    private static let prefilterSpan = 64 * 1024
    private static let cacheState = CacheState()
    private static let queue = DispatchQueue(
        label: "com.fileid.exact-duplicate-verifier", qos: .userInitiated)
    private static let destructiveQueue = DispatchQueue(
        label: "com.fileid.exact-duplicate-delete", qos: .userInitiated,
        attributes: .concurrent)

    static func verify(
        candidates: [FileRow], candidateCount: Int, inputPartial: Bool
    ) async -> ExactDuplicateSnapshot {
        let cancel = CancelFlag()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
            queue.async {
                let stopped = ExactDuplicateSnapshot(
                    groups: [], partial: true, candidateCount: candidateCount,
                    skipped: 0, cancelled: true)
                var skipped = 0
                var bySample: [Key: [Prefiltered]] = [:]
                for file in candidates {
                    if cancel.isSet { return continuation.resume(returning: stopped) }
                    guard let sampled = prefilter(for: file) else {
                        skipped += 1
                        continue
                    }
                    bySample[Key(digest: sampled.prefilter, size: file.sizeBytes), default: []]
                        .append(sampled)
                }
                var grouped: [Key: [FileRow]] = [:]
                for members in bySample.values where members.count > 1 {
                    for member in members {
                        if cancel.isSet { return continuation.resume(returning: stopped) }
                        guard let digest = fullDigest(for: member) else {
                            skipped += 1
                            continue
                        }
                        grouped[Key(digest: digest, size: member.file.sizeBytes), default: []]
                            .append(member.file)
                    }
                }
                trimCache()
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
                    skipped: skipped,
                    capped: inputPartial || groupTruncated || memberTruncated))
            }
            }
        } onCancel: {
            cancel.set()
        }
    }

    // The cache key is (path, size, modified time, device, inode), so an entry is
    // valid for as long as the file is untouched: no expiry, only a size bound.
    private static func prefilter(for file: FileRow) -> Prefiltered? {
        guard let key = ExactFileDigest.cacheKey(
            url: file.url, expectedSize: file.sizeBytes) else { return nil }
        cacheState.age &+= 1
        if var cached = cacheState.values[key] {
            cached.age = cacheState.age
            cacheState.values[key] = cached
            return Prefiltered(file: file, key: key, prefilter: cached.prefilter)
        }
        let small = file.sizeBytes <= Int64(prefilterSpan * 2)
        let sample = small
            ? ExactFileDigest.compute(url: file.url, expectedSize: file.sizeBytes)
            : edgeSample(url: file.url, size: file.sizeBytes)
        guard let sample,
              ExactFileDigest.cacheKey(
                url: file.url, expectedSize: file.sizeBytes) == key else { return nil }
        cacheState.values[key] = CachedDigest(
            prefilter: sample, digest: small ? sample : nil, age: cacheState.age)
        return Prefiltered(file: file, key: key, prefilter: sample)
    }

    private static func fullDigest(for member: Prefiltered) -> Data? {
        if let digest = cacheState.values[member.key]?.digest { return digest }
        let file = member.file
        guard let digest = ExactFileDigest.compute(
            url: file.url, expectedSize: file.sizeBytes),
              ExactFileDigest.cacheKey(
                url: file.url, expectedSize: file.sizeBytes) == member.key else { return nil }
        cacheState.age &+= 1
        cacheState.values[member.key] = CachedDigest(
            prefilter: member.prefilter, digest: digest, age: cacheState.age)
        return digest
    }

    /// SHA-256 of the first and last 64 KiB. Only rules files out: copies are still
    /// decided by the full-file digest.
    private static func edgeSample(url: URL, size: Int64) -> Data? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        do {
            guard let head = try handle.read(upToCount: prefilterSpan),
                  head.count == prefilterSpan else { return nil }
            try handle.seek(toOffset: UInt64(size) - UInt64(prefilterSpan))
            guard let tail = try handle.read(upToCount: prefilterSpan),
                  tail.count == prefilterSpan else { return nil }
            var hasher = SHA256()
            hasher.update(data: head)
            hasher.update(data: tail)
            return Data(hasher.finalize())
        } catch {
            return nil
        }
    }

    private static func trimCache() {
        guard cacheState.values.count > cacheCapacity else { return }
        let excess = cacheState.values.count - cacheCapacity + cacheCapacity / 10
        let oldest = cacheState.values.sorted { $0.value.age < $1.value.age }
            .prefix(excess)
            .map(\.key)
        for key in oldest { cacheState.values.removeValue(forKey: key) }
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

/// One copy to move to the Trash. `exactKeeper` is the copy that stays; when
/// set, the victim is re-hashed against it immediately before the move.
struct DuplicateTrashItem: Sendable {
    let id: Int64
    let url: URL
    let size: Int64
    let exactKeeper: URL?
}

/// What the engine did with a Cleanup batch. `batchID` undoes it with `restoreFromTrash`;
/// nil `batchID` with nothing moved means the engine never answered.
struct DuplicateTrashOutcome: Sendable {
    var trashed: [(id: Int64, size: Int64)] = []
    var verificationFailed = 0
    var protectedLocation = 0
    var failed = 0
    var batchID: String?
    var engineUnavailable = false
    var timedOut = false
}

extension ExactDuplicateVerifier {
    /// The Cleanup trash path, shared with Chat. Each exact copy is hashed with its
    /// keeper (8 at a time) and sent as evidence; the engine re-proves it right before
    /// the move, refuses protected locations, journals the batch so Undo puts files
    /// and their catalog rows back, and removes the rows of moved files.
    @MainActor
    static func trashThroughEngine(_ work: [DuplicateTrashItem], engine: EngineClient,
                                   progress: (@MainActor @Sendable (String) -> Void)? = nil) async -> DuplicateTrashOutcome {
        var outcome = DuplicateTrashOutcome()
        progress?("Checking copies")
        let evidence = await withTaskGroup(of: (Int64, ExactTrashIdentity?, Bool).self) { group -> [(Int64, ExactTrashIdentity?, Bool)] in
            var results: [(Int64, ExactTrashIdentity?, Bool)] = []
            var next = 0
            func add(_ item: DuplicateTrashItem) {
                group.addTask {
                    guard let keeper = item.exactKeeper else { return (item.id, nil, true) }
                    let identity: ExactTrashIdentity? = await withCheckedContinuation { continuation in
                        destructiveQueue.async {
                            guard let copy = ExactFileDigest.computeWithIdentity(url: item.url, expectedSize: item.size),
                                  let kept = ExactFileDigest.computeWithIdentity(url: keeper, expectedSize: item.size),
                                  copy.digest == kept.digest else { return continuation.resume(returning: nil) }
                            let hex = copy.digest.map { String(format: "%02x", $0) }.joined()
                            continuation.resume(returning: ExactTrashIdentity(
                                fileID: item.id, path: item.url.path, sizeBytes: item.size, sha256Hex: hex,
                                keeperPath: keeper.path, keeperSizeBytes: item.size, keeperSha256Hex: hex))
                        }
                    }
                    return (item.id, identity, identity != nil)
                }
            }
            while next < min(8, work.count) { add(work[next]); next += 1 }
            for await result in group {
                results.append(result)
                if next < work.count { add(work[next]); next += 1 }
            }
            return results
        }
        let sizes = Dictionary(work.map { ($0.id, $0.size) }, uniquingKeysWith: { first, _ in first })
        let accepted = evidence.filter(\.2)
        outcome.verificationFailed = evidence.count - accepted.count
        guard !accepted.isEmpty else { return outcome }
        let identities = accepted.compactMap(\.1)
        progress?("Moving \(accepted.count.formatted()) to Trash")
        let signal = engine.bulkActionResultSignal
        guard engine.send(.trashFiles(fileIDs: accepted.map(\.0), exactIdentities: identities.isEmpty ? nil : identities)) else {
            outcome.engineUnavailable = true
            return outcome
        }
        guard let result = await TakeRequests.bulkResult(engine, after: signal, prefix: "trashFiles",
                                                         timeout: 300 + Double(accepted.count) / 5) else {
            outcome.timedOut = true
            return outcome
        }
        outcome.trashed = result.messages.filter(\.ok).compactMap(\.fileID).map { ($0, sizes[$0] ?? 0) }
        SpotlightIndexer.deindex(ids: outcome.trashed.map(\.id))
        for refusal in result.messages where !refusal.ok {
            switch trashRefusal(refusal) {
            case "protected": outcome.protectedLocation += 1
            case "changed": outcome.verificationFailed += 1
            default: outcome.failed += 1
            }
        }
        outcome.batchID = result.action.split(separator: ":", maxSplits: 1).dropFirst().first.map(String.init)
        return outcome
    }

    /// The helper's reason code; a helper that sends none is read from its fixed
    /// wording (ReadOnlyLocations and the trash checks).
    nonisolated static func trashRefusal(_ item: BulkActionItem) -> String {
        if let reason = item.reason { return reason }
        let text = item.message ?? ""
        if text.hasPrefix("This file belongs to a managed media library") || text.hasPrefix("This location is read-only") {
            return "protected"
        }
        if text.hasPrefix("File changed since") || text.hasSuffix("Nothing was moved.") { return "changed" }
        return "failed"
    }

    /// Puts a Cleanup batch back: files return to where they were and their catalog
    /// rows (tags, faces, embeddings) are restored. Returns how many came back.
    @MainActor
    static func undoTrash(batchID: String, engine: EngineClient) async -> (restored: Int, message: String?) {
        let signal = engine.bulkActionResultSignal
        guard engine.send(.restoreFromTrash(batchID: batchID)) else { return (0, "FileID is still getting ready. Try again in a moment.") }
        let result = await TakeRequests.bulkResult(engine, after: signal, prefix: "restoreFromTrash", timeout: 300)
        return (result?.succeeded ?? 0, result == nil ? "Putting files back didn't finish. Check the Trash." : result?.messages.first(where: { !$0.ok })?.message)
    }
}
