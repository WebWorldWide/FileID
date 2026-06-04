//! SQLite database — owns the single writer connection; reads fan out via
//! fresh read-only connections (`open_read`). Drop-in for GRDB.swift on macOS.
//!
//! The writer is single-threaded by design (matches the macOS
//! `Database.swift` invariant): SQLite WAL allows concurrent readers but
//! only one writer at a time. We serialize writes through one connection;
//! reads use ephemeral read-only connections opened on demand.

pub mod migrations;

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// PRAGMAs applied at every connection open.
/// WAL + NORMAL sync + 256 MB mmap + 64 MB cache.
pub const SETUP_PRAGMAS: &[&str] = &[
    "PRAGMA journal_mode = WAL",
    "PRAGMA synchronous = NORMAL",
    "PRAGMA temp_store = MEMORY",
    "PRAGMA mmap_size = 268435456",     // 256 MB
    "PRAGMA cache_size = -65536",        // 64 MB (negative = KB)
    "PRAGMA wal_autocheckpoint = 10000", // ~40 MB before checkpoint
    // Pin the 64 MB page cache in memory instead of spilling to a temp
    // file mid-transaction. Our worst transaction is a 100-row tagged-file
    // batch (well under cache); spill never wins.
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

/// Open an ephemeral READ-ONLY connection. Query handlers use these so they
/// never contend on the single writer mutex — WAL lets readers run concurrent
/// with the one writer. `NO_MUTEX` is safe because each connection stays on the
/// task that opened it. `journal_mode`/`synchronous` are writer concerns and
/// can't be set on a RO conn, so only the read-relevant pragmas are applied.
pub fn open_read(db_path: &Path) -> Result<Connection> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening read-only db at {}", db_path.display()))?;
    conn.execute_batch(
        "PRAGMA query_only = ON; \
         PRAGMA temp_store = MEMORY; \
         PRAGMA mmap_size = 268435456; \
         PRAGMA cache_size = -65536;",
    )
    .context("applying read-only pragmas")?;
    Ok(conn)
}

/// Run `PRAGMA quick_check` on the writer connection. Returns `Ok(())` when the
/// database is structurally sound, or `Err(detail)` carrying the first problem
/// SQLite reports. quick_check is the fast cousin of `integrity_check` — it
/// skips the expensive index-content cross-check, so it's cheap enough to run
/// at every startup yet still catches a torn page / truncated file before the
/// app reads garbage. Bounded to the first error (`quick_check(1)`) so a badly
/// corrupted large library can't hang startup enumerating every fault.
pub fn quick_check(conn: &Connection) -> std::result::Result<(), String> {
    match conn.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0)) {
        Ok(s) if s.eq_ignore_ascii_case("ok") => Ok(()),
        Ok(detail) => Err(detail),
        Err(e) => Err(format!("integrity check could not run: {e}")),
    }
}

/// Drain WAL into the main DB file. Called at shutdown to keep the on-disk
/// state self-contained — `PRAGMA wal_checkpoint(TRUNCATE)`.
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

/// Wipe every row of user data while keeping the schema + migration ledger
/// intact. Runs on the engine's writer connection — the single owner of the
/// DB handle — so "wipe library" never races the OS file-lock the way the app
/// deleting `fileid.sqlite` right after the engine exits does.
///
/// Tables are discovered from `sqlite_master`, so a future migration that adds
/// a table is wiped automatically (no hard-coded list to drift). FTS5 virtual
/// tables are emptied with the `'delete-all'` command; their shadow tables are
/// skipped (deleting from them directly corrupts the index). `grdb_migrations`
/// is preserved so the schema version survives the wipe.
pub fn wipe_all(conn: &Connection) -> Result<()> {
    // FTS5 virtual tables (emptied via the special command, never row-by-row).
    let virtual_tables: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND sql LIKE 'CREATE VIRTUAL TABLE%'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    // A plain table is an FTS5 shadow table when its name is `<vtab>_<suffix>`.
    let is_shadow = |name: &str| virtual_tables.iter().any(|v| name.starts_with(&format!("{v}_")));
    // Real data tables: CREATE TABLE, not sqlite internal, not the migration
    // ledger, not an FTS shadow table.
    let data_tables: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND sql LIKE 'CREATE TABLE%' \
               AND name NOT LIKE 'sqlite_%' AND name <> 'grdb_migrations'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).filter(|n| !is_shadow(n)).collect()
    };

    // FK off so DELETE order across parent/child tables is irrelevant (must be
    // toggled outside a transaction; it's a no-op inside one). `foreign_keys`
    // is a PER-CONNECTION setting, NOT transaction-scoped — if the body below
    // errors out via `?`, a naked early return would leave the engine's single
    // long-lived writer with FK enforcement OFF for the rest of the session,
    // letting orphaned child rows be inserted. Run the body in a closure and
    // re-enable FK on EVERY exit path before propagating the original error.
    conn.execute_batch("PRAGMA foreign_keys = OFF")?;
    let wipe = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        for t in &data_tables {
            tx.execute_batch(&format!("DELETE FROM \"{t}\""))
                .with_context(|| format!("wiping table {t}"))?;
        }
        for v in &virtual_tables {
            tx.execute_batch(&format!("INSERT INTO \"{v}\"(\"{v}\") VALUES('delete-all')"))
                .with_context(|| format!("resetting FTS table {v}"))?;
        }
        // Reset AUTOINCREMENT so the next scan starts at id 1 again. Only present
        // when an AUTOINCREMENT table exists (it does); guard anyway.
        let _ = tx.execute_batch("DELETE FROM sqlite_sequence");
        tx.commit().context("committing wipe")?;
        Ok(())
    })();
    let _ = conn.execute_batch("PRAGMA foreign_keys = ON");
    wipe?;

    // Shrink the on-disk file: collapse the WAL, then reclaim freed pages.
    // Both best-effort — an ephemeral reader holding a lock makes VACUUM
    // SQLITE_BUSY, which is harmless (space reclaims on a later run).
    let _ = checkpoint_truncate(conn);
    let _ = conn.execute_batch("VACUUM");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDb(std::path::PathBuf);
    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("sqlite-wal"));
            let _ = std::fs::remove_file(self.0.with_extension("sqlite-shm"));
        }
    }

    #[test]
    fn open_read_is_read_only() {
        let path = std::env::temp_dir().join(format!(
            "fileid-open-read-{}-{:?}.sqlite",
            std::process::id(),
            std::thread::current().id()
        ));
        let _guard = TempDb(path.clone());

        // Writer creates the file + a table + a row.
        {
            let w = open_writer(&path).expect("open_writer");
            w.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
                .expect("create table");
            w.execute("INSERT INTO t (v) VALUES ('a')", [])
                .expect("insert");
            checkpoint_truncate(&w).expect("checkpoint");
        }

        // Read-only connection can SELECT.
        let r = open_read(&path).expect("open_read");
        let n: i64 = r
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .expect("select count");
        assert_eq!(n, 1);

        // ...but writes are rejected.
        assert!(
            r.execute("INSERT INTO t (v) VALUES ('b')", []).is_err(),
            "INSERT through a read-only conn must fail"
        );
        assert!(
            r.execute_batch("CREATE TABLE t2 (id INTEGER)").is_err(),
            "CREATE TABLE through a read-only conn must fail"
        );
    }
}
