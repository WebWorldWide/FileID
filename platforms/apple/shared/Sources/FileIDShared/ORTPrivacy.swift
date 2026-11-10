import Foundation
import OnnxRuntimeBindings

/// Keeps ONNX Runtime from collecting or sending telemetry in this process.
///
/// Official ONNX Runtime builds from 1.29 on carry Microsoft's 1DS uploader on
/// macOS. It starts with the runtime's first environment, writes a device id and
/// an event queue under ~/Library/Application Support/Microsoft/DeveloperTools/
/// .onnxruntime and uploads from there; `DisableTelemetryEvents` comes too late
/// to stop either. Only `ORT_DISABLE_TELEMETRY=1`, read before that first
/// environment, keeps it from starting. The pinned 1.24.2 framework has no
/// uploader; this keeps a package update from adding one silently.
public enum ORTPrivacy {
    public static let disableTelemetryVariable = "ORT_DISABLE_TELEMETRY"

    /// Call first thing in `main` (and before spawning helpers), while the
    /// process is single-threaded.
    public static func disableTelemetryBeforeLoad() {
        guard getenv(disableTelemetryVariable).map({ String(cString: $0) }) != "1" else { return }
        setenv(disableTelemetryVariable, "1", 1)
    }

    /// The only way FileID creates an ORT environment.
    public static func makeEnv(loggingLevel: ORTLoggingLevel) throws -> ORTEnv {
        disableTelemetryBeforeLoad()
        let env = try ORTEnv(loggingLevel: loggingLevel)
        disableTelemetryEvents()
        return env
    }

    /// Calls the C API's `DisableTelemetryEvents`, which the Objective-C
    /// bindings don't expose. It acts on the process-wide provider and ignores
    /// its `OrtEnv` argument. Returns false when the runtime isn't linked.
    @discardableResult
    public static func disableTelemetryEvents() -> Bool {
        typealias GetApiBase = @convention(c) () -> UnsafeRawPointer?
        typealias GetApi = @convention(c) (UInt32) -> UnsafeRawPointer?
        typealias DisableTelemetry = @convention(c) (OpaquePointer?) -> OpaquePointer?
        let defaultHandle = UnsafeMutableRawPointer(bitPattern: -2)
        guard let symbol = dlsym(defaultHandle, "OrtGetApiBase"),
              let base = unsafeBitCast(symbol, to: GetApiBase.self)(),
              let api = base.load(as: GetApi.self)(1)
        else { return false }
        // OrtApi only grows at the end; DisableTelemetryEvents has been entry 6
        // since API version 1.
        let disable = api.load(fromByteOffset: 6 * MemoryLayout<UnsafeRawPointer>.stride, as: DisableTelemetry.self)
        return disable(nil) == nil
    }
}
