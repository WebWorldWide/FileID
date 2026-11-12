import FileIDVLMCore
import Foundation

/// Gives FileIDVLMCore (the MLX code this engine shares with FileIDAppleKit)
/// the engine's log, scheduler, downloader and state folder. Called once at
/// launch, before any model work.
enum VLMCoreHostSetup {
    static func install() {
        VLMCoreHost.install(
            log: { level, event, path, error, extra in
                switch level {
                case "error": JSONLog.shared.error(ev: event, path: path, error: error)
                case "warn": JSONLog.shared.warn(ev: event, path: path, error: error)
                default: JSONLog.shared.info(ev: event, path: path, extra: extra?.mapValues { AnyCodable($0) })
                }
            },
            scheduler: EngineScheduler.shared,
            fetchRepo: { repo, documentsHF, progress in
                try await VLMDownloader.shared.fetchRepo(repo: repo, documentsHF: documentsHF, progress: progress)
            },
            stateDirectory: EngineStateRoot.directory
        )
    }
}

/// The engine's `ResourceScheduler` behind the core's numbered leases.
actor EngineScheduler: VLMCoreHost.Scheduler {
    static let shared = EngineScheduler()
    private var next: UInt64 = 0
    private var leases: [UInt64: ResourceScheduler.MemoryLease] = [:]

    func reserve(memoryMB: UInt64, cpuUnits: Int, ioUnits: Int, interactive: Bool) async throws -> UInt64 {
        let lease = try await ResourceScheduler.shared.reserve(
            ResourceScheduler.Demand(memoryMB: memoryMB, cpuUnits: cpuUnits, ioUnits: ioUnits),
            priority: interactive ? .interactive : .background)
        next &+= 1
        leases[next] = lease
        return next
    }

    func retainMemory(_ lease: UInt64) async {
        guard let held = leases[lease] else { return }
        await ResourceScheduler.shared.retainMemory(held)
    }

    func release(_ lease: UInt64) async {
        guard let held = leases.removeValue(forKey: lease) else { return }
        await ResourceScheduler.shared.release(held)
    }
}
