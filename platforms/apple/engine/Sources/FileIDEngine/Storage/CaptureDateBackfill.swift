import CryptoKit
import Foundation
import GRDB

/// Catalogs written before scans read EXIF capture times store the file's copy
/// date for photos, and an incremental scan skips unchanged files, so those dates
/// would never be corrected. After a library folder's next completed scan this
/// reads each photo's EXIF time once (properties only, no decode) and fixes rows
/// that differ by more than a second. A marker per folder keeps it to one pass.
enum CaptureDateBackfill {
    static let version = "v1"

    static func marker(for root: URL) -> URL {
        let digest = SHA256.hash(data: Data(root.standardizedFileURL.path.utf8)).map { String(format: "%02x", $0) }.joined()
        return EngineStateRoot.directory.appendingPathComponent("capture-dates-\(version)", isDirectory: true)
            .appendingPathComponent(String(digest.prefix(32)))
    }

    /// True when the catalog already has rows under `root`, i.e. a scan of it could
    /// skip files whose stored date predates EXIF capture times.
    static func catalogHasRows(database: Database, root: URL) async -> Bool {
        let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        return (try? await database.pool.read { db in
            try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM files WHERE substr(path_text, 1, ?) = ?)",
                              arguments: [prefix.utf8.count, prefix])
        }) ?? true
    }

    /// Marks `root` done without a pass: a full scan just read every photo's EXIF.
    static func markDone(root: URL) {
        let mark = marker(for: root)
        try? FileManager.default.createDirectory(at: mark.deletingLastPathComponent(), withIntermediateDirectories: true)
        FileManager.default.createFile(atPath: mark.path, contents: Data())
    }

    @discardableResult
    static func runIfNeeded(database: Database, root: URL, isCancelled: @Sendable () -> Bool) async -> Int {
        let mark = marker(for: root)
        guard !FileManager.default.fileExists(atPath: mark.path) else { return 0 }
        let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        struct Photo: Sendable { let id: Int64; let path: String; let stored: Double? }
        guard let rows = try? await database.pool.read({ db in
            try Row.fetchAll(db, sql: "SELECT id, path_text, created_at FROM files WHERE kind = 'image' AND failed = 0 AND substr(path_text, 1, ?) = ?",
                             arguments: [prefix.utf8.count, prefix])
                .map { Photo(id: $0["id"], path: $0["path_text"], stored: $0["created_at"]) }
        }) else { return 0 }
        var fixes: [(Int64, Double)] = []
        for row in rows {
            if isCancelled() { return 0 }
            guard let captured = Tagging.exifCaptureTime(url: URL(fileURLWithPath: row.path))?.timeIntervalSince1970 else { continue }
            if row.stored.map({ abs($0 - captured) > 1 }) ?? true { fixes.append((row.id, captured)) }
        }
        for start in stride(from: 0, to: fixes.count, by: 500) {
            let chunk = fixes[start..<min(start + 500, fixes.count)]
            try? await database.pool.write { db in
                for (id, captured) in chunk {
                    try db.execute(sql: "UPDATE files SET created_at = ? WHERE id = ?", arguments: [captured, id])
                }
            }
        }
        markDone(root: root)
        JSONLog.shared.info(ev: "capture_dates_backfilled", extra: ["checked": AnyCodable(rows.count), "fixed": AnyCodable(fixes.count)])
        return fixes.count
    }
}
