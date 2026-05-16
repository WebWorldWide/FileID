//! SQLite database — owns the single connection (writer side) plus a small
//! pool for read transactions. Drop-in for GRDB.swift on macOS.
//!
//! The writer is single-threaded by design (matches the macOS
//! `Database.swift` invariant): SQLite WAL allows concurrent readers but
//! only one writer at a time. We serialize writes through one connection;
//! reads use ephemeral read-only connections opened on demand.

pub mod migrations;

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// PRAGMAs applied at every connection open. Mirror of `Database.swift`'s
/// `prepareDatabase` block. WAL + NORMAL sync + 256 MB mmap + 64 MB cache.
pub const SETUP_PRAGMAS: &[&str] = &[
    "PRAGMA journal_mode = WAL",
    "PRAGMA synchronous = NORMAL",
    "PRAGMA temp_store = MEMORY",
    "PRAGMA mmap_size = 268435456",     // 256 MB
    "PRAGMA cache_size = -65536",        // 64 MB (negative = KB)
    "PRAGMA wal_autocheckpoint = 10000", // ~40 MB before checkpoint
    // V15.2 perf: pin the 64 MB page cache in memory instead of spilling
    // to a temp file mid-transaction. Our worst transaction is a
    // 100-row tagged-file batch (well under the cache); spill never wins.
    "PRAGMA cache_spill = 0",
    "PRAGMA foreign_keys = ON",
];

/// Open the engine's writer connection. Creates the file + schema if absent.
/// Applies every migration up to v7 in registered order.
pub fn open_writer(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating db parent dir {}", parent.display()))?;
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening db at {}", db_path.display()))?;
    for pragma in SETUP_PRAGMAS {
        conn.execute_batch(pragma)
            .with_context(|| format!("applying {pragma}"))?;
    }
    migrations::apply(&conn).context("applying migrations")?;

    // Sweep orphaned "running" scan_sessions left over from a previous
    // crash. The engine writes status='running' at scan start; if it
    // exits abnormally the row stays stale forever, polluting Settings →
    // Recent scans. Mark them all as 'failed' on startup; new scans
    // overwrite this when they finish cleanly.
    let _ = conn.execute(
        "UPDATE scan_sessions SET status = 'failed', completed_at = COALESCE(completed_at, started_at) \
         WHERE status = 'running'",
        [],
    );

    Ok(conn)
}

/// Open a read-only connection. The writer's WAL allows concurrent readers
/// without blocking; readers see a snapshot at the time the connection was
/// opened. App side will use this; the engine creates the writer.
#[allow(dead_code)]
pub fn open_reader(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening reader at {}", db_path.display()))?;
    for pragma in SETUP_PRAGMAS {
        // Some PRAGMAs are no-ops on read-only conns; ignore errors.
        let _ = conn.execute_batch(pragma);
    }
    Ok(conn)
}

/// Drain WAL into the main DB file. Called at shutdown to keep the on-disk
/// state self-contained. Mirror of macOS `Database.swift`'s shutdown
/// `PRAGMA wal_checkpoint(TRUNCATE)`.
///
/// Retries up to 5 times on `SQLITE_BUSY` (a read connection holding an
/// active txn at shutdown). 50 ms between attempts → ~250 ms worst case,
/// well under any reasonable shutdown grace period. After exhaustion,
/// returns the error so the caller can log + continue (the WAL persists
/// to disk and gets reapplied on next open — same as today's failure
/// mode without a retry, just less likely to hit it).
pub fn checkpoint_truncate(conn: &Connection) -> Result<()> {
    const MAX_ATTEMPTS: u32 = 5;
    const RETRY_DELAY_MS: u64 = 50;
    for attempt in 1..=MAX_ATTEMPTS {
        match conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)") {
            Ok(()) => return Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::DatabaseBusy
                    && attempt < MAX_ATTEMPTS =>
            {
                std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
                continue;
            }
            Err(e) => return Err(e).context("WAL checkpoint(TRUNCATE) failed"),
        }
    }
    Ok(())
}
