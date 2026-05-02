// Defensive helper for the Application Support directory.
//
// Every call site that reaches for FileManager.default.urls(for:
// .applicationSupportDirectory, in: .userDomainMask).first! force-
// unwraps the array. macOS guarantees the directory exists in
// practice, but a sandboxed-then-broken environment crashes the
// whole app on a render path. Replace with a single helper that
// degrades gracefully to `~/tmp` instead of crashing.
import Foundation

enum AppSupportPath {
    /// `~/Library/Application Support` for the current user, with a
    /// safe `temporaryDirectory` fallback if the lookup ever returns
    /// an empty array (theoretical, but defensive — a single force-
    /// unwrap deep in a SwiftUI render body would otherwise tear down
    /// the whole app).
    static var root: URL {
        FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first
            ?? FileManager.default.temporaryDirectory
    }

    /// `~/Library/Application Support/FileID/` — the app's per-user
    /// data root. Wraps the directory creation so callers don't have
    /// to repeat the boilerplate.
    static var fileID: URL {
        let url = root.appendingPathComponent("FileID", isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    /// `~/Library/Application Support/FileID/Models/` — where every
    /// downloaded ML model lives.
    static var models: URL {
        let url = fileID.appendingPathComponent("Models", isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }
}
