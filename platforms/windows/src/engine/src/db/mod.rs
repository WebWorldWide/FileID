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
    Ok(conn)
}

/// Open a read-only connection. The writer's WAL allows concurrent readers
/// without blocking; readers see a snapshot at the time the connection was
/// opened. App side will use this; the engine creates the writer.
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
pub fn checkpoint_truncate(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .context("WAL checkpoint(TRUNCATE) failed")?;
    Ok(())
}
