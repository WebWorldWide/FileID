// Which engine the app starts: the shared Rust engine (`Contents/MacOS/fileidd`)
// or the Swift engine (`Contents/MacOS/FileIDEngine`), and when to fall back.
import Foundation

public enum EngineBackend: String, CaseIterable, Sendable {
    case rust
    case swift

    public var binaryName: String {
        switch self {
        case .rust: "fileidd"
        case .swift: "FileIDEngine"
        }
    }
}

public enum EngineBackendPolicy {
    /// `defaults write com.fileid.app engine.backend swift|rust`; Settings ▸ Engine writes it too.
    public static let defaultsKey = "engine.backend"
    /// Overrides the setting for one launch (`FILEID_ENGINE_BACKEND=swift`).
    public static let environmentKey = "FILEID_ENGINE_BACKEND"

    /// The App Store gate, and the only one: builds compiled with `-DFILEID_APP_STORE`
    /// (scripts/testflight.sh) run the Swift engine only, whatever the setting says,
    /// until the owner accepts the Rust engine in the sandbox (CONVERGENCE.md Stage 10).
    /// `assemble_app.sh` does not bundle `fileidd` for those builds.
    public static var isAppStoreBuild: Bool {
        #if FILEID_APP_STORE
        true
        #else
        false
        #endif
    }

    /// The backend the environment or the setting names; nil means the build default.
    public static func explicitChoice(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        defaults: UserDefaults = .standard
    ) -> EngineBackend? {
        let raw = environment[environmentKey].flatMap { $0.isEmpty ? nil : $0 }
            ?? defaults.string(forKey: defaultsKey)
        return raw.flatMap { EngineBackend(rawValue: $0.lowercased()) }
    }

    /// The backend this build starts first: the explicit choice, else Rust when it is bundled.
    public static func preferred(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        defaults: UserDefaults = .standard,
        appStore: Bool = isAppStoreBuild,
        rustBundled: Bool
    ) -> EngineBackend {
        if appStore { return .swift }
        return explicitChoice(environment: environment, defaults: defaults) ?? (rustBundled ? .rust : .swift)
    }

    /// Engines to try, in order. Rust is followed by Swift; Swift never falls forward.
    /// Engines `demoted` this session are skipped, except Swift, the last resort.
    public static func candidates(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        defaults: UserDefaults = .standard,
        appStore: Bool = isAppStoreBuild,
        rustBundled: Bool,
        demoted: Set<EngineBackend> = []
    ) -> [EngineBackend] {
        let first = preferred(environment: environment, defaults: defaults, appStore: appStore, rustBundled: rustBundled)
        let order: [EngineBackend] = first == .rust ? [.rust, .swift] : [.swift]
        return order.filter { $0 == .swift || !demoted.contains($0) }
    }
}

/// Session-only record of a Rust engine that failed to start. Swift is never demoted.
/// Nothing is persisted: the next launch tries the preferred engine again.
public struct EngineFallback: Sendable, Equatable {
    /// Exits before `ready` that demote an engine (a crash loop at startup).
    public static let earlyExitLimit = 2

    public private(set) var demoted: Set<EngineBackend> = []
    public private(set) var note: String?
    private var earlyExits: [EngineBackend: Int] = [:]

    public init() {}

    /// The binary is missing, fails the signing check, or can't be launched.
    /// True when this demotes the engine.
    @discardableResult
    public mutating func launchFailed(_ backend: EngineBackend, reason: String) -> Bool {
        demote(backend, because: reason)
    }

    /// The engine exited without sending `ready`. True when this demotes it.
    @discardableResult
    public mutating func exitedBeforeReady(_ backend: EngineBackend) -> Bool {
        let count = (earlyExits[backend] ?? 0) + 1
        earlyExits[backend] = count
        guard count >= Self.earlyExitLimit else { return false }
        return demote(backend, because: "it stopped \(count) times while starting")
    }

    public mutating func reachedReady(_ backend: EngineBackend) {
        earlyExits[backend] = 0
    }

    /// The engine used up its respawn budget. True when this demotes it.
    @discardableResult
    public mutating func respawnsExhausted(_ backend: EngineBackend) -> Bool {
        demote(backend, because: "it kept stopping")
    }

    private mutating func demote(_ backend: EngineBackend, because reason: String) -> Bool {
        guard backend != .swift, !demoted.contains(backend) else { return false }
        demoted.insert(backend)
        note = "The shared engine didn't start (\(reason)), so FileID is using the Swift engine until you quit."
        return true
    }
}
