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
        let refused = result.messages.filter { !$0.ok }
        outcome.verificationFailed += refused.filter { ($0.message ?? "").contains("changed") || ($0.message ?? "").contains("match") }.count
        outcome.failed = refused.count - refused.filter { ($0.message ?? "").contains("changed") || ($0.message ?? "").contains("match") }.count
        outcome.batchID = result.action.split(separator: ":", maxSplits: 1).dropFirst().first.map(String.init)
        return outcome
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
