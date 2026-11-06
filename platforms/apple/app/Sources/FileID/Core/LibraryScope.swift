import Foundation
import FileIDShared

/// One library location that Library, Search & Moments and Chat are scoped to. A file belongs to
/// it when its path is the location's path or lies under it, never a sibling that shares the
/// prefix (/Camera never holds /Camera2), and never inside a deeper location: the most specific
/// location owns a file, as in `OfflineRoots`. Mirrors the engines' `rootID` rule.
struct LibraryScope: Equatable, Sendable {
    static let storageKey = "libraryScopeRootID"

    let root: LibraryRoot
    let bounds: [String]
    let deeper: [[String]]
    /// JSON bound to `LibraryScope.sql`'s one parameter.
    let param: String

    init?(rootID: Int64?, roots: [LibraryRoot]) {
        guard let rootID, let root = roots.first(where: { $0.id == rootID }) else { return nil }
        let own = Self.subtreeBounds(root.path)
        let nested = Set(roots.filter { $0.id != rootID }.map { Self.subtreeBounds($0.path) }
            .filter { $0[0] != own[0] && Self.within(own, $0[0]) })
        self.root = root
        bounds = own
        deeper = nested.sorted { $0.lexicographicallyPrecedes($1) }
        let payload: [Any] = own + [deeper]
        param = String(decoding: (try? JSONSerialization.data(withJSONObject: payload, options: [.withoutEscapingSlashes])) ?? Data("[]".utf8), as: UTF8.self)
    }

    /// The stored choice once the location list is known: kept while the list is still empty
    /// (not loaded yet), dropped when its location is gone.
    static func reconciled(_ stored: Int64?, roots: [LibraryRoot]) -> Int64? {
        guard let stored, !roots.isEmpty else { return stored }
        return roots.contains { $0.id == stored } ? stored : nil
    }

    /// Title bar label: the location's name, "All locations", or the folder being worked on
    /// before anything is cataloged.
    static func title(_ scope: LibraryScope?, roots: [LibraryRoot], folderName: String?) -> String {
        if let scope { return scope.root.display }
        if roots.isEmpty { return folderName ?? "Choose a folder" }
        return "All locations"
    }

    /// Spoken label for one location row in the switcher.
    static func spokenLabel(_ root: LibraryRoot) -> String {
        "\(root.display), \(fileCountText(root.fileCount))" + (root.online ? "" : ", offline")
    }

    static func fileCountText(_ count: Int64) -> String {
        count == 1 ? "1 file" : "\(count.formatted()) files"
    }

    func contains(_ path: String) -> Bool {
        Self.within(bounds, path) && !deeper.contains { Self.within($0, path) }
    }

    /// SQL that is true when `column` lies in the scope bound to its one `?`, or when that
    /// value is NULL.
    static func sql(_ column: String) -> String {
        func inside(_ bounds: String) -> String {
            "(\(column)=json_extract(\(bounds),'$[0]') OR (\(column)>=json_extract(\(bounds),'$[1]') AND \(column)<json_extract(\(bounds),'$[2]')))"
        }
        return "(SELECT s.v IS NULL OR (\(inside("s.v")) AND NOT EXISTS (SELECT 1 FROM json_each(s.v,'$[3]') d WHERE \(inside("d.value")))) FROM (SELECT ? AS v) s)"
    }

    /// Half-open range of every path under `root`: `root/` up to `root0` (`]` follows `\`).
    static func subtreeBounds(_ root: String) -> [String] {
        let windows = root.contains("\\") && !root.contains("/")
        let separator: Character = windows ? "\\" : "/"
        var trimmed = root
        while trimmed.last == separator { trimmed.removeLast() }
        return [trimmed, trimmed + String(separator), trimmed + (windows ? "]" : "0")]
    }

    /// SQLite compares paths byte by byte, so this does too.
    private static func within(_ bounds: [String], _ path: String) -> Bool {
        path.utf8.elementsEqual(bounds[0].utf8)
            || (!path.utf8.lexicographicallyPrecedes(bounds[1].utf8) && path.utf8.lexicographicallyPrecedes(bounds[2].utf8))
    }
}
