import Foundation

enum SecurityScopedAccessError: Error {
    case bookmarkRequired
    case bookmarkInvalid
    case bookmarkPathMismatch
    case accessDenied
}

final class SecurityScopedAccessRegistry: @unchecked Sendable {
    static let shared = SecurityScopedAccessRegistry()

    private let lock = NSLock()
    private var retainedURLs: [String: URL] = [:]

    private init() {}

    func acquire(path: String, bookmark: Data?) throws -> String {
        #if FILEID_APP_STORE
        guard let bookmark else { throw SecurityScopedAccessError.bookmarkRequired }
        #else
        guard let bookmark else { return path }
        #endif

        var stale = false
        let url: URL
        do {
            url = try URL(
                resolvingBookmarkData: bookmark,
                options: Self.resolutionOptions,
                relativeTo: nil,
                bookmarkDataIsStale: &stale
            )
        } catch {
            throw SecurityScopedAccessError.bookmarkInvalid
        }

        let resolvedPath = url.standardizedFileURL.path
        guard resolvedPath == URL(fileURLWithPath: path).standardizedFileURL.path else {
            throw SecurityScopedAccessError.bookmarkPathMismatch
        }

        lock.lock()
        defer { lock.unlock() }
        if retainedURLs[resolvedPath] != nil { return resolvedPath }

        let started = url.startAccessingSecurityScopedResource()
        #if FILEID_APP_STORE
        guard started else { throw SecurityScopedAccessError.accessDenied }
        #endif
        if started { retainedURLs[resolvedPath] = url }
        return resolvedPath
    }

    private static var resolutionOptions: URL.BookmarkResolutionOptions {
        #if FILEID_APP_STORE
        [.withSecurityScope]
        #else
        []
        #endif
    }
}
