// Under the shared Rust engine (`engine.backend` = rust) the engine installs the
// ONNX models through `prewarmModel`, as it does on Windows and Linux, into the
// same files the app's own installers write; with the Swift engine the app keeps
// downloading them itself. The files on disk stay the only install state, so
// each installer's `refreshStatus()` is unchanged.
import Foundation
import FileIDShared

@MainActor
enum EngineModelInstall {
    enum Outcome: Equatable {
        case finished
        case cancelled
        case failed(String)
    }

    static weak var engine: EngineClient?

    /// True while the running engine is the Rust engine.
    static var active: Bool {
        guard let engine, case .ready(let info) = engine.state else { return false }
        return info.engine == "rust"
    }

    /// Requests every kind and follows them until all `files` are on disk, the
    /// engine reports an error for one of them, or the task is cancelled.
    static func install(
        kinds: [String],
        files: [URL],
        progress: @escaping @MainActor (Double, String) -> Void
    ) async -> Outcome {
        guard let engine else { return .failed(notResponding) }
        var errorSignal = engine.lastErrorSignal
        for kind in kinds where !engine.send(.prewarmModel(modelKind: kind)) {
            return .failed(notResponding)
        }
        var fractions = Dictionary(uniqueKeysWithValues: kinds.map { ($0, 0.0) })
        while true {
            if Task.isCancelled {
                for kind in kinds {
                    _ = engine.send(.cancelPrewarm(modelKind: kind))
                    engine.clearModelDownloadProgress(forModelKind: kind)
                }
                return .cancelled
            }
            try? await Task.sleep(for: .milliseconds(250))
            guard case .ready = engine.state else { return .failed(notResponding) }
            if engine.lastErrorSignal != errorSignal {
                errorSignal = engine.lastErrorSignal
                if let error = engine.lastError, let kind = error.modelKind, kinds.contains(kind) {
                    return error.kind == "prewarm_cancelled" ? .cancelled : .failed(error.message)
                }
            }
            if let current = engine.modelDownloadProgress, fractions[current.modelKind] != nil {
                fractions[current.modelKind] = current.fraction
                progress(fractions.values.reduce(0, +) / Double(kinds.count), current.message)
            } else if files.allSatisfy({ FileManager.default.fileExists(atPath: $0.path) }) {
                return .finished
            }
        }
    }

    static let notResponding = "FileID isn't responding. Quit and reopen FileID, then try again."
}
