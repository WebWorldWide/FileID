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
        public let id: String
        public let category: JobCategory
        public let title: String
        public let etaSeconds: Double?
        public let run: @Sendable () async -> Void

        public init(id: String = UUID().uuidString,
                    category: JobCategory,
                    title: String,
                    etaSeconds: Double?,
                    run: @escaping @Sendable () async -> Void) {
            self.id = id
            self.category = category
            self.title = title
            self.etaSeconds = etaSeconds
            self.run = run
        }
    }

    private var pending: [Job] = []
    private var running: Job?
    private var drainerStarted = false
    private weak var sink: IPCSink?

    private init() {}

    public func attachSink(_ s: IPCSink) {
        self.sink = s
    }

    /// Add a job. Starts the drainer the first time anything's enqueued.
    /// Emits a queueState event so the UI sees the new entry immediately.
    public func enqueue(_ job: Job) async {
        pending.append(job)
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
            let next: Job? = await {
                if pending.isEmpty {
                    return nil
                }
                let j = pending.removeFirst()
                running = j
                return j
            }()
            guard let job = next else {
                // Nothing to do — sleep briefly + recheck. The next
                // enqueue() doesn't actively wake us, but the cost of a
                // 250 ms tick when the queue is empty is negligible
                // and avoids a continuation-based wake/notify dance.
                try? await Task.sleep(nanoseconds: 250_000_000)
                // If still nothing AND drainer hasn't been re-armed,
                // we'd loop forever — but enqueue() always sets pending
                // before returning, so on the next iteration we pick up.
                continue
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
