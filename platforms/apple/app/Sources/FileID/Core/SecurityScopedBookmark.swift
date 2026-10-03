import Foundation

enum SecurityScopedBookmark {
    static let pickedFolderDefaultsKey = "pickedFolderBookmark.v2"

    private final class RetainedAccess: @unchecked Sendable {
        static let shared = RetainedAccess()
        private let lock = NSLock()
        private var urls: [String: URL] = [:]

        func retain(_ url: URL) -> Bool {
            lock.lock()
            defer { lock.unlock() }
            if urls[url.path] != nil { return true }
            let started = url.startAccessingSecurityScopedResource()
            #if FILEID_APP_STORE
            guard started else { return false }
            #endif
            if started { urls[url.path] = url }
            return true
        }
    }

    static var creationOptions: URL.BookmarkCreationOptions {
        #if FILEID_APP_STORE
        [.withSecurityScope]
        #else
        []
        #endif
    }

    static var resolutionOptions: URL.BookmarkResolutionOptions {
        #if FILEID_APP_STORE
        [.withSecurityScope]
        #else
        []
        #endif
    }

    static func make(for url: URL) throws -> Data {
        try url.bookmarkData(
            options: creationOptions,
            includingResourceValuesForKeys: nil,
            relativeTo: nil
        )
    }

    static func resolve(_ data: Data, stale: inout Bool) throws -> URL {
        try URL(
            resolvingBookmarkData: data,
            options: resolutionOptions,
            relativeTo: nil,
            bookmarkDataIsStale: &stale
        )
    }

    @discardableResult
    static func retainAccess(for data: Data) -> URL? {
        do {
            var stale = false
            let url = try resolve(data, stale: &stale)
            return RetainedAccess.shared.retain(url) ? url : nil
        } catch {
            return nil
        }
    }
}
