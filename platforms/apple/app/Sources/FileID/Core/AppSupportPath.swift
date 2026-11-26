import Foundation
import FileIDShared

enum AppSupportPath {
    /// `~/Library/Application Support` for the current user, falling back to
    /// `temporaryDirectory` if the lookup returns nothing, so a render path never force-unwraps.
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
        if (try? ReadOnlyLocations.requireWritable(url)) != nil {
            try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        }
        return url
    }

    /// `~/Library/Application Support/FileID/Models/` — where every
    /// downloaded ML model lives.
    static var models: URL {
        let url = fileID.appendingPathComponent("Models", isDirectory: true)
        if (try? ReadOnlyLocations.requireWritable(url)) != nil {
            try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        }
        return url
    }
}
