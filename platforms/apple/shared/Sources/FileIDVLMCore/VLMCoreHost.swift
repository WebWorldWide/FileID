// What the host engine provides to the shared MLX code. The Swift engine
// installs its scheduler, log, downloader and state folder at launch
// (`VLMCoreHost.install`); FileIDAppleKit installs the Rust host's offline
// model check. Defaults: JSON lines on stderr, an in-process memory ledger, no
// downloads. Nothing here opens a URL, socket or HTTP client.
import FileIDModelInfo
import Foundation

package enum VLMCoreHost {
    package typealias Log = @Sendable (_ level: String, _ event: String, _ path: String?, _ error: String?, _ extra: [String: AnyCodable]?) -> Void

    /// The engine's memory and compute authority.
    package protocol Scheduler: Sendable {
        func reserve(memoryMB: UInt64, cpuUnits: Int, ioUnits: Int, interactive: Bool) async throws -> UInt64
        func retainMemory(_ lease: UInt64) async
        func release(_ lease: UInt64) async
    }

    /// Makes `repo`'s pinned files available under `documentsHF/models/<repo>`,
    /// or throws. The Swift engine downloads; the Rust host only verifies.
    package typealias FetchRepo = @Sendable (_ repo: String, _ documentsHF: URL, _ progress: @escaping @Sendable (Double, Int64, Int64) -> Void) async throws -> Void

    private static let lock = NSLock()
    nonisolated(unsafe) private static var log: Log?
    nonisolated(unsafe) private static var scheduler: any Scheduler = MemoryLedger.shared
    nonisolated(unsafe) private static var fetch: FetchRepo?
    nonisolated(unsafe) private static var state: URL?

    package static func install(log: Log? = nil, scheduler: (any Scheduler)? = nil, fetchRepo: FetchRepo? = nil, stateDirectory: URL? = nil) {
        lock.lock(); defer { lock.unlock() }
        if let log { self.log = log }
        if let scheduler { self.scheduler = scheduler }
        if let fetchRepo { fetch = fetchRepo }
        if let stateDirectory { state = stateDirectory }
    }

    static func emit(_ level: String, _ event: String, path: String?, error: String?, extra: [String: AnyCodable]?) {
        let route = lock.withLock { log }
        if let route { return route(level, event, path, error, extra) }
        StderrLog.write(level: level, event: event, path: path, error: error, extra: extra)
    }

    static var currentScheduler: any Scheduler { lock.withLock { scheduler } }
    static var fetchRepo: FetchRepo? { lock.withLock { fetch } }
    static var stateDirectory: URL {
        lock.withLock { state } ?? FileManager.default.temporaryDirectory.appendingPathComponent("FileIDVLMCore", isDirectory: true)
    }

    /// Memory the default ledger holds for a loaded model, in MB.
    package static func retainedMB() async -> UInt64 { await MemoryLedger.shared.retainedMB }
}

package struct AnyCodable: Encodable, Sendable {
    private let encodeValue: @Sendable (Encoder) throws -> Void
    package init<T: Encodable & Sendable>(_ value: T) {
        encodeValue = { encoder in try value.encode(to: encoder) }
    }
    package func encode(to encoder: Encoder) throws { try encodeValue(encoder) }
}

private enum StderrLog {
    struct Entry: Encodable {
        let t: Date
        let src = "FileIDVLMCore"
        let lvl: String
        let ev: String
        let path: String?
        let error: String?
        let extra: [String: AnyCodable]?
    }

    static let lock = NSLock()
    static func write(level: String, event: String, path: String?, error: String?, extra: [String: AnyCodable]?) {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        guard var line = try? encoder.encode(Entry(t: Date(), lvl: level, ev: event, path: path, error: error, extra: extra)) else { return }
        line.append(0x0A)
        lock.withLock { FileHandle.standardError.write(line) }
    }
}

/// The engine log as the moved code calls it.
final class JSONLog: Sendable {
    static let shared = JSONLog()
    func info(ev: String, sess: String? = nil, path: String? = nil, extra: [String: AnyCodable]? = nil) {
        VLMCoreHost.emit("info", ev, path: path, error: nil, extra: extra)
    }
    func warn(ev: String, sess: String? = nil, path: String? = nil, error: String? = nil, extra: [String: AnyCodable]? = nil) {
        VLMCoreHost.emit("warn", ev, path: path, error: error, extra: extra)
    }
    func error(ev: String, sess: String? = nil, path: String? = nil, error: String? = nil, extra: [String: AnyCodable]? = nil) {
        VLMCoreHost.emit("error", ev, path: path, error: error, extra: extra)
    }
    func flush() {}
}

/// The engine scheduler as the moved code calls it; the host's `Scheduler`
/// decides.
actor ResourceScheduler {
    static let shared = ResourceScheduler()

    struct MemoryLease: Hashable, Sendable {
        let id: UInt64
    }

    struct Demand: Sendable, Equatable {
        let memoryMB: UInt64
        let cpuUnits: Int
        let ioUnits: Int
        init(memoryMB: UInt64 = 0, cpuUnits: Int = 0, ioUnits: Int = 0) {
            self.memoryMB = memoryMB
            self.cpuUnits = cpuUnits
            self.ioUnits = ioUnits
        }
    }

    enum Priority: Int, Sendable {
        case background
        case interactive
    }

    func reserve(_ demand: Demand, priority: Priority) async throws -> MemoryLease {
        let id = try await VLMCoreHost.currentScheduler.reserve(
            memoryMB: demand.memoryMB, cpuUnits: demand.cpuUnits, ioUnits: demand.ioUnits, interactive: priority == .interactive)
        return MemoryLease(id: id)
    }

    func retainMemory(_ lease: MemoryLease) async {
        await VLMCoreHost.currentScheduler.retainMemory(lease.id)
    }

    func release(_ lease: MemoryLease) async {
        await VLMCoreHost.currentScheduler.release(lease.id)
    }
}

/// The default scheduler: records what is reserved and retained so a host
/// that keeps memory itself (the Rust engine) can read it back.
actor MemoryLedger: VLMCoreHost.Scheduler {
    static let shared = MemoryLedger()
    private var next: UInt64 = 0
    private var reservations: [UInt64: UInt64] = [:]
    private var retained: Set<UInt64> = []

    func reserve(memoryMB: UInt64, cpuUnits: Int, ioUnits: Int, interactive: Bool) async throws -> UInt64 {
        try Task.checkCancellation()
        next &+= 1
        reservations[next] = memoryMB
        return next
    }

    func retainMemory(_ lease: UInt64) { retained.insert(lease) }

    func release(_ lease: UInt64) {
        reservations[lease] = nil
        retained.remove(lease)
    }

    var retainedMB: UInt64 { retained.reduce(0) { $0 + (reservations[$1] ?? 0) } }
}

enum VLMDownloaderError: LocalizedError {
    case unavailable(String)
    var errorDescription: String? {
        switch self {
        case .unavailable(let reason): return reason
        }
    }
}

/// The engine downloader as the moved code calls it; the host's `FetchRepo`
/// decides.
final class VLMDownloader: Sendable {
    static let shared = VLMDownloader()
    func fetchRepo(repo: String, documentsHF: URL, progress: @escaping @Sendable (Double, Int64, Int64) -> Void) async throws {
        guard let fetch = VLMCoreHost.fetchRepo else {
            throw VLMDownloaderError.unavailable("This model isn't installed.")
        }
        try await fetch(repo, documentsHF, progress)
    }
}

enum EngineStateRoot {
    static var directory: URL { VLMCoreHost.stateDirectory }
}

/// The engine's file-kind namespace as the moved code names it. Public
/// because `DeepAnalyze.analyze` is; inside the Swift engine its own
/// `DiscoveredFile` shadows this one and `Kind` is the same `FileKind`.
public enum DiscoveredFile {
    public typealias Kind = FileKind
}
