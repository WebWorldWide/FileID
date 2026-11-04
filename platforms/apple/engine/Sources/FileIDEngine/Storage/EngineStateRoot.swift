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
        // Test processes must never write journals, plans or logs into the user's real state.
        if let testRoot { return testRoot }
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID", isDirectory: true)
    }

    /// A per-process temporary state folder when running inside a test bundle.
    static let testRoot: URL? = {
        // SwiftPM runs tests in `xctest` or `swiftpm-testing-helper` (Swift Testing), passing the
        // `.xctest` bundle as an argument; the app and engine never load XCTest.
        let process = ProcessInfo.processInfo
        let inTests = ["xctest", "swiftpm-testing-helper"].contains(process.processName)
            || CommandLine.arguments.contains { $0.contains(".xctest") }
            || process.environment["XCTestConfigurationFilePath"] != nil
            || Bundle.allBundles.contains { $0.bundlePath.hasSuffix(".xctest") }
            || NSClassFromString("XCTestCase") != nil
        guard inTests else { return nil }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDTestState-\(ProcessInfo.processInfo.processIdentifier)", isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }()

    static var logsDirectory: URL { directory.appendingPathComponent("logs", isDirectory: true) }
}
