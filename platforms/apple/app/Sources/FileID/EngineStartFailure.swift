// The one engine the app runs: the shared Rust engine, bundled as `Contents/MacOS/fileidd`.
// When it can't start, the app says so (EngineUnavailableCard); it never starts another engine.
import Foundation

public enum EngineBinary {
    public static let name = "fileidd"
}

/// Why the engine isn't running, as the one sentence the unavailable card shows.
public struct EngineStartFailure: Sendable, Equatable {
    public enum Cause: Sendable, Equatable {
        case missing
        case signatureMismatch
        case couldNotLaunch
        case exitsAtStart
        case keepsStopping
    }

    public let cause: Cause

    public init(_ cause: Cause) { self.cause = cause }

    public var summary: String {
        switch cause {
        case .missing: "FileID’s helper file is missing from this copy of FileID."
        case .signatureMismatch: "FileID’s helper doesn’t match this copy of the app, so FileID won’t run it."
        case .couldNotLaunch: "macOS wouldn’t let FileID start its helper."
        case .exitsAtStart: "FileID’s helper stopped as soon as it started."
        case .keepsStopping: "FileID’s helper keeps stopping."
        }
    }

    /// Reinstalling helps when the file is missing or altered, not when the engine ran and stopped.
    public var suggestsReinstall: Bool {
        switch cause {
        case .missing, .signatureMismatch, .couldNotLaunch: true
        case .exitsAtStart, .keepsStopping: false
        }
    }
}

/// Exits before `ready`: a crash loop at startup, which the respawn backoff would otherwise
/// keep retrying for half a minute.
public struct EngineStartupExits: Sendable, Equatable {
    public static let limit = 2

    private var count = 0

    public init() {}

    /// True once the engine has stopped `limit` times without reaching `ready`.
    public mutating func exitedBeforeReady() -> Bool {
        count += 1
        return count >= Self.limit
    }

    public mutating func reachedReady() { count = 0 }
}
