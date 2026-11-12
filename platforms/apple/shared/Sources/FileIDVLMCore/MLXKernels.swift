import Foundation
import Metal

/// Whether MLX can load its kernels. MLX's first call of any kind (even
/// `Memory.cacheLimit`) loads a metallib and exits the process, not a Swift
/// error, when it finds none, so model loads check this first.
public enum MLXKernels {
    /// A Metal device plus a metallib where MLX's `load_default_library`
    /// (mlx/backend/metal/device.cpp) looks: beside the binary that contains
    /// MLX (this one: the engine, or libFileIDAppleKit.dylib), its Resources,
    /// the SwiftPM `mlx-swift_Cmlx` bundle, then `default.metallib` in the
    /// working directory.
    public static let available: Bool = {
        guard MTLCreateSystemDefaultDevice() != nil else { return false }
        var info = Dl_info()
        let image = dladdr(#dsohandle, &info) != 0 && info.dli_fname != nil
            ? URL(fileURLWithPath: String(cString: info.dli_fname))
            : (Bundle.main.executableURL ?? URL(fileURLWithPath: CommandLine.arguments[0]))
        let folder = image.resolvingSymlinksInPath().deletingLastPathComponent()
        var candidates = ["mlx.metallib", "Resources/mlx.metallib", "Resources/default.metallib"]
            .map { folder.appendingPathComponent($0) }
        candidates.append(URL(fileURLWithPath: "default.metallib"))
        for parent in [Bundle.main.bundleURL] + Bundle.allBundles.compactMap(\.resourceURL) {
            if let resources = Bundle(url: parent.appendingPathComponent("mlx-swift_Cmlx.bundle"))?.resourceURL {
                candidates.append(resources.appendingPathComponent("default.metallib"))
            }
        }
        return candidates.contains { FileManager.default.fileExists(atPath: $0.path) }
    }()
}

public struct MLXKernelsUnavailable: LocalizedError {
    public init() {}
    public var errorDescription: String? {
        "This FileID build can't run local AI models: the MLX GPU library (mlx.metallib) wasn't found next to the engine, or this Mac has no Metal GPU. The model files are installed; reinstall FileID to use them."
    }
}
