// Capability checks for Deep Analyze.
//
// MLX needs a precompiled mlx.metallib for GPU kernel load. run.sh
// copies it next to the engine binary inside FileID.app/Contents/MacOS/.
// If it's missing, MLX will SIGSEGV deep in kernel binding on the first
// VLM inference — opaque from the user's perspective. We probe for it
// at engine startup and surface a clear error instead.
import Foundation
import Metal

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

    /// True iff MLX can load its kernels: a Metal device exists and a metallib
    /// sits where MLX's `load_default_library` (mlx/backend/metal/device.cpp)
    /// looks, including the SwiftPM resource bundle. Without one, MLX's first
    /// call (even `Memory.cacheLimit`) exits the process, so the model-load
    /// path checks this first and throws `MLXKernelsUnavailable` instead.
    public static let mlxKernelsAvailable: Bool = {
        guard MTLCreateSystemDefaultDevice() != nil else { return false }
        let exeDir = (Bundle.main.executableURL ?? URL(fileURLWithPath: CommandLine.arguments[0]))
            .resolvingSymlinksInPath().deletingLastPathComponent()
        var candidates = [
            exeDir.appendingPathComponent("mlx.metallib"),
            exeDir.appendingPathComponent("Resources/mlx.metallib"),
            exeDir.appendingPathComponent("Resources/default.metallib"),
            URL(fileURLWithPath: "default.metallib"),
        ]
        for parent in [Bundle.main.bundleURL] + Bundle.allBundles.compactMap(\.resourceURL) {
            if let resources = Bundle(url: parent.appendingPathComponent("mlx-swift_Cmlx.bundle"))?.resourceURL {
                candidates.append(resources.appendingPathComponent("default.metallib"))
            }
        }
        return candidates.contains { FileManager.default.fileExists(atPath: $0.path) }
    }()
}

public struct MLXKernelsUnavailable: LocalizedError {
    public var errorDescription: String? {
        "This FileID build can't run local AI models: the MLX GPU library (mlx.metallib) wasn't found next to the engine, or this Mac has no Metal GPU. The model files are installed; reinstall FileID to use them."
    }
}
