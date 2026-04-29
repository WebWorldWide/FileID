import Foundation
import SQLite3

// MARK: - SQLiteCheckpoint
//
// SwiftData wraps Core Data wraps SQLite with WAL journal mode. Every
// `ModelContext.save()` appends to `<store>-wal`. SQLite *eventually*
// auto-checkpoints (default `wal_autocheckpoint = 1000` pages) and merges
// the WAL back into the main DB file, but on a long scan with frequent
// 400-record batch saves the WAL file can grow to hundreds of MB before
// the auto-checkpoint catches up. Each subsequent `save()` then has to
// fsync against an ever-larger WAL — the symptom the user described as
// "incredibly long wait time after running for a while."
//
// The cure: open a separate sqlite3 connection to the same store file and
// run `PRAGMA wal_checkpoint(TRUNCATE)` periodically. SQLite serializes
// connection-level access via its own locks, so this is safe to call
// concurrently with SwiftData's writes — at worst we get SQLITE_BUSY,
// in which case we retry next round.
//
// Caller cadence: invoke from `commitBatchSave` every N batches (e.g. every
// 8 saves at saveEvery=400 = roughly every 3 200 files = ~3 minutes at
// 18 files/s). More frequent than that wastes effort; less frequent lets
// the WAL grow into the slow zone.
enum SQLiteCheckpoint {

    /// Resolves the SwiftData default store URL. SwiftData puts it at
    /// `~/Library/Application Support/default.store`. If the path doesn't
    /// resolve (sandboxed install, weird container), returns nil and the
    /// caller no-ops.
    private static var storeURL: URL? {
        FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first?
            .appendingPathComponent("default.store")
    }

    /// Run `PRAGMA wal_checkpoint(TRUNCATE)` on the SwiftData store.
    /// Returns the (busy, log, checkpointed) tuple SQLite reports, or nil
    /// if the connection couldn't be opened.
    @discardableResult
    static func truncateWAL() -> (busy: Int, logFrames: Int, checkpointed: Int)? {
        guard let url = storeURL,
              FileManager.default.fileExists(atPath: url.path) else { return nil }

        var db: OpaquePointer?
        // SQLITE_OPEN_NOMUTEX = single-threaded mode (we only ever call from
        // one place); SQLITE_OPEN_PRIVATECACHE keeps us off the shared cache
        // SwiftData might be using. Fall back to defaults if open fails.
        let flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_NOMUTEX
        let openResult = sqlite3_open_v2(url.path, &db, flags, nil)
        guard openResult == SQLITE_OK, let db else {
            if let db { sqlite3_close(db) }
            NSLog("FileID SQLiteCheckpoint: open failed (%d) %@",
                  openResult, url.path)
            return nil
        }
        defer { sqlite3_close(db) }

        // Reasonable busy-timeout so a checkpoint started mid-write doesn't
        // immediately fail with SQLITE_BUSY. 500 ms is plenty — SwiftData's
        // saves complete in < 100 ms in practice.
        sqlite3_busy_timeout(db, 500)

        var stmt: OpaquePointer?
        let prepResult = sqlite3_prepare_v2(
            db, "PRAGMA wal_checkpoint(TRUNCATE);", -1, &stmt, nil
        )
        guard prepResult == SQLITE_OK, let stmt else {
            if let stmt { sqlite3_finalize(stmt) }
            NSLog("FileID SQLiteCheckpoint: prepare failed (%d)", prepResult)
            return nil
        }
        defer { sqlite3_finalize(stmt) }

        let stepResult = sqlite3_step(stmt)
        guard stepResult == SQLITE_ROW else {
            // SQLITE_BUSY (5) is OK — try again next round.
            if stepResult != SQLITE_BUSY {
                NSLog("FileID SQLiteCheckpoint: step unexpected (%d)", stepResult)
            }
            return nil
        }
        let busy        = Int(sqlite3_column_int(stmt, 0))
        let logFrames   = Int(sqlite3_column_int(stmt, 1))
        let checkpointed = Int(sqlite3_column_int(stmt, 2))
        return (busy, logFrames, checkpointed)
    }

    /// Returns the WAL file size in MB, or nil if it doesn't exist.
    /// Useful for diagnostic logging.
    static func walSizeMB() -> Double? {
        guard let url = storeURL else { return nil }
        let walURL = url.deletingLastPathComponent()
            .appendingPathComponent(url.lastPathComponent + "-wal")
        guard let attrs = try? FileManager.default.attributesOfItem(atPath: walURL.path),
              let bytes = attrs[.size] as? NSNumber else { return nil }
        return bytes.doubleValue / 1_048_576
    }
}
