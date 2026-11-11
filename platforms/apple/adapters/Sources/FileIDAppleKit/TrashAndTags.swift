import Foundation

/// FileManager.trashItem and Finder tag names for the Rust engine. Identity
/// checks, journals and recovery stay in Rust (`shell/macos.rs`).
enum TrashMover {
    #if DEBUG
    /// Tests only: a folder that stands in for the user's Trash. Release builds
    /// have no such switch, and nothing outside the test target sets it.
    nonisolated(unsafe) static var testRoot: URL?
    #endif

    /// Moves the item to the Trash and returns where it landed, when known.
    static func trash(_ url: URL) throws -> URL? {
        #if DEBUG
        if let root = testRoot {
            let landed = root.appendingPathComponent(url.lastPathComponent)
            try FileManager.default.moveItem(at: url, to: landed)
            return landed
        }
        #endif
        var landed: NSURL?
        try FileManager.default.trashItem(at: url, resultingItemURL: &landed)
        return landed as URL?
    }
}

enum FinderTags {
    static func read(_ url: URL) throws -> [String] {
        // A fresh URL: resource values are cached per instance.
        try URL(fileURLWithPath: url.path).resourceValues(forKeys: [.tagNamesKey]).tagNames ?? []
    }

    /// Replaces the tags. `NSURL.setResourceValue` works on macOS 15; the
    /// `URLResourceValues.tagNames` setter needs macOS 26.
    static func write(_ tags: [String], to url: URL) throws {
        guard FileManager.default.fileExists(atPath: url.path) else {
            throw CocoaError(.fileNoSuchFile, userInfo: [NSFilePathErrorKey: url.path])
        }
        try (URL(fileURLWithPath: url.path) as NSURL).setResourceValue(tags as NSArray, forKey: .tagNamesKey)
    }
}

/// `strdup` for an out-parameter the Rust side frees with fileid_apple_free.
func setOut(_ pointer: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?, _ text: String) {
    pointer?.pointee = strdup(text)
}

func errorText(_ error: Error) -> String {
    (error as NSError).localizedDescription
}
