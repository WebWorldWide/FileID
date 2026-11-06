import Foundation
import FileIDShared

/// Which cataloged paths sit under a location that is offline right now. The most
/// specific location wins, so an online folder inside an offline drive's path stays online.
struct OfflineRoots: Equatable, Sendable {
    struct Entry: Equatable, Sendable {
        let path: String
        let online: Bool
    }

    let entries: [Entry]

    static let none = OfflineRoots(entries: [])

    private init(entries: [Entry]) { self.entries = entries }

    init(_ roots: [LibraryRoot]) {
        guard roots.contains(where: { !$0.online }) else { self.init(entries: []); return }
        let entries = roots.map { Entry(path: Self.trimmed($0.path), online: $0.online) }
            .sorted { $0.path.count > $1.path.count }
        self.init(entries: entries)
    }

    var isEmpty: Bool { entries.isEmpty }

    func isOffline(_ path: String) -> Bool {
        guard !entries.isEmpty else { return false }
        return entries.first { Self.contains($0.path, path) }.map { !$0.online } ?? false
    }

    private static func trimmed(_ path: String) -> String {
        var path = path
        while path.count > 1 && path.hasSuffix("/") { path.removeLast() }
        return path
    }

    private static func contains(_ root: String, _ path: String) -> Bool {
        path == root || path.hasPrefix(root == "/" ? root : root + "/")
    }
}
