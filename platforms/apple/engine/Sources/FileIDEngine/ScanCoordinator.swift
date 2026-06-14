// Owns one scan session. Holds the ID, phase, counters, and pause/cancel flags.
// All scan-state mutation happens here; readers observe via snapshot().
//
// Cancel + Pause flags are also mirrored in `nonisolated(unsafe) static` slots
// (with NSLock) so they're readable from sync contexts (e.g. inside the
// FileManager.enumerator loop) without an actor hop. Cross-actor reads of an
// `actor var` would require `await`, which the sync enumerator can't do.
import Foundation
import FileIDShared

public actor ScanCoordinator {

    // MARK: - Sync-readable cancel + pause mirrors
    //
    // The Discovery enumerator and the per-file worker loop are sync; they
    // can't `await coordinator.isCancelled` per file (actor hop per file =
    // 5-10 ms = a 60% throughput regression at our 150 files/s baseline).
    // Mirror cancel + pause in static slots with NSLock so any worker can
    // poll cheaply.
    private static let mirrorLock = NSLock()
    private nonisolated(unsafe) static var cancelMirror: Bool = false
    private nonisolated(unsafe) static var pauseMirror: Bool = false

    public nonisolated static func isCancelledSync() -> Bool {
        mirrorLock.lock(); defer { mirrorLock.unlock() }
        return cancelMirror
    }

    public nonisolated static func isPausedSync() -> Bool {
        mirrorLock.lock(); defer { mirrorLock.unlock() }
        return pauseMirror
    }

    private nonisolated static func setCancelMirror(_ value: Bool) {
        mirrorLock.lock(); defer { mirrorLock.unlock() }
        cancelMirror = value
    }

    private nonisolated static func setPauseMirror(_ value: Bool) {
        mirrorLock.lock(); defer { mirrorLock.unlock() }
        pauseMirror = value
    }

    public struct Session: Sendable {
        public let id: String
        public let rootDisplayPath: String
        public let startedAt: Date
        public var phase: ScanPhase
        public var totalFiles: Int          // 0 until discovery done
        public var discovered: Int
        public var processed: Int
        public var failed: Int
    }

    private(set) var current: Session?
    private var cancelled = false
    private var paused = false
    private var activeScanTask: Task<Void, Never>?
    private var activeRestructureTask: Task<Void, Never>?

    private var lastEmitAt: Date = .distantPast
    private var lastProcessedSnapshot: Int = 0
    private var lastSnapshotAt: Date = .distantPast
    private var rollingFilesPerSecond: Double = 0

    public init() {}

    public func startSession(rootDisplayPath: String) -> Session {
        let s = Session(
            id: UUID().uuidString,
            rootDisplayPath: rootDisplayPath,
            startedAt: Date(),
            phase: .discovering,
            totalFiles: 0,
            discovered: 0,
            processed: 0,
            failed: 0
        )
        current = s
        cancelled = false
        paused = false
        Self.setCancelMirror(false)            // reset for new session
        Self.setPauseMirror(false)
        lastProcessedSnapshot = 0
        lastSnapshotAt = Date()
        // B8: the engine runs many scans in one process, so the rolling rate
        // MUST reset per session — otherwise scan #2+ seeds its EMA from scan
        // #1's stale rate (the `== 0 ? instant : EMA` branch below takes the
        // EMA path immediately) and shows a wrong ETA until the average decays.
        rollingFilesPerSecond = 0
        return s
    }

    public func bumpDiscovered(to count: Int) {
        guard var s = current else { return }
        s.discovered = count
        current = s
    }

    public func setTotal(_ total: Int) {
        guard var s = current else { return }
        s.totalFiles = total
        s.phase = .tagging
        current = s
    }

    public func bumpProcessed() {
        guard var s = current else { return }
        s.processed += 1
        current = s
    }

    public func bumpFailed() {
        guard var s = current else { return }
        s.failed += 1
        current = s
    }

    public func setPhase(_ phase: ScanPhase) {
        guard var s = current else { return }
        s.phase = phase
        current = s
    }

    public func requestPause()  {
        paused = true
        Self.setPauseMirror(true)
    }
    public func requestResume() {
        paused = false
        Self.setPauseMirror(false)
    }
    public func requestCancel() {
        cancelled = true
        Self.setCancelMirror(true)             // visible to sync Discovery + workers
        // Setting the flag alone is NOT enough: the discovery producer can be
        // suspended inside `await discoveryChan.send(file)` on an UNBUFFERED
        // (rendezvous) channel after every worker has already broken out of
        // its `for await` on cancel — with no consumer, `send` parks forever,
        // the TaskGroup never returns, and the scan task (plus the whole
        // JobQueue) wedges. AsyncChannel's send/next are cancellation-aware,
        // so cancelling the task unblocks the suspended producer and lets the
        // group finish and emit scanComplete(.cancelled).
        activeScanTask?.cancel()
        // F-C6-013 wiring: cancelScan is the app's single "stop the current long
        // op" signal — also stop an in-flight restructure apply. The apply task
        // polls `Task.isCancelled` per move (Restructure.apply's default), so
        // cancelling the registered handle breaks its loop at the next boundary.
        // No stale-cancel risk: a fresh apply registers a new (un-cancelled)
        // handle, and only a subsequent requestCancel cancels it.
        activeRestructureTask?.cancel()
    }

    /// Track the in-flight scan task so the engine can await it on shutdown
    /// and not kill it mid-flight. Setting a new task replaces any stale one.
    public func setActiveScan(_ task: Task<Void, Never>) {
        activeScanTask = task
    }

    /// Track the in-flight restructure-apply task so `requestCancel` can stop it
    /// and the engine can await its terminal `restructureApplyResult` on
    /// shutdown rather than `_exit`-ing over it. Pass nil to clear on completion.
    public func setActiveRestructure(_ task: Task<Void, Never>?) {
        activeRestructureTask = task
    }

    /// Block until the active restructure apply (if any) completes — so a
    /// shutdown mid-apply still flushes the terminal result event.
    public func awaitActiveRestructure() async {
        await activeRestructureTask?.value
        activeRestructureTask = nil
    }

    /// True iff a scan task is currently registered AND not yet finished.
    /// Used by the dispatcher to reject re-entrant `startScan` commands.
    public var hasActiveScan: Bool {
        guard let task = activeScanTask else { return false }
        return !task.isCancelled
            && (current?.phase == .discovering
                || current?.phase == .tagging
                || current?.phase == .postScan)
    }

    /// Block until the active scan (if any) completes. Called from main()
    /// before exiting so the parent process gets the full event sequence.
    public func awaitActiveScan() async {
        await activeScanTask?.value
        activeScanTask = nil
    }

    public var isCancelled: Bool { cancelled }
    public var isPaused: Bool    { paused }

    public func snapshot() -> ScanProgress? {
        guard let s = current else { return nil }
        let now = Date()
        let dt = now.timeIntervalSince(lastSnapshotAt)
        if dt >= 1.0 {
            let delta = Double(s.processed - lastProcessedSnapshot)
            let instant = dt > 0 ? delta / dt : 0
            // 60 s rolling EMA; weight recent samples more heavily.
            rollingFilesPerSecond = rollingFilesPerSecond == 0
                ? instant
                : (0.7 * rollingFilesPerSecond) + (0.3 * instant)
            lastSnapshotAt = now
            lastProcessedSnapshot = s.processed
        }
        let eta: Double? = {
            guard s.totalFiles > 0, rollingFilesPerSecond > 0.01 else { return nil }
            let remaining = max(0, s.totalFiles - s.processed)
            return Double(remaining) / rollingFilesPerSecond
        }()
        return ScanProgress(
            sessionID: s.id,
            phase: s.phase,
            total: s.totalFiles,
            discovered: s.discovered,
            processed: s.processed,
            failed: s.failed,
            filesPerSecond: rollingFilesPerSecond,
            etaSeconds: eta,
            residentMB: Hardware.residentMB(),
            availableMB: Hardware.availableMemoryMB()
        )
    }
}
