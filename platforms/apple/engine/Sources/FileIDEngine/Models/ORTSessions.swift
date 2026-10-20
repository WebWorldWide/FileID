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

    /// Returns the session and whether the CoreML EP bound. With `.optional`, a
    /// CoreML bind failure leaves ORT's implicit CPU EP in place.
    static func make(
        modelPath: String,
        coreML binding: CoreMLBinding = .optional,
        configure: (ORTCoreMLExecutionProviderOptions) -> Void = { _ in }
    ) throws -> (session: ORTSession, coreML: Bool, bindError: Error?) {
        // The CoreML EP aborts the process if it is appended before an env exists.
        let env = try env()
        let options = try ORTSessionOptions()
        var bound = false
        var bindError: Error?
        if binding != .disabled {
            let coreML = ORTCoreMLExecutionProviderOptions()
            configure(coreML)
            do {
                try options.appendCoreMLExecutionProvider(with: coreML)
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
