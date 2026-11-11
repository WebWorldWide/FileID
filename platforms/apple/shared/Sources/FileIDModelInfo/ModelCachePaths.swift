import Foundation

public enum ModelCachePaths {
    public static var huggingFaceRoot: URL {
        if let root = isolatedRoot(environment: ProcessInfo.processInfo.environment) {
            return root
        }
#if FILEID_APP_STORE
        return (FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory)
            .appendingPathComponent("FileID/Models/huggingface", isDirectory: true)
        #else
        return (FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory)
            .appendingPathComponent("huggingface", isDirectory: true)
        #endif
    }

    static func isolatedRoot(environment: [String: String]) -> URL? {
        guard let path = environment["FILEID_HF_CACHE_ROOT"], path.hasPrefix("/") else {
            return nil
        }
        return URL(fileURLWithPath: path, isDirectory: true).standardizedFileURL
    }

    public static var huggingFaceModels: URL {
        huggingFaceRoot.appendingPathComponent("models", isDirectory: true)
    }
}
