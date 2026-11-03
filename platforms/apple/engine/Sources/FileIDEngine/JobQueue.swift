// JobQueue — serializes long-running engine jobs (scan, face cluster,
// Deep Analyze) so the user can fire-and-forget without remembering
// to wait. Each enqueue returns immediately; jobs run one at a time.
//
// Why an actor: only one drainer task pulls from the queue. Mutations
// are async-safe by construction.
//
// Cancellation: each job's `run` closure is responsible for honoring
// the existing per-feature cancel signals (ScanCoordinator's mirrors,
// DeepAnalyze.requestCancel). The queue itself can also be cancelled
// at the queued level via `cancelPending(id:)`.
import Foundation
import FileIDShared

public actor JobQueue {
    public static let shared = JobQueue()

    public struct Job: Sendable {
        public enum Priority: Int, Sendable { case background = 0, interactive = 1 }
        public let id: String
        public let category: JobCategory
        public let title: String
        public let etaSeconds: Double?
        public let priority: Priority
        /// False for short interactive work (a chat summary) that must not make a
        /// Describe/scan command answer `already_running`; the Rust engine's chat
        /// bypasses its mutation gate the same way.
        public let blocksExclusiveCommands: Bool
        public let run: @Sendable () async -> Void

        public init(id: String = UUID().uuidString,
                    category: JobCategory,
                    title: String,
                    etaSeconds: Double?,
                    priority: Priority = .background,
                    blocksExclusiveCommands: Bool = true,
                    run: @escaping @Sendable () async -> Void) {
            self.id = id
            self.category = category
            self.title = title
            self.etaSeconds = etaSeconds
            self.priority = priority
            self.blocksExclusiveCommands = blocksExclusiveCommands
            self.run = run
        }
    }

    private var pending: [Job] = []
    private var running: Job?
    private var drainerStarted = false
    private weak var sink: IPCSink?
    init() {}

    public func attachSink(_ s: IPCSink) {
        self.sink = s
    }

    public func enqueue(_ job: Job) async {
        let position = pending.firstIndex { $0.priority.rawValue < job.priority.rawValue } ?? pending.endIndex
        pending.insert(job, at: position)
        await emitState()
        startDrainerIfNeeded()
    }

    /// Cancel a queued (not-yet-running) job. The currently-running job
    /// is cancelled via its category-specific channel (not here).
    public func cancelPending(id: String) async {
        pending.removeAll { $0.id == id }
        await emitState()
    }

    public func snapshot() -> QueueState {
        QueueState(
            running: running.map { Self.toQueuedJob($0) },
            pending: pending.map { Self.toQueuedJob($0) },
            totalEtaSeconds: totalEta()
        )
    }

    /// True if a job of `category` is running or queued. Used to reject
    /// duplicate deep-analyze commands with the same
    /// deep_analyze_already_running error the Windows engine emits —
    /// the IPC contract must answer identically on both platforms.
    public func hasActive(category: JobCategory) -> Bool {
        if let running, running.category == category, running.blocksExclusiveCommands { return true }
        return pending.contains { $0.category == category && $0.blocksExclusiveCommands }
    }

    /// Waits up to `timeout` for no job of `category` to be running or queued. A job that has
    /// just emitted its terminal event still holds its slot for a moment, so a client that
    /// reacts instantly (a script, "Scan again") would otherwise be told it is still busy.
    public func waitUntilIdle(category: JobCategory, timeout: Duration = .seconds(1)) async -> Bool {
        let deadline = ContinuousClock.now + timeout
        while hasActive(category: category) {
            if ContinuousClock.now >= deadline { return false }
            try? await Task.sleep(for: .milliseconds(25))
        }
        return true
    }

    // MARK: - Internals

    private func startDrainerIfNeeded() {
        guard !drainerStarted else { return }
        drainerStarted = true
        Task.detached(priority: .userInitiated) { [weak self] in
            await self?.drain()
        }
    }

    private func drain() async {
        while true {
            // Pop next job under actor isolation.
            let next: Job? = {
                if pending.isEmpty {
                    return nil
                }
                let j = pending.removeFirst()
                running = j
                return j
            }()
            guard let job = next else {
                drainerStarted = false
                return
            }
            await emitState()
            JSONLog.shared.info(ev: "job_start",
                                extra: ["id": AnyCodable(job.id),
                                        "category": AnyCodable(job.category.rawValue),
                                        "title": AnyCodable(job.title)])
            await job.run()
            JSONLog.shared.info(ev: "job_done",
                                extra: ["id": AnyCodable(job.id),
                                        "category": AnyCodable(job.category.rawValue)])
            running = nil
            await emitState()
        }
    }

    private func emitState() async {
        guard let sink else { return }
        await sink.emit(.queueState(snapshot()))
    }

    private func totalEta() -> Double? {
        let parts: [Double] = ([running].compactMap { $0?.etaSeconds })
            + pending.compactMap { $0.etaSeconds }
        guard !parts.isEmpty else { return nil }
        return parts.reduce(0, +)
    }

    private static func toQueuedJob(_ j: Job) -> QueuedJob {
        QueuedJob(id: j.id, category: j.category,
                   title: j.title, etaSeconds: j.etaSeconds)
    }
}
