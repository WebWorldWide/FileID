import Foundation
import OnnxRuntimeBindings

/// One process-wide ORT environment and the CoreML-EP session builder the ONNX
/// services share.
enum ORTSessions {
    private static let lock = NSLock()
    nonisolated(unsafe) private static var sharedEnv: ORTEnv?

    static func env() throws -> ORTEnv {
        try lock.withLock {
            if let sharedEnv { return sharedEnv }
            let env = try ORTEnv(loggingLevel: ORTLoggingLevel.warning)
            sharedEnv = env
            return env
        }
    }

    enum CoreMLBinding { case required, optional, disabled }

    /// Keeps a model's compiled CoreML program between runs. Without it the
    /// CoreML provider recompiles on every engine start and leaves each
    /// compilation in the temporary folder when the process exits.
    struct CoreMLCache {
        /// SHA-256 of the verified model file, so a changed file never reuses
        /// another file's compiled program.
        let modelDigest: String
        let mlProgram: Bool
        let subgraphs: Bool
        var cpuAndGPU = false

        var directory: URL {
            let os = ProcessInfo.processInfo.operatingSystemVersion
            let runtime = "ort\(ORTVersion() ?? "unknown")-macos\(os.majorVersion).\(os.minorVersion).\(os.patchVersion)"
            let format = (mlProgram ? "mlprogram" : "neuralnetwork") + (subgraphs ? "-sub" : "") + (cpuAndGPU ? "-gpu" : "")
            return EngineStateRoot.directory
                .appendingPathComponent("ModelCache/CoreML", isDirectory: true)
                .appendingPathComponent("\(modelDigest.prefix(32))-\(format)-\(runtime)", isDirectory: true)
        }

        var providerOptions: [String: String] {
            var options = ["ModelFormat": mlProgram ? "MLProgram" : "NeuralNetwork",
                           "EnableOnSubgraphs": subgraphs ? "1" : "0",
                           "ModelCacheDirectory": directory.path]
            if cpuAndGPU { options["MLComputeUnits"] = "CPUAndGPU" }
            return options
        }
    }

    /// Returns the session and whether the CoreML EP bound. With `.optional`, a
    /// CoreML bind failure leaves ORT's implicit CPU EP in place.
    static func make(
        modelPath: String,
        coreML binding: CoreMLBinding = .optional,
        cache: CoreMLCache? = nil,
        configure: (ORTCoreMLExecutionProviderOptions) -> Void = { _ in }
    ) throws -> (session: ORTSession, coreML: Bool, bindError: Error?) {
        // The CoreML EP aborts the process if it is appended before an env exists.
        let env = try env()
        let options = try ORTSessionOptions()
        var bound = false
        var bindError: Error?
        if binding != .disabled {
            do {
                if let cache, (try? FileManager.default.createDirectory(at: cache.directory, withIntermediateDirectories: true)) != nil {
                    try options.appendCoreMLExecutionProvider(withOptionsV2: cache.providerOptions)
                } else {
                    let coreML = ORTCoreMLExecutionProviderOptions()
                    configure(coreML)
                    try options.appendCoreMLExecutionProvider(with: coreML)
                }
                bound = true
            } catch {
                if binding == .required { throw error }
                bindError = error
            }
        }
        let session = try ORTSession(env: env, modelPath: modelPath, sessionOptions: options)
        return (session, bound, bindError)
    }
}
