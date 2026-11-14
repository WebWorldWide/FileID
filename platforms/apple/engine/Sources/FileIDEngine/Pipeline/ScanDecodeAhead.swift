import Foundation
import AsyncAlgorithms
import FileIDShared
import FileIDImaging

/// A bounded hand-off between the scan's decoders and its inference workers.
/// A producer reserves room before it decodes, so the frames being decoded
/// and the frames waiting together stay under `capacity` and `budgetBytes`.
/// Each consumer waiting with nothing to take adds room for one more: an idle
/// inference worker may always have one file decoding for it, as it did when
/// workers decoded their own. Items leave in the order they were filled.
final class DecodeAheadQueue<Item: Sendable>: @unchecked Sendable {
    struct Ticket: Sendable, Hashable {
        fileprivate let id: UInt64
    }

    struct Stats: Sendable, Equatable {
        /// Calls to `next()`, and the items that were waiting at each.
        var asks = 0
        var depthSum = 0
        /// Items taken without waiting, and items a consumer had to wait for.
        var hits = 0
        var misses = 0
        var takeWaitSeconds = 0.0
        /// Reservations that had to wait for room.
        var reserveWaits = 0
        var reserveWaitSeconds = 0.0
        var peakHeld = 0
        var peakBytes = 0

        var averageDepth: Double { asks > 0 ? Double(depthSum) / Double(asks) : 0 }
    }

    private struct Reserver {
        let id: UInt64
        let bytes: Int
        let since: TimeInterval
        let continuation: CheckedContinuation<Ticket?, Never>
    }

    private struct Taker {
        let id: UInt64
        let since: TimeInterval
        let continuation: CheckedContinuation<Item?, Never>
    }

    private let lock = NSLock()
    private let capacity: Int
    private let budgetBytes: Int
    private let discard: @Sendable (Item) -> Void
    private var outstanding: [UInt64: Int] = [:]
    private var ready: [(item: Item, bytes: Int)] = []
    private var heldBytes = 0
    private var reservers: [Reserver] = []
    private var takers: [Taker] = []
    private var nextID: UInt64 = 0
    private var finished = false
    private var cancelled = false
    private var counters = Stats()

    /// `discard` receives every item the queue drops instead of delivering.
    init(capacity: Int, budgetBytes: Int, discard: @escaping @Sendable (Item) -> Void = { _ in }) {
        self.capacity = max(1, capacity)
        self.budgetBytes = max(0, budgetBytes)
        self.discard = discard
    }

    var stats: Stats {
        lock.lock(); defer { lock.unlock() }
        return counters
    }

    var held: (count: Int, bytes: Int) {
        lock.lock(); defer { lock.unlock() }
        return (outstanding.count + ready.count, heldBytes)
    }

    private func makeID() -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        nextID &+= 1
        return nextID
    }

    private func hasRoomLocked(for bytes: Int) -> Bool {
        let held = outstanding.count + ready.count
        guard held < capacity + takers.count else { return false }
        return held == 0 || heldBytes + bytes <= budgetBytes + takers.count * bytes
    }

    private func grantLocked(_ id: UInt64, bytes: Int) {
        outstanding[id] = bytes
        heldBytes += bytes
        counters.peakHeld = max(counters.peakHeld, outstanding.count + ready.count)
        counters.peakBytes = max(counters.peakBytes, heldBytes)
    }

    /// Grants waiting reservations in order and, once everything has been
    /// delivered after `finish()`, releases the waiting consumers.
    private func settleLocked() -> [() -> Void] {
        var resumes: [() -> Void] = []
        let now = ProcessInfo.processInfo.systemUptime
        while let first = reservers.first, hasRoomLocked(for: first.bytes) {
            reservers.removeFirst()
            grantLocked(first.id, bytes: first.bytes)
            counters.reserveWaitSeconds += now - first.since
            let ticket = Ticket(id: first.id)
            resumes.append { first.continuation.resume(returning: ticket) }
        }
        if finished, outstanding.isEmpty, ready.isEmpty {
            for taker in takers { resumes.append { taker.continuation.resume(returning: nil) } }
            takers.removeAll()
            for reserver in reservers { resumes.append { reserver.continuation.resume(returning: nil) } }
            reservers.removeAll()
        }
        return resumes
    }

    /// Room for one item of about `bytes`; waits while the queue is full.
    /// nil once the queue is finished or cancelled, or when the task is cancelled.
    func reserve(bytes: Int) async -> Ticket? {
        let id = makeID()
        let bytes = max(0, bytes)
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Ticket?, Never>) in
                lock.lock()
                if cancelled || finished || Task.isCancelled {
                    lock.unlock()
                    continuation.resume(returning: nil)
                    return
                }
                if reservers.isEmpty, hasRoomLocked(for: bytes) {
                    grantLocked(id, bytes: bytes)
                    lock.unlock()
                    continuation.resume(returning: Ticket(id: id))
                    return
                }
                counters.reserveWaits += 1
                reservers.append(Reserver(
                    id: id, bytes: bytes, since: ProcessInfo.processInfo.systemUptime, continuation: continuation))
                lock.unlock()
            }
        } onCancel: {
            self.dropReserver(id)
        }
    }

    private func dropReserver(_ id: UInt64) {
        lock.lock()
        guard let index = reservers.firstIndex(where: { $0.id == id }) else {
            lock.unlock()
            return
        }
        let reserver = reservers.remove(at: index)
        let resumes = settleLocked()
        lock.unlock()
        reserver.continuation.resume(returning: nil)
        resumes.forEach { $0() }
    }

    /// Hands over the item a reservation was made for; `bytes` is what it
    /// really holds. Never waits. After a cancel the item goes to `discard`.
    func fill(_ ticket: Ticket, _ item: Item, bytes: Int) {
        lock.lock()
        guard let reserved = outstanding.removeValue(forKey: ticket.id) else {
            lock.unlock()
            discard(item)
            return
        }
        let bytes = max(0, bytes)
        heldBytes -= reserved
        var deliver: (() -> Void)?
        if !takers.isEmpty {
            let taker = takers.removeFirst()
            counters.misses += 1
            counters.takeWaitSeconds += ProcessInfo.processInfo.systemUptime - taker.since
            deliver = { taker.continuation.resume(returning: item) }
        } else {
            ready.append((item, bytes))
            heldBytes += bytes
            counters.peakBytes = max(counters.peakBytes, heldBytes)
        }
        let resumes = settleLocked()
        lock.unlock()
        deliver?()
        resumes.forEach { $0() }
    }

    /// Gives a reservation back without an item.
    func abandon(_ ticket: Ticket) {
        lock.lock()
        guard let reserved = outstanding.removeValue(forKey: ticket.id) else {
            lock.unlock()
            return
        }
        heldBytes -= reserved
        let resumes = settleLocked()
        lock.unlock()
        resumes.forEach { $0() }
    }

    /// The oldest filled item; waits for one. nil once the queue is finished
    /// and empty, cancelled, or when the task is cancelled.
    func next() async -> Item? {
        let id = makeID()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Item?, Never>) in
                lock.lock()
                if cancelled || Task.isCancelled {
                    lock.unlock()
                    continuation.resume(returning: nil)
                    return
                }
                counters.asks += 1
                counters.depthSum += ready.count
                if !ready.isEmpty {
                    let first = ready.removeFirst()
                    heldBytes -= first.bytes
                    counters.hits += 1
                    let resumes = settleLocked()
                    lock.unlock()
                    continuation.resume(returning: first.item)
                    resumes.forEach { $0() }
                    return
                }
                if finished, outstanding.isEmpty {
                    lock.unlock()
                    continuation.resume(returning: nil)
                    return
                }
                takers.append(Taker(id: id, since: ProcessInfo.processInfo.systemUptime, continuation: continuation))
                let resumes = settleLocked()
                lock.unlock()
                resumes.forEach { $0() }
            }
        } onCancel: {
            self.dropTaker(id)
        }
    }

    private func dropTaker(_ id: UInt64) {
        lock.lock()
        guard let index = takers.firstIndex(where: { $0.id == id }) else {
            lock.unlock()
            return
        }
        let taker = takers.remove(at: index)
        lock.unlock()
        taker.continuation.resume(returning: nil)
    }

    /// No more reservations will come; consumers drain what is left.
    func finish() {
        lock.lock()
        finished = true
        let resumes = settleLocked()
        lock.unlock()
        resumes.forEach { $0() }
    }

    /// Drops everything held and releases everyone waiting. Items filled
    /// later go straight to `discard`.
    func cancel() {
        lock.lock()
        cancelled = true
        finished = true
        let dropped = ready.map(\.item)
        ready.removeAll()
        outstanding.removeAll()
        heldBytes = 0
        let waitingReservers = reservers
        let waitingTakers = takers
        reservers.removeAll()
        takers.removeAll()
        lock.unlock()
        for reserver in waitingReservers { reserver.continuation.resume(returning: nil) }
        for taker in waitingTakers { taker.continuation.resume(returning: nil) }
        dropped.forEach(discard)
    }
}

/// Decode-ahead for the scan: a few tasks hash and decode the next images
/// while the inference workers are busy, so a worker that frees up finds a
/// decoded frame instead of decoding one while the Neural Engine and GPU idle.
/// The number of inference pipelines stays the worker count.
enum ScanDecodeAhead {
    /// Flip to `true` to decode ahead by default (depth `automaticDepth`).
    /// `FILEID_DECODE_AHEAD` overrides either way: 0 is off, N keeps N frames ahead.
    static let enabledByDefault = false
    static let maxDepth = 32
    static let defaultBudgetMB = 96

    static func automaticDepth(workerCap: Int) -> Int {
        max(1, min(8, workerCap / 2))
    }

    /// Frames kept decoded ahead of the workers; 0 means the workers decode
    /// their own files, exactly as before this stage existed.
    static func depth(
        workerCap: Int, environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> Int {
        let fallback = enabledByDefault ? automaticDepth(workerCap: workerCap) : 0
        guard let raw = environment["FILEID_DECODE_AHEAD"]?.trimmingCharacters(in: .whitespaces),
              let requested = Int(raw) else { return fallback }
        return requested <= 0 ? 0 : min(requested, maxDepth)
    }

    /// `FILEID_DECODE_AHEAD_MB` caps the decoded frames held ahead of the workers.
    static func budgetBytes(environment: [String: String] = ProcessInfo.processInfo.environment) -> Int {
        let megabytes = environment["FILEID_DECODE_AHEAD_MB"].flatMap { Int($0.trimmingCharacters(in: .whitespaces)) }
            .map { max(16, min(2048, $0)) } ?? defaultBudgetMB
        return megabytes << 20
    }

    /// The largest frame the bounded decode can produce (square, RGBA).
    static var frameEstimateBytes: Int {
        let side = Tagging.scanMaxPixels
        return side * side * 4
    }

    struct Summary: Sendable, Equatable {
        var queue = DecodeAheadQueue<Tagging.PreparedFile>.Stats()
        /// Images handed to a worker already decoded.
        var decoded = 0
        /// Images a worker decoded itself because memory was short or other work had priority.
        var notAdmitted = 0
        /// Copies that took a twin's result: never decoded.
        var copies = 0
        /// Files that are not images.
        var other = 0
        var decodeSeconds = 0.0
    }

    private final class Tally: @unchecked Sendable {
        private let lock = NSLock()
        private var summary = Summary()

        func note(_ prepared: Tagging.PreparedFile, admitted: Bool) {
            lock.lock(); defer { lock.unlock() }
            switch prepared.stage {
            case .whole: summary.other += 1
            case .image(.done, _): summary.copies += 1
            case .image(.analyze, let decoded):
                if decoded != nil { summary.decoded += 1 } else if !admitted { summary.notAdmitted += 1 }
                summary.decodeSeconds += prepared.prepSeconds
            }
        }

        func result(_ queue: DecodeAheadQueue<Tagging.PreparedFile>.Stats) -> Summary {
            lock.lock(); defer { lock.unlock() }
            var out = summary
            out.queue = queue
            return out
        }
    }

    private static func waitWhilePaused(
        isPaused: @Sendable () -> Bool, isCancelled: @Sendable () -> Bool
    ) async {
        while isPaused(), !isCancelled(), !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 200_000_000)
        }
    }

    /// Runs the tagging stage of a scan with decode-ahead: `source` → decoders
    /// → bounded queue → `workers` inference loops → `emit`.
    ///
    /// `admit(bytes)` says whether one more decoded frame may be held now;
    /// when it says no the file is still hashed here and the worker decodes
    /// it. `process` runs one prepared file through a worker; nil stops that
    /// worker (cancelled scan). Twin waits happen in the decoders, never in
    /// a worker, so workers cannot all wait on a frame that sits in the queue.
    static func run(
        source: AsyncChannel<DiscoveredFile>,
        depth: Int,
        budgetBytes: Int,
        workers: Int,
        twins: TwinRegistry?,
        catalog: CatalogTwins?,
        isCancelled: @escaping @Sendable () -> Bool,
        isPaused: @escaping @Sendable () -> Bool,
        admit: @escaping @Sendable (Int) async -> Bool,
        process: @escaping @Sendable (Tagging.PreparedFile) async -> TaggedFile?,
        emit: @escaping @Sendable (TaggedFile) async -> Void
    ) async -> Summary {
        let queue = DecodeAheadQueue<Tagging.PreparedFile>(capacity: depth, budgetBytes: budgetBytes) {
            Tagging.discard($0, twins: twins)
        }
        let tally = Tally()
        let estimate = frameEstimateBytes
        await withTaskGroup(of: Void.self) { group in
            group.addTask {
                await withTaskGroup(of: Void.self) { decoders in
                    // An idle worker adds room for one more decode, so up to
                    // depth + workers reservations can be open at once.
                    for _ in 0..<(max(1, depth) + max(1, workers)) {
                        decoders.addTask {
                            for await file in source {
                                if isCancelled() { break }
                                await waitWhilePaused(isPaused: isPaused, isCancelled: isCancelled)
                                if isCancelled() || Task.isCancelled { break }
                                let isImage = file.kind == .image && !file.movedFromKnownRow
                                guard let ticket = await queue.reserve(bytes: isImage ? estimate : 0) else { break }
                                let admitted = isImage ? await admit(estimate) : false
                                let prepared = await Tagging.prepare(
                                    discovered: file, twins: twins, catalog: catalog, decode: admitted)
                                tally.note(prepared, admitted: admitted)
                                queue.fill(ticket, prepared, bytes: prepared.decodedBytes)
                            }
                        }
                    }
                }
                queue.finish()
            }
            group.addTask {
                await withTaskGroup(of: Void.self) { inference in
                    for _ in 0..<max(1, workers) {
                        inference.addTask {
                            while !isCancelled() {
                                await waitWhilePaused(isPaused: isPaused, isCancelled: isCancelled)
                                if isCancelled() || Task.isCancelled { break }
                                guard let prepared = await queue.next() else { break }
                                if let copy = prepared.finishedCopy {
                                    await emit(copy)
                                    continue
                                }
                                guard let tagged = await process(prepared) else {
                                    Tagging.discard(prepared, twins: twins)
                                    break
                                }
                                await emit(tagged)
                            }
                        }
                    }
                }
                // Every worker has stopped: nothing will take what the decoders hold.
                queue.cancel()
            }
        }
        return tally.result(queue.stats)
    }
}
