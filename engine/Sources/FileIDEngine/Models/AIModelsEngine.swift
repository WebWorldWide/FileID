// Engine-only extensions on the shared AIModelKind. These rely on
// `Hardware` (engine-side) and the local filesystem (HuggingFace cache),
// which the app process doesn't have direct access to.
import Foundation
import FileIDShared

extension AIModelKind {
    /// Default to the user's previous pick (UserDefaults, written by the
    /// app's Settings tab), else the top recommendation for this hardware.
    public static func currentlyActive() -> AIModelKind {
        if let raw = UserDefaults.standard.string(forKey: "deepAnalyzeActiveModel"),
           let k = AIModelKind(rawValue: raw) {
            return k
        }
        let ram = Hardware.physicalMemoryGB
        return recommendedFor(ramGB: ram).first ?? .qwen2VL3B
    }

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
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first!
            .appendingPathComponent("huggingface/models", isDirectory: true)
    }
}
