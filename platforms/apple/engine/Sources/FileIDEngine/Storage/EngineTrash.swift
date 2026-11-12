import Foundation

/// Where `trashFiles` puts files. Shipped builds use the user's Trash
/// (`FileManager.trashItem`). Test builds compiled with `-DFILEID_TEST_TRASH_ROOT`
/// (conformance only; `assemble_app.sh` refuses such an engine) move them into
/// `$FILEID_TEST_TRASH_ROOT` instead, and only with a scratch catalog
/// (`FILEID_DATABASE_PATH`); otherwise they refuse, so a test build never
/// reaches the real Trash. The Rust engine's `test-trash-root` feature works
/// the same way, with the same Finder naming ("photo.jpg", then "photo 2.jpg").
enum EngineTrash {
    struct Refusal: LocalizedError {
        let errorDescription: String?
    }

    /// Moves `url` to the Trash and returns where it landed (nil when the Trash didn't say).
    static func move(_ url: URL) throws -> URL? {
        #if FILEID_TEST_TRASH_ROOT
        return try TestRoot.move(url, into: TestRoot.folder())
        #else
        var landed: NSURL?
        try FileManager.default.trashItem(at: url, resultingItemURL: &landed)
        return landed as URL?
        #endif
    }

    /// The folders a file trashed from `original` can be in.
    static func folders(for original: URL) -> [URL]? {
        #if FILEID_TEST_TRASH_ROOT
        return [(try? TestRoot.folder())].compactMap { $0 }
        #else
        return nil
        #endif
    }

    #if FILEID_TEST_TRASH_ROOT
    enum TestRoot {
        static func folder() throws -> URL {
            let environment = ProcessInfo.processInfo.environment
            guard let root = environment["FILEID_TEST_TRASH_ROOT"], !root.isEmpty else {
                throw Refusal(errorDescription: "this test build moves files only into FILEID_TEST_TRASH_ROOT, which is not set")
            }
            guard let catalog = environment["FILEID_DATABASE_PATH"], !catalog.isEmpty else {
                throw Refusal(errorDescription: "the test Trash root needs a scratch catalog (FILEID_DATABASE_PATH)")
            }
            guard root.hasPrefix("/") else { throw Refusal(errorDescription: "FILEID_TEST_TRASH_ROOT must be absolute") }
            let url = URL(fileURLWithPath: root, isDirectory: true)
            try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
            return url
        }

        static func move(_ url: URL, into root: URL) throws -> URL {
            let name = url.lastPathComponent
            let stem = url.deletingPathExtension().lastPathComponent
            let ext = url.pathExtension.isEmpty ? "" : ".\(url.pathExtension)"
            for attempt in 1..<10_000 {
                let candidate = root.appendingPathComponent(attempt == 1 ? name : "\(stem) \(attempt)\(ext)")
                if renamex_np(url.path, candidate.path, UInt32(RENAME_EXCL)) == 0 { return candidate }
                if errno != EEXIST {
                    throw Refusal(errorDescription: "move into the test Trash root: \(String(cString: strerror(errno)))")
                }
            }
            throw Refusal(errorDescription: "no free name in the test Trash root for \(name)")
        }
    }
    #endif
}
