import Foundation
import GRDB
import FileIDShared

/// The scanned locations (`library_roots`), each with whether it is reachable right now and
/// how many files it holds (a nested location's files count only there). Read-only: never
/// scans and never touches the files.
enum LibraryRoots {
    /// Half-open range matching every stored path under `root`: `root/` up to `root0`
    /// (`0` is the character after `/`; `]` follows `\`).
    static func subtreeBounds(_ root: String) -> (exact: String, low: String, high: String) {
        let windows = root.contains("\\") && !root.contains("/")
        let separator: Character = windows ? "\\" : "/"
        var trimmed = root
        while trimmed.last == separator { trimmed.removeLast() }
        return (trimmed, trimmed + String(separator), trimmed + (windows ? "]" : "0"))
    }

    /// The files one location holds: its path and everything under it, minus any deeper
    /// location, because the most specific location owns a file (the apps' `OfflineRoots`
    /// rule). Mirrors Rust `roots::RootScope`.
    struct Scope: Sendable, Equatable {
        let bounds: [String]
        let deeper: [[String]]
        let param: String

        init(root: String, others: [String]) {
            let own = LibraryRoots.subtreeBounds(root)
            let nested = Set(others.map(LibraryRoots.subtreeBounds)
                .filter { $0.exact != own.exact && Scope.within([own.exact, own.low, own.high], $0.exact) }
                .map { [$0.exact, $0.low, $0.high] })
            bounds = [own.exact, own.low, own.high]
            deeper = nested.sorted { $0.lexicographicallyPrecedes($1) }
            let payload: [Any] = bounds + [deeper]
            param = String(decoding: (try? JSONSerialization.data(withJSONObject: payload, options: [.withoutEscapingSlashes])) ?? Data("[]".utf8), as: UTF8.self)
        }

        /// nil without an id; throws for an id the catalog doesn't know.
        static func load(_ db: GRDB.Database, id: Int64?) throws -> Scope? {
            guard let id else { return nil }
            guard id > 0 else { throw UnknownRoot() }
            let rows = try Row.fetchAll(db, sql: "SELECT id,path FROM library_roots")
            guard let root = rows.first(where: { ($0["id"] as Int64) == id }) else { throw UnknownRoot() }
            return Scope(root: root["path"], others: rows.filter { ($0["id"] as Int64) != id }.map { $0["path"] })
        }

        func contains(_ path: String) -> Bool {
            Scope.within(bounds, path) && !deeper.contains { Scope.within($0, path) }
        }

        /// SQLite compares paths byte by byte, so these do too.
        private static func within(_ bounds: [String], _ path: String) -> Bool {
            path.utf8.elementsEqual(bounds[0].utf8)
                || (!path.utf8.lexicographicallyPrecedes(bounds[1].utf8) && path.utf8.lexicographicallyPrecedes(bounds[2].utf8))
        }
    }

    struct UnknownRoot: Error, LocalizedError {
        var errorDescription: String? { "That location is no longer in the library." }
    }

    /// SQL that is true when `column` lies in the scope bound to the one `?` it contains
    /// (a `Scope.param`), or when that value is NULL. Mirrors Rust `roots::clause`.
    static func scopeSQL(_ column: String) -> String {
        func inside(_ bounds: String) -> String {
            "(\(column)=json_extract(\(bounds),'$[0]') OR (\(column)>=json_extract(\(bounds),'$[1]') AND \(column)<json_extract(\(bounds),'$[2]')))"
        }
        return "(SELECT s.v IS NULL OR (\(inside("s.v")) AND NOT EXISTS (SELECT 1 FROM json_each(s.v,'$[3]') d WHERE \(inside("d.value")))) FROM (SELECT ? AS v) s)"
    }

    /// Scans record the full path as the display name; show the folder's own name instead.
    static func displayName(path: String, stored: String) -> String {
        guard stored == path else { return stored }
        let name = path.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? ""
        return name.isEmpty ? stored : name
    }

    static func list(_ db: GRDB.Database, online: (String) -> Bool = { ScanRootGuard.isReachable($0) }) throws -> [LibraryRoot] {
        let rows = try Row.fetchAll(db, sql: "SELECT id,path,display,kind,enabled,last_scan_at,last_status FROM library_roots ORDER BY added_at,id")
        let paths = rows.map { (($0["id"] as Int64), ($0["path"] as String)) }
        return try rows.map { row in
            let path: String = row["path"]
            let id: Int64 = row["id"]
            let bounds = subtreeBounds(path)
            let scope = Scope(root: path, others: paths.filter { $0.0 != id }.map(\.1))
            let totals = try Row.fetchOne(db, sql: """
                SELECT COUNT(*) AS c,COALESCE(SUM(size_bytes),0) AS b FROM files
                WHERE failed=0 AND (path_text=? OR (path_text>=? AND path_text<?)) AND \(scopeSQL("path_text"))
                """, arguments: [bounds.exact, bounds.low, bounds.high, scope.param])
            return LibraryRoot(
                id: row["id"], path: path, display: Self.displayName(path: path, stored: row["display"]), kind: row["kind"], enabled: (row["enabled"] as Int64) != 0,
                online: online(path), fileCount: totals?["c"] ?? 0, totalBytes: totals?["b"] ?? 0,
                lastScanAt: row["last_scan_at"], lastStatus: row["last_status"]
            )
        }
    }
}
