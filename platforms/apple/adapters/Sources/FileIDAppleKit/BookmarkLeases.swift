import Foundation

/// Folders the Rust engine reads through bookmarks the app made. A lease keeps
/// security-scoped access open until it is closed or the engine shuts down.
final class BookmarkLeases: @unchecked Sendable {
    static let shared = BookmarkLeases()

    struct Lease {
        let id: UInt64
        let path: String
        let stale: Bool
    }

    private let lock = NSLock()
    private var nextID: UInt64 = 1
    private var open: [UInt64: (url: URL, scoped: Bool)] = [:]

    func open(_ bookmark: Data) -> Lease? {
        var stale = false
        let url: URL
        if let scoped = try? URL(resolvingBookmarkData: bookmark, options: [.withSecurityScope, .withoutUI],
                                 relativeTo: nil, bookmarkDataIsStale: &stale) {
            url = scoped
        } else if let plain = try? URL(resolvingBookmarkData: bookmark, options: [.withoutUI],
                                       relativeTo: nil, bookmarkDataIsStale: &stale) {
            url = plain
        } else {
            return nil
        }
        let scoped = url.startAccessingSecurityScopedResource()
        let id = lock.withLock { () -> UInt64 in
            let id = nextID
            nextID += 1
            open[id] = (url, scoped)
            return id
        }
        return Lease(id: id, path: url.path, stale: stale)
    }

    func close(_ id: UInt64) {
        let entry = lock.withLock { open.removeValue(forKey: id) }
        if let entry, entry.scoped { entry.url.stopAccessingSecurityScopedResource() }
    }

    func closeAll() {
        let all = lock.withLock { () -> [(url: URL, scoped: Bool)] in
            defer { open.removeAll() }
            return Array(open.values)
        }
        for entry in all where entry.scoped { entry.url.stopAccessingSecurityScopedResource() }
    }

    var count: Int { lock.withLock { open.count } }
}
