import Foundation

enum SecurityScopedBookmarkError: Error {
    case accessDenied
}

enum SecurityScopedBookmark {
    static let pickedFolderDefaultsKey = "pickedFolderBookmark.v2"

    private final class RetainedRootAccess: @unchecked Sendable {
        static let shared = RetainedRootAccess()

        private let lock = NSLock()
        private var rootURL: URL?

        func retain(_ url: URL) -> Bool {
            lock.lock()
            if rootURL?.standardizedFileURL.path == url.standardizedFileURL.path {
                lock.unlock()
                return true
            }

            let started = url.startAccessingSecurityScopedResource()
            #if FILEID_APP_STORE
            guard started else {
                lock.unlock()
                return false
            }
            #endif

            let previous = rootURL
            rootURL = started ? url : nil
            lock.unlock()
            previous?.stopAccessingSecurityScopedResource()
            return true
        }

        func release() {
            lock.lock()
            let previous = rootURL
            rootURL = nil
            lock.unlock()
            previous?.stopAccessingSecurityScopedResource()
        }
    }

    private static var persistentCreationOptions: URL.BookmarkCreationOptions {
        #if FILEID_APP_STORE
        [.withSecurityScope]
        #else
        []
        #endif
    }

    private static var persistentResolutionOptions: URL.BookmarkResolutionOptions {
        #if FILEID_APP_STORE
        [.withSecurityScope]
        #else
        []
        #endif
    }

    static func make(for url: URL) throws -> Data {
        try url.bookmarkData(
            options: persistentCreationOptions,
            includingResourceValuesForKeys: nil,
            relativeTo: nil
        )
    }

    static func makeIPCBookmark(for url: URL) throws -> Data {
        #if FILEID_APP_STORE
        guard url.startAccessingSecurityScopedResource() else {
            throw SecurityScopedBookmarkError.accessDenied
        }
        defer { url.stopAccessingSecurityScopedResource() }
        #endif

        return try url.bookmarkData(
            options: [],
            includingResourceValuesForKeys: nil,
            relativeTo: nil
        )
    }

    static func resolve(_ data: Data, stale: inout Bool) throws -> URL {
        try URL(
            resolvingBookmarkData: data,
            options: persistentResolutionOptions,
            relativeTo: nil,
            bookmarkDataIsStale: &stale
        )
    }

    @discardableResult
    static func retainAccess(for data: Data) -> URL? {
        do {
            var stale = false
            let url = try resolve(data, stale: &stale)
            return RetainedRootAccess.shared.retain(url) ? url : nil
        } catch {
            return nil
        }
    }

    static func releaseRetainedAccess() {
        RetainedRootAccess.shared.release()
    }
}
