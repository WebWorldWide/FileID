import Foundation

enum SecurityScopedAccessError: Error {
    case bookmarkRequired
    case bookmarkInvalid
    case bookmarkPathMismatch
    case accessDenied
}

final class SecurityScopedAccessLease: @unchecked Sendable {
    let path: String

    private weak var registry: SecurityScopedAccessRegistry?
    private let lock = NSLock()
    private var isReleased = false

    fileprivate init(path: String, registry: SecurityScopedAccessRegistry?) {
        self.path = path
        self.registry = registry
    }

    func release() {
        lock.lock()
        guard !isReleased else {
            lock.unlock()
            return
        }
        isReleased = true
        lock.unlock()
        registry?.release(path: path)
    }

    deinit {
        release()
    }
}

final class SecurityScopedAccessRegistry: @unchecked Sendable {
    static let shared = SecurityScopedAccessRegistry()

    private struct Access {
        let url: URL
        var references: Int
        let didStartAccess: Bool
    }

    private let lock = NSLock()
    private var accessByPath: [String: Access] = [:]
    private var rootLease: SecurityScopedAccessLease?

    var activeAccessCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return accessByPath.count
    }

    func replaceRootAccess(path: String, bookmark: Data) throws {
        let lease = try acquire(path: path, bookmark: bookmark)
        lock.lock()
        let previous = rootLease
        rootLease = lease
        lock.unlock()
        previous?.release()
    }

    func releaseRootAccess() {
        lock.lock()
        let previous = rootLease
        rootLease = nil
        lock.unlock()
        previous?.release()
    }

    func acquire(path: String, bookmark: Data?) throws -> SecurityScopedAccessLease {
        #if FILEID_APP_STORE
        guard let bookmark else { throw SecurityScopedAccessError.bookmarkRequired }
        #else
        guard let bookmark else {
            return SecurityScopedAccessLease(path: URL(fileURLWithPath: path).standardizedFileURL.path, registry: nil)
        }
        #endif

        var stale = false
        let url: URL
        do {
            url = try URL(
                resolvingBookmarkData: bookmark,
                options: [],
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
        let didStartAccess = url.startAccessingSecurityScopedResource()
        #if FILEID_APP_STORE
        guard didStartAccess else { throw SecurityScopedAccessError.accessDenied }
        #endif

        lock.lock()
        if var access = accessByPath[resolvedPath] {
            access.references += 1
            accessByPath[resolvedPath] = access
            lock.unlock()
            if didStartAccess { url.stopAccessingSecurityScopedResource() }
        } else {
            accessByPath[resolvedPath] = Access(url: url, references: 1, didStartAccess: didStartAccess)
            lock.unlock()
        }
        return SecurityScopedAccessLease(path: resolvedPath, registry: self)
    }

    fileprivate func release(path: String) {
        lock.lock()
        guard var access = accessByPath[path] else {
            lock.unlock()
            return
        }
        access.references -= 1
        if access.references > 0 {
            accessByPath[path] = access
            lock.unlock()
            return
        }
        accessByPath.removeValue(forKey: path)
        lock.unlock()
        if access.didStartAccess { access.url.stopAccessingSecurityScopedResource() }
    }
}
