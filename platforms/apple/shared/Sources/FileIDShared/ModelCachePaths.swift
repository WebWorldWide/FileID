import Foundation

public enum ModelCachePaths {
    public static var huggingFaceRoot: URL {
        #if FILEID_APP_STORE
        (FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory)
            .appendingPathComponent("FileID/Models/huggingface", isDirectory: true)
        #else
        (FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory)
            .appendingPathComponent("huggingface", isDirectory: true)
        #endif
    }

    public static var huggingFaceModels: URL {
        huggingFaceRoot.appendingPathComponent("models", isDirectory: true)
    }
}
