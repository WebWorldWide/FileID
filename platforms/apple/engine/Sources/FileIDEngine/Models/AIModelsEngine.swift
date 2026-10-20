// Engine-only extensions on the shared AIModelKind. These rely on
// `Hardware` (engine-side) and the local filesystem (HuggingFace cache),
// which the app process doesn't have direct access to.
import Foundation
import FileIDShared

extension AIModelKind {
    /// Heuristic: weights live in MLX's HuggingFace hub cache. We treat
    /// the existence of `config.json` under that path as "downloaded".
    public func isInstalledOnDisk() -> Bool {
        let cache = Self.huggingFaceCacheDir()
            .appendingPathComponent(sourceRepo, isDirectory: true)
        return FileManager.default.fileExists(
            atPath: cache.appendingPathComponent("config.json").path
        )
    }

    private static func huggingFaceCacheDir() -> URL {
        ModelCachePaths.huggingFaceModels
    }
}
