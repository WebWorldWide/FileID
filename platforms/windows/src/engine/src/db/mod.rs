//! SQLite database — owns the single writer connection; reads fan out via
//! fresh read-only connections (`open_read`). Drop-in for GRDB.swift on macOS.
//!
//! The writer is single-threaded by design (matches the macOS
//! `Database.swift` invariant): SQLite WAL allows concurrent readers but
//! only one writer at a time. We serialize writes through one connection;
//! reads use ephemeral read-only connections opened on demand.

pub mod migrations;
#[path = "../read/mod.rs"]
#[allow(dead_code)]
pub mod read;
pub mod zero_byte;

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
/// Applies every migration up to v20 in registered order.
pub fn open_writer(db_path: &Path) -> Result<Connection> {
    crate::util::read_only::require_writable(db_path)?;
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
    // rusqlite's default cache holds 16 statements; one dbwriter flush keeps ~18
    // prepare_cached alive at once, so the default evicts+re-prepares 1-2 hot
    // statements every batch. 32 keeps them all resident across flushes.
    conn.set_prepared_statement_cache_capacity(32);
    migrations::apply(&conn).context("applying migrations")?;
    #[cfg(target_os = "linux")]
    rehash_case_sensitive_paths(&conn).context("rehashing Linux path hashes")?;

    // Sweep orphaned "running" scan_sessions left over from a previous
    // crash. The engine writes status='running' at scan start; if it
    // exits abnormally the row stays stale forever, polluting Settings →
    // Recent scans. Mark them all as 'failed' on startup; new scans
    // overwrite this when they finish cleanly.
    match conn.execute(
        "UPDATE scan_sessions SET status = 'failed', completed_at = COALESCE(completed_at, started_at) \
         WHERE status = 'running'",
        [],
    ) {
        // A swept 'running' row means the previous engine died mid-scan, so
        // the last committed batch may have durable face_prints rows whose
        // crop JPEGs (written post-commit, outside the tx) never landed. Flag
        // it so the next incremental scan reconciles missing crops instead of
        // skipping those now-unchanged files forever. Clean shutdowns sweep 0
        // rows and pay nothing.
        Ok(swept) if swept > 0 => {
            UNCLEAN_PRIOR_SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
            tracing::info!(swept, "open_writer: swept orphaned 'running' scan_sessions (unclean prior shutdown)");
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "open_writer: sweeping orphaned 'running' scan_sessions failed");
        }
    }

    crate::commands::tools::recover(&conn)?;
    crate::catalog_index::CatalogIndex::recover(&conn)?;
    Ok(conn)
}

/// Linux catalogs written before path hashes became case-sensitive hold
/// lowercased hashes; the missing-file sweep compares hashes, so unchanged
/// files would be hidden after the first rescan. Runs once and records itself
/// in `user_version` because `grdb_migrations` rejects unknown identifiers.
#[cfg(target_os = "linux")]
fn rehash_case_sensitive_paths(conn: &Connection) -> Result<()> {
    const DONE: i64 = 1;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version & DONE != 0 {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let rows: Vec<(i64, String, i64)> = tx
        .prepare("SELECT id, path_text, path_hash FROM files")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut update = tx.prepare("UPDATE files SET path_hash = ?2 WHERE id = ?1")?;
    let mut changed = 0usize;
    for (id, path_text, old) in rows {
        let new = crate::util::path_safety::stable_path_hash(&path_text);
        if new != old {
            update.execute(rusqlite::params![id, new])?;
            changed += 1;
        }
    }
    drop(update);
    tx.execute_batch(&format!("PRAGMA user_version = {}", version | DONE))?;
    tx.commit()?;
    if changed > 0 {
        tracing::info!(changed, "open_writer: rehashed Linux path hashes");
    }
    Ok(())
}

/// Set true when `open_writer` sweeps a stale `running` scan_sessions row —
/// i.e. the previous engine process died mid-scan. Read once by the next
/// incremental scan to decide whether to reconcile face crops that a
/// post-commit write may have missed (see scan_session skip-set preload).
pub static UNCLEAN_PRIOR_SHUTDOWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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
        // wal_checkpoint(TRUNCATE) returns a row `(busy, log, checkpointed)`. A
        // blocked truncate surfaces as `busy != 0` in that row, NOT as a
        // SQLITE_BUSY error — `execute_batch` discards the row, so it would
        // report Ok() even when nothing was truncated (and the SQLITE_BUSY retry
        // arm was dead). Read the busy flag and retry on it.
        let busy = match conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get::<_, i64>(0)) {
            Ok(busy) => busy,
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::DatabaseBusy
                    && attempt < MAX_ATTEMPTS =>
            {
                std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
                continue;
            }
            Err(e) => return Err(e).context("WAL checkpoint(TRUNCATE) failed"),
        };
        if busy == 0 {
            return Ok(());
        }
        if attempt >= MAX_ATTEMPTS {
            anyhow::bail!("WAL checkpoint(TRUNCATE) still busy after {MAX_ATTEMPTS} attempts");
        }
        std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
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
    let virtual_tables: Vec<(String, bool)> = {
        let mut stmt = conn.prepare(
            "SELECT name, instr(lower(replace(sql, ' ', '')), 'content=') > 0 FROM sqlite_master \
             WHERE type='table' AND sql LIKE 'CREATE VIRTUAL TABLE%'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    // A plain table is an FTS5 shadow table when its name is `<vtab>_<suffix>`.
    let is_shadow = |name: &str| virtual_tables.iter().any(|(v, _)| name.starts_with(&format!("{v}_")));
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
        // 'delete-all' only exists for contentless/external-content FTS5; a
        // table that stores its own content is emptied with a plain DELETE.
        for (v, external_content) in &virtual_tables {
            let sql = if *external_content {
                format!("INSERT INTO \"{v}\"(\"{v}\") VALUES('delete-all')")
            } else {
                format!("DELETE FROM \"{v}\"")
            };
            tx.execute_batch(&sql).with_context(|| format!("resetting FTS table {v}"))?;
        }
        // Reset AUTOINCREMENT so the next scan starts at id 1 again. Only present
        // when an AUTOINCREMENT table exists (it does); guard anyway.
        let _ = tx.execute_batch("DELETE FROM sqlite_sequence");
        tx.commit().context("committing wipe")?;
        Ok(())
    })();
    let restore_foreign_keys = (|| -> Result<()> {
        conn.execute_batch("PRAGMA foreign_keys = ON")
            .context("re-enabling foreign_keys after wipe")?;
        let enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        if enabled != 1 {
            anyhow::bail!("foreign_keys remained disabled after wipe");
        }
        Ok(())
    })();
    if let Err(restore_error) = restore_foreign_keys {
        return match wipe {
            Ok(()) => Err(restore_error),
            Err(wipe_error) => Err(anyhow::anyhow!(
                "wipe failed: {wipe_error:#}; foreign-key recovery also failed: {restore_error:#}"
            )),
        };
    }
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_rehash_replaces_lowercased_path_hashes_once() {
        let path = std::env::temp_dir().join(format!(
            "fileid-rehash-{}-{:?}.sqlite",
            std::process::id(),
            std::thread::current().id()
        ));
        let _guard = TempDb(path.clone());
        {
            let w = open_writer(&path).expect("open_writer");
            w.execute(
                "INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension) \
                 VALUES ('/lib/Photo.JPG', ?1, 1, 0, 'image', 'jpg')",
                [crate::util::path_safety::stable_path_hash("/lib/photo.jpg")],
            )
            .expect("insert legacy row");
            w.execute_batch("PRAGMA user_version = 0").expect("reset marker");
        }
        let w = open_writer(&path).expect("reopen");
        let hash: i64 = w
            .query_row("SELECT path_hash FROM files", [], |row| row.get(0))
            .expect("hash");
        assert_eq!(hash, crate::util::path_safety::stable_path_hash("/lib/Photo.JPG"));
        let version: i64 = w.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version & 1, 1);
    }

    #[test]
    fn wipe_all_empties_every_fts_kind() {
        let conn = Connection::open_in_memory().unwrap();
        migrations::apply(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension) \
             VALUES ('/lib/a.txt', 1, 1, 0, 'document', 'txt'); \
             INSERT INTO catalog_evidence_fts(evidence_id, file_id, kind, text) VALUES (1, 1, 'passage', 'mulch');",
        )
        .unwrap();
        wipe_all(&conn).expect("wipe");
        let files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        let evidence: i64 = conn
            .query_row("SELECT COUNT(*) FROM catalog_evidence_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!((files, evidence), (0, 0));
    }
}
