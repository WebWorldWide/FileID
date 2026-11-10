import FileIDORTC
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

    /// Calls the C API's `DisableTelemetryEvents`; false if ONNX Runtime refused.
    @discardableResult
    public static func disableTelemetryEvents() -> Bool {
        fileid_ort_disable_telemetry_events()
    }
}
