// Capability checks for Deep Analyze.
//
// MLX needs a precompiled mlx.metallib for GPU kernel load. run.sh
// copies it next to the engine binary inside FileID.app/Contents/MacOS/.
// If it's missing, MLX will SIGSEGV deep in kernel binding on the first
// VLM inference — opaque from the user's perspective. We probe for it
// at engine startup and surface a clear error instead.
import Foundation

public enum DeepAnalyzeCapability {

    /// True iff a usable mlx.metallib (or default.metallib) sits next to
    /// the running executable. False means run.sh didn't / couldn't build
    /// it on this machine — UI should disable Deep Analyze.
    public static func metallibPresent() -> Bool {
        let exe = URL(fileURLWithPath: CommandLine.arguments[0])
        let dir = exe.deletingLastPathComponent()
        let candidates = ["mlx.metallib", "default.metallib"]
        for name in candidates {
            let p = dir.appendingPathComponent(name).path
            if FileManager.default.fileExists(atPath: p) { return true }
        }
        return false
    }
}
