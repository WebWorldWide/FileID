import Foundation

public enum ModelCachePaths {
    /// Where description models live: `~/Library/Application Support/FileID/Models/huggingface`.
    /// Developer ID installs that already downloaded models into the old
    /// `~/Documents/huggingface` keep using it (nothing is moved); new installs stay
    /// out of Documents so iCloud "Desktop & Documents" never uploads multi-GB weights.
    public static var huggingFaceRoot: URL {
        if let root = isolatedRoot(environment: ProcessInfo.processInfo.environment) { return root }
        return defaultRoot
    }

    private static let defaultRoot: URL = {
        let fm = FileManager.default
        let support = (fm.urls(for: .applicationSupportDirectory, in: .userDomainMask).first ?? fm.temporaryDirectory)
            .appendingPathComponent("FileID/Models/huggingface", isDirectory: true)
        #if FILEID_APP_STORE
        return support
        #else
        guard let documents = fm.urls(for: .documentDirectory, in: .userDomainMask).first else { return support }
        return resolvedRoot(applicationSupport: support, legacy: documents.appendingPathComponent("huggingface", isDirectory: true))
        #endif
    }()

    static func resolvedRoot(applicationSupport: URL, legacy: URL, fileManager: FileManager = .default) -> URL {
        let legacyModels = legacy.appendingPathComponent("models", isDirectory: true)
        let entries = (try? fileManager.contentsOfDirectory(atPath: legacyModels.path)) ?? []
        return entries.contains { !$0.hasPrefix(".") } ? legacy : applicationSupport
    }

    static func isolatedRoot(environment: [String: String]) -> URL? {
        guard let path = environment["FILEID_HF_CACHE_ROOT"], path.hasPrefix("/") else { return nil }
        return URL(fileURLWithPath: path, isDirectory: true).standardizedFileURL
    }

    public static var huggingFaceModels: URL {
        huggingFaceRoot.appendingPathComponent("models", isDirectory: true)
    }
}
