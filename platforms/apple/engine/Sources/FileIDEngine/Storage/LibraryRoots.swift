import Foundation
import GRDB
import FileIDShared

/// The scanned locations (`library_roots`), each with whether it is reachable right now and
/// how many files it holds. Read-only: never scans and never touches the files.
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

    /// Scans record the full path as the display name; show the folder's own name instead.
    static func displayName(path: String, stored: String) -> String {
        guard stored == path else { return stored }
        let name = path.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? ""
        return name.isEmpty ? stored : name
    }

    static func list(_ db: GRDB.Database, online: (String) -> Bool = { ScanRootGuard.isReachable($0) }) throws -> [LibraryRoot] {
        let rows = try Row.fetchAll(db, sql: "SELECT id,path,display,kind,enabled,last_scan_at,last_status FROM library_roots ORDER BY added_at,id")
        return try rows.map { row in
            let path: String = row["path"]
            let bounds = subtreeBounds(path)
            let totals = try Row.fetchOne(db, sql: """
                SELECT COUNT(*) AS c,COALESCE(SUM(size_bytes),0) AS b FROM files
                WHERE failed=0 AND (path_text=? OR (path_text>=? AND path_text<?))
                """, arguments: [bounds.exact, bounds.low, bounds.high])
            return LibraryRoot(
                id: row["id"], path: path, display: Self.displayName(path: path, stored: row["display"]), kind: row["kind"], enabled: (row["enabled"] as Int64) != 0,
                online: online(path), fileCount: totals?["c"] ?? 0, totalBytes: totals?["b"] ?? 0,
                lastScanAt: row["last_scan_at"], lastStatus: row["last_status"]
            )
        }
    }
}
