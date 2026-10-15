import Foundation

/// Where the engine keeps its own state (logs, journals, plans, face crops).
/// Normally `~/Library/Application Support/FileID`; when the database path is
/// configured explicitly (tests, fixtures, isolated runs) state stays beside
/// that database so an isolated engine never touches the user's real library.
enum EngineStateRoot {
    static var directory: URL {
        if let configured = ProcessInfo.processInfo.environment["FILEID_DATABASE_PATH"], !configured.isEmpty {
            return URL(fileURLWithPath: configured).deletingLastPathComponent()
        }
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID", isDirectory: true)
    }

    static var logsDirectory: URL { directory.appendingPathComponent("logs", isDirectory: true) }
}
