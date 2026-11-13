// Whether a model's weights are on disk (the Hugging Face cache layout).
import Foundation

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
