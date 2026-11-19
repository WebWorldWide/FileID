//! SQLite database: one single-threaded writer connection (WAL allows only one writer); reads use ephemeral read-only
//! connections (`open_read`).

pub mod migrations;
pub mod person_counts;
#[path = "../read/mod.rs"]
pub mod read;
pub mod zero_byte;

#[cfg(test)]
pub(crate) mod test_support {
    use rusqlite::{params, Connection};

    pub(crate) fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        super::migrations::apply(&conn).unwrap();
        conn
    }

    pub(crate) fn insert_file(conn: &Connection, id: i64, path: &str, kind: &str) {
        let extension = path.rsplit_once('.').map_or("", |(_, ext)| ext).to_ascii_lowercase();
        conn.execute(
            "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) \
             VALUES(?1,?2,?1,100,10,0,?3,?4)",
            params![id, path, kind, extension],
        )
        .unwrap();
    }

    const SWIFT_WRITTEN: &str = "../../../../shared/test-corpus/conformance/fixtures/swift-written";

    /// A file the retired Swift engine wrote, committed under
    /// `shared/test-corpus/conformance/fixtures/swift-written`.
    pub(crate) fn swift_written(relative: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SWIFT_WRITTEN).join(relative)
    }

    /// Re-roots the paths in a Swift-written journal: the folder the fixture was
    /// captured in becomes `to`, in every string, so the journal names files the
    /// test made.
    pub(crate) fn rebase_paths(value: &mut serde_json::Value, from: &str, to: &str) {
        match value {
            serde_json::Value::String(text) => *text = text.replace(from, to),
            serde_json::Value::Array(items) => items.iter_mut().for_each(|item| rebase_paths(item, from, to)),
            serde_json::Value::Object(map) => map.values_mut().for_each(|item| rebase_paths(item, from, to)),
            _ => {}
        }
    }

    /// A copy of a catalog the Swift engine wrote, open for the test; the copy
    /// is removed when this is dropped.
    pub(crate) struct SwiftCatalog {
        pub(crate) conn: Connection,
        directory: std::path::PathBuf,
    }

    impl Drop for SwiftCatalog {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.directory).ok();
        }
    }

    pub(crate) fn swift_catalog(fixture: &str) -> SwiftCatalog {
        let directory = std::env::temp_dir().join(format!("fileid-swift-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let copy = directory.join("fileid.sqlite");
        std::fs::copy(swift_written(&format!("{fixture}/state/FileID/fileid.sqlite")), &copy).unwrap();
        let conn = super::open_writer(&copy).unwrap();
        SwiftCatalog { conn, directory }
    }
}

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Applied at every connection open: WAL + NORMAL sync + 256 MB mmap + 64 MB cache.
pub const SETUP_PRAGMAS: &[&str] = &[
    "PRAGMA journal_mode = WAL",
    "PRAGMA synchronous = NORMAL",
    "PRAGMA temp_store = MEMORY",
    "PRAGMA mmap_size = 268435456", // 256 MB
    "PRAGMA cache_size = -65536", // 64 MB (negative = KB)
    "PRAGMA wal_autocheckpoint = 10000", // ~40 MB before checkpoint
    // Pin the page cache in memory instead of spilling to a temp file mid-transaction; the worst transaction (a 100-row
    // tagged-file batch) fits well under it.
    "PRAGMA cache_spill = 0",
    "PRAGMA foreign_keys = ON",
];

pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug)]
pub struct ReadOnlyDatabase;

impl std::fmt::Display for ReadOnlyDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the library database is read-only")
    }
}

impl std::error::Error for ReadOnlyDatabase {}

pub fn open_writer(db_path: &Path) -> Result<Connection> {
    crate::util::read_only::require_writable(db_path)?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating db parent dir {}", parent.display()))?;
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening db at {}", db_path.display()))?;
    conn.busy_timeout(BUSY_TIMEOUT).context("setting busy timeout")?;
    if conn.is_readonly(rusqlite::MAIN_DB).unwrap_or(false) {
        return Err(ReadOnlyDatabase.into());
    }
    for pragma in SETUP_PRAGMAS {
        conn.execute_batch(pragma)
            .with_context(|| format!("applying {pragma}"))?;
    }
    // The default 16-statement cache evicts and re-prepares 1-2 hot statements on every dbwriter flush (~18 live at
    // once).
    conn.set_prepared_statement_cache_capacity(32);
    migrations::apply(&conn).context("applying migrations")?;
    #[cfg(target_os = "linux")]
    rehash_case_sensitive_paths(&conn).context("rehashing Linux path hashes")?;

    match conn.execute(
        "UPDATE scan_sessions SET status = 'failed', completed_at = COALESCE(completed_at, started_at) \
         WHERE status = 'running'",
        [],
    ) {
        // The previous engine died mid-scan, so the last committed batch may hold face_prints whose crop JPEGs (written
        // post-commit) never landed; flag it so the next incremental scan reconciles them.
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
    crate::commands::take_grouping::recover(&conn)?;
    crate::commands::face_refresh::recover(&conn)?;
    Ok(conn)
}

/// Linux catalogs written before path hashes became case-sensitive hold lowercased hashes, which would hide unchanged
/// files after the first rescan. Runs once, recorded in `user_version` because `grdb_migrations` rejects unknown
/// identifiers.
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

/// Set when `open_writer` sweeps a stale `running` scan_sessions row; read once by the next incremental scan (see
/// scan_session skip-set preload).
pub static UNCLEAN_PRIOR_SHUTDOWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Ephemeral READ-ONLY connection so query handlers never contend on the writer mutex. `NO_MUTEX` is safe because each
/// connection stays on the task that opened it; `journal_mode`/`synchronous` can't be set on a RO conn.
pub fn open_read(db_path: &Path) -> Result<Connection> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening read-only db at {}", db_path.display()))?;
    conn.busy_timeout(BUSY_TIMEOUT).context("setting busy timeout")?;
    conn.execute_batch(
        "PRAGMA query_only = ON; \
         PRAGMA temp_store = MEMORY; \
         PRAGMA mmap_size = 268435456; \
         PRAGMA cache_size = -65536;",
    )
    .context("applying read-only pragmas")?;
    Ok(conn)
}

pub struct ReaderPool {
    capacity: usize,
    idle: parking_lot::Mutex<Vec<(std::path::PathBuf, Connection)>>,
}

pub struct Reader<'a> {
    pool: &'a ReaderPool,
    path: std::path::PathBuf,
    conn: Option<Connection>,
}

impl std::ops::Deref for Reader<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("held until drop")
    }
}

impl std::ops::DerefMut for Reader<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("held until drop")
    }
}

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        // A connection left inside a transaction would pin an old snapshot.
        let Some(conn) = self.conn.take().filter(Connection::is_autocommit) else { return };
        let mut idle = self.pool.idle.lock();
        idle.retain(|(path, _)| *path == self.path);
        if idle.len() < self.pool.capacity {
            idle.push((std::mem::take(&mut self.path), conn));
        }
    }
}

impl ReaderPool {
    pub const fn new(capacity: usize) -> Self {
        Self { capacity, idle: parking_lot::Mutex::new(Vec::new()) }
    }

    pub fn get(&self, db_path: &Path) -> Result<Reader<'_>> {
        let cached = {
            let mut idle = self.idle.lock();
            idle.retain(|(path, _)| path == db_path);
            idle.pop()
        };
        let conn = match cached {
            Some((_, conn)) => conn,
            None => {
                let conn = open_read(db_path)?;
                conn.set_prepared_statement_cache_capacity(32);
                conn
            }
        };
        Ok(Reader { pool: self, path: db_path.to_path_buf(), conn: Some(conn) })
    }

    pub fn clear(&self) {
        self.idle.lock().clear();
    }

    #[cfg(test)]
    fn idle_count(&self) -> usize {
        self.idle.lock().len()
    }
}

// Tests delete their temporary catalogs, which an idle handle would block on Windows.
static READERS: ReaderPool = ReaderPool::new(if cfg!(test) { 0 } else { 4 });

/// `None` for an in-memory database (only its own connection can see it).
pub fn reader_for(writer: &parking_lot::Mutex<Connection>) -> Result<Option<Reader<'static>>> {
    let path = writer.lock().path().filter(|path| !path.is_empty()).map(std::path::PathBuf::from);
    path.map(|path| READERS.get(&path)).transpose()
}

pub fn close_readers() {
    READERS.clear();
}

/// `Err(detail)` carries the first problem SQLite reports; bounded to `quick_check(1)` so a badly corrupted large library can't hang startup enumerating every fault.
pub fn quick_check(conn: &Connection) -> std::result::Result<(), String> {
    match conn.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0)) {
        Ok(s) if s.eq_ignore_ascii_case("ok") => Ok(()),
        Ok(detail) => Err(detail),
        Err(e) => Err(format!("integrity check could not run: {e}")),
    }
}

/// Runs on its own read-only connection after `ready` so a large library does not delay launch. `cancel` interrupts it:
/// its read transaction would otherwise make the shutdown WAL checkpoint report busy.
#[derive(Default)]
pub struct IntegrityCheck {
    state: parking_lot::Mutex<IntegrityCheckState>,
}

#[derive(Default)]
struct IntegrityCheckState {
    cancelled: bool,
    finished: bool,
    interrupt: Option<rusqlite::InterruptHandle>,
}

impl IntegrityCheck {
    pub fn run(&self, db_path: &Path) -> Option<String> {
        let verdict = self.check(db_path);
        let mut state = self.state.lock();
        state.finished = true;
        state.interrupt = None;
        if state.cancelled { None } else { verdict.err() }
    }

    fn check(&self, db_path: &Path) -> std::result::Result<(), String> {
        if self.state.lock().cancelled {
            return Ok(());
        }
        let conn = open_read(db_path).map_err(|e| format!("integrity check could not run: {e:#}"))?;
        {
            let mut state = self.state.lock();
            if state.cancelled {
                return Ok(());
            }
            state.interrupt = Some(conn.get_interrupt_handle());
        }
        let verdict = quick_check(&conn);
        self.state.lock().interrupt = None;
        verdict
    }

    pub fn cancel(&self) {
        // An interrupt that lands before the statement starts is dropped by SQLite, so repeat it.
        for _ in 0..100 {
            {
                let mut state = self.state.lock();
                state.cancelled = true;
                match (&state.interrupt, state.finished) {
                    (_, true) => return,
                    (Some(handle), false) => handle.interrupt(),
                    (None, false) => {}
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    pub fn skip(&self) {
        self.state.lock().finished = true;
    }
}

/// Retries on SQLITE_BUSY up to 5 times, 50 ms apart; on exhaustion the caller logs and the WAL is replayed on next
/// open.
pub fn checkpoint_truncate(conn: &Connection) -> Result<()> {
    const MAX_ATTEMPTS: u32 = 5;
    const RETRY_DELAY_MS: u64 = 50;
    for attempt in 1..=MAX_ATTEMPTS {
        // `wal_checkpoint(TRUNCATE)` reports a blocked truncate as `busy != 0` in its result row, not as an error;
        // `execute_batch` would discard it.
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

/// Wipe all user data, keeping schema and migration ledger. FTS5 shadow tables are skipped (deleting them directly
/// corrupts the index).
pub fn wipe_all(conn: &Connection) -> Result<()> {
    let virtual_tables: Vec<(String, bool)> = {
        let mut stmt = conn.prepare(
            "SELECT name, instr(lower(replace(sql, ' ', '')), 'content=') > 0 FROM sqlite_master \
             WHERE type='table' AND sql LIKE 'CREATE VIRTUAL TABLE%'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let is_shadow = |name: &str| virtual_tables.iter().any(|(v, _)| name.starts_with(&format!("{v}_")));
    let data_tables: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master \
             WHERE type='table' AND sql LIKE 'CREATE TABLE%' \
               AND name NOT LIKE 'sqlite_%' AND name <> 'grdb_migrations' \
               AND name <> 'catalog_vector_state'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).filter(|n| !is_shadow(n)).collect()
    };

    // foreign_keys is per-connection and must be toggled outside a transaction: re-enable on every exit path.
    conn.execute_batch("PRAGMA foreign_keys = OFF")?;
    let wipe = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        for t in &data_tables {
            tx.execute_batch(&format!("DELETE FROM \"{t}\""))
                .with_context(|| format!("wiping table {t}"))?;
        }
        // 'delete-all' only exists for contentless/external-content FTS5.
        for (v, external_content) in &virtual_tables {
            let sql = if *external_content {
                format!("INSERT INTO \"{v}\"(\"{v}\") VALUES('delete-all')")
            } else {
                format!("DELETE FROM \"{v}\"")
            };
            tx.execute_batch(&sql).with_context(|| format!("resetting FTS table {v}"))?;
        }
        // The vector triggers UPDATE this row on every embedding change, so deleting it would stop the visual-search
        // index seeing new embeddings. Keep it, reset its identity (invalidates cached graphs) and generation.
        tx.execute_batch(
            "UPDATE catalog_vector_state SET instance_id=hex(randomblob(16)),generation=0; \
             UPDATE catalog_vector_state SET nonce=instance_id;",
        )
        .context("resetting vector state")?;
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

    // Best-effort: an ephemeral reader holding a lock makes VACUUM SQLITE_BUSY; space reclaims on a later run.
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

        {
            let w = open_writer(&path).expect("open_writer");
            w.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
                .expect("create table");
            w.execute("INSERT INTO t (v) VALUES ('a')", [])
                .expect("insert");
            checkpoint_truncate(&w).expect("checkpoint");
        }

        let r = open_read(&path).expect("open_read");
        let n: i64 = r
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .expect("select count");
        assert_eq!(n, 1);

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

    fn temp_db(label: &str) -> (std::path::PathBuf, TempDir) {
        let dir = std::env::temp_dir().join(format!("fileid-db-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("fileid.sqlite"), TempDir(dir))
    }

    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn seeded(path: &Path) {
        let w = open_writer(path).expect("open_writer");
        w.execute(
            "INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension) \
             VALUES ('/lib/a.txt', 1, 1, 0, 'document', 'txt')",
            [],
        )
        .unwrap();
        checkpoint_truncate(&w).unwrap();
    }

    #[test]
    fn a_newer_schema_is_refused_without_touching_the_database() {
        let (path, _dir) = temp_db("newer");
        seeded(&path);
        {
            let w = open_writer(&path).unwrap();
            w.execute("INSERT INTO grdb_migrations (identifier) VALUES ('v99_from_the_future')", [])
                .unwrap();
            checkpoint_truncate(&w).unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let err = open_writer(&path).expect_err("newer schema must be refused");
        assert!(err.downcast_ref::<migrations::NewerThanEngine>().is_some(), "{err:#}");
        drop(err);
        let r = open_read(&path).unwrap();
        let files: i64 = r.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0)).unwrap();
        assert_eq!(files, 1);
        drop(r);
        assert_eq!(std::fs::read(&path).unwrap(), before, "refusal must not rewrite the file");
    }

    #[test]
    fn a_foreign_user_version_is_preserved_and_data_survives() {
        let (path, _dir) = temp_db("user-version");
        seeded(&path);
        {
            let w = open_writer(&path).unwrap();
            w.execute_batch("PRAGMA user_version = 4096").unwrap();
        }
        let w = open_writer(&path).expect("a high user_version is not a schema version");
        let version: i64 = w.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version & !1, 4096);
        let files: i64 = w.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0)).unwrap();
        assert_eq!(files, 1);
    }

    #[test]
    fn garbage_and_truncated_files_fail_cleanly() {
        let (path, _dir) = temp_db("garbage");
        std::fs::write(&path, b"this is not a sqlite database, just some bytes .....").unwrap();
        assert!(open_writer(&path).is_err());

        let (path, _dir2) = temp_db("truncated");
        seeded(&path);
        let bytes = std::fs::read(&path).unwrap();
        for cut in [0usize, 50, 100, 4096, bytes.len() / 2, bytes.len() - 1] {
            let cut = cut.min(bytes.len());
            std::fs::write(&path, &bytes[..cut]).unwrap();
            let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
            let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
            if let Ok(conn) = open_writer(&path) {
                let _ = quick_check(&conn);
                let _ = conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0));
            }
        }
    }

    #[test]
    fn the_reader_pool_reuses_connections_and_never_writes() {
        let (path, _dir) = temp_db("pool");
        seeded(&path);
        let pool = ReaderPool::new(2);
        {
            let first = pool.get(&path).unwrap();
            let second = pool.get(&path).unwrap();
            let third = pool.get(&path).unwrap();
            assert_eq!(pool.idle_count(), 0);
            assert!(first.execute("DELETE FROM files", []).is_err(), "pooled connections are read-only");
            let _ = (second.query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0)).unwrap(), third);
        }
        assert_eq!(pool.idle_count(), 2, "at most `capacity` connections are kept");
        let writer = Connection::open(&path).unwrap();
        writer.execute("UPDATE files SET size_bytes=4242", []).unwrap();
        let reused = pool.get(&path).unwrap();
        assert_eq!(pool.idle_count(), 1);
        let seen: i64 = reused.query_row("SELECT MAX(size_bytes) FROM files", [], |row| row.get(0)).unwrap();
        assert_eq!(seen, 4242, "a reused connection sees later commits");
        reused.execute_batch("BEGIN").unwrap();
        drop(reused);
        assert_eq!(pool.idle_count(), 1, "a connection left in a transaction is closed, not reused");

        let (other, _other_dir) = temp_db("pool-other");
        seeded(&other);
        drop(pool.get(&other).unwrap());
        assert_eq!(pool.idle_count(), 1, "switching databases drops the old one's connections");
        assert!(pool.idle.lock().iter().all(|(held, _)| *held == other));
        pool.clear();
        assert_eq!(pool.idle_count(), 0);

        let memory = parking_lot::Mutex::new(Connection::open_in_memory().unwrap());
        assert!(reader_for(&memory).unwrap().is_none());
        let file = parking_lot::Mutex::new(Connection::open(&path).unwrap());
        let shared = reader_for(&file).unwrap().expect("file databases get a reader");
        assert!(shared.execute("DELETE FROM files", []).is_err());
    }

    fn many_rows(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(id INTEGER PRIMARY KEY, body TEXT);").unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        for id in 0..4000 {
            tx.execute("INSERT INTO t VALUES(?1, ?2)", rusqlite::params![id, "x".repeat(200)]).unwrap();
        }
        tx.commit().unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    }

    #[test]
    fn the_background_check_passes_a_sound_database_and_reports_a_damaged_one() {
        let (path, _dir) = temp_db("integrity-bg");
        many_rows(&path);
        assert_eq!(IntegrityCheck::default().run(&path), None);

        let mut bytes = std::fs::read(&path).unwrap();
        let page = 4096;
        assert!(bytes.len() > page * 8);
        let start = bytes.len() / 2 / page * page;
        bytes[start..start + page].fill(0xA5);
        std::fs::write(&path, &bytes).unwrap();
        let detail = IntegrityCheck::default().run(&path).expect("damage is reported");
        assert!(!detail.is_empty());
        assert!(quick_check(&Connection::open(&path).unwrap()).is_err());
    }

    #[test]
    fn a_cancelled_background_check_reports_nothing_and_releases_its_connection() {
        let (path, _dir) = temp_db("integrity-cancel");
        many_rows(&path);
        let mut bytes = std::fs::read(&path).unwrap();
        let start = bytes.len() / 2 / 4096 * 4096;
        bytes[start..start + 4096].fill(0xA5);
        std::fs::write(&path, &bytes).unwrap();

        let check = IntegrityCheck::default();
        check.cancel();
        assert_eq!(check.run(&path), None);
        check.cancel();

        let skipped = IntegrityCheck::default();
        skipped.skip();
        let started = std::time::Instant::now();
        skipped.cancel();
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }

    #[test]
    fn cancelling_from_another_thread_ends_a_running_check() {
        let (path, _dir) = temp_db("integrity-race");
        many_rows(&path);
        for _ in 0..20 {
            let check = std::sync::Arc::new(IntegrityCheck::default());
            let runner = {
                let check = check.clone();
                let path = path.clone();
                std::thread::spawn(move || check.run(&path))
            };
            check.cancel();
            assert_eq!(runner.join().unwrap(), None);
            let writer = Connection::open(&path).unwrap();
            let busy: i64 = writer.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0)).unwrap();
            assert_eq!(busy, 0);
        }
    }

    #[test]
    fn bit_flipped_pages_never_panic() {
        let (path, _dir) = temp_db("bitflip");
        seeded(&path);
        let pristine = std::fs::read(&path).unwrap();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..40 {
            let mut bytes = pristine.clone();
            for _ in 0..8 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let at = (state as usize) % bytes.len();
                bytes[at] ^= 1 << (state >> 61);
            }
            std::fs::write(&path, &bytes).unwrap();
            let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
            let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
            if let Ok(conn) = open_writer(&path) {
                let _ = quick_check(&conn);
                let _ = wipe_all(&conn);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_database_is_refused_with_a_clear_error() {
        use std::os::unix::fs::PermissionsExt;
        let (path, _dir) = temp_db("read-only");
        seeded(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let enforced = std::fs::OpenOptions::new().write(true).open(&path).is_err();
        let result = open_writer(&path);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        if enforced {
            let err = result.expect_err("a read-only database must not open as the writer");
            assert!(err.downcast_ref::<ReadOnlyDatabase>().is_some(), "{err:#}");
        }
    }

    #[test]
    fn the_writer_waits_out_another_connections_lock() {
        let (path, _dir) = temp_db("busy");
        seeded(&path);
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            holder.execute_batch("COMMIT").unwrap();
        });
        let w = open_writer(&path).expect("busy_timeout must outlast a short lock");
        w.execute("UPDATE files SET size_bytes = 2", []).expect("write after the lock clears");
        release.join().unwrap();
    }

    #[test]
    fn a_full_disk_rolls_the_migration_back_and_reports_it() {
        let (path, _dir) = temp_db("full");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA max_page_count = 8").unwrap();
            assert!(migrations::apply(&conn).is_err(), "migrations cannot fit in 8 pages");
            let applied: i64 = conn
                .query_row("SELECT COUNT(*) FROM grdb_migrations", [], |row| row.get(0))
                .unwrap_or(0);
            let tables: i64 = conn
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = 'files'", [], |row| row.get(0))
                .unwrap();
            assert_eq!(tables > 0, applied > 0, "a failed migration must not leave half its schema");
        }
        open_writer(&path).expect("the next open finishes the migrations");
    }

    #[test]
    fn wipe_all_keeps_the_vector_state_the_triggers_need() {
        let conn = crate::db::test_support::memory_db();
        conn.execute_batch(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES (1, '/lib/a.jpg', 1, 1, 0, 'image', 'jpg'); \
             INSERT INTO clip_embeddings (file_id, embedding, model) VALUES (1, x'00', 'm');",
        )
        .unwrap();
        let namespaces = |conn: &Connection| -> i64 { conn.query_row("SELECT COUNT(*) FROM catalog_vector_state", [], |r| r.get(0)).unwrap() };
        let before = namespaces(&conn);
        assert!(before > 0);
        let old_instance: String = conn.query_row("SELECT instance_id FROM catalog_vector_state WHERE namespace='clip'", [], |r| r.get(0)).unwrap();
        wipe_all(&conn).expect("wipe");
        assert_eq!(namespaces(&conn), before, "the state rows must survive a wipe");
        let (instance, generation, changes): (String, i64, i64) = conn
            .query_row(
                "SELECT instance_id, generation, (SELECT COUNT(*) FROM catalog_vector_changes) FROM catalog_vector_state WHERE namespace='clip'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_ne!(instance, old_instance, "a new instance id drops cached indexes");
        assert_eq!((generation, changes), (0, 0));
        conn.execute_batch(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES (1, '/lib/b.jpg', 2, 1, 0, 'image', 'jpg'); \
             INSERT INTO clip_embeddings (file_id, embedding, model) VALUES (1, x'00', 'm');",
        )
        .unwrap();
        let generation: i64 = conn.query_row("SELECT generation FROM catalog_vector_state WHERE namespace='clip'", [], |r| r.get(0)).unwrap();
        assert!(generation >= 1, "a new embedding after the wipe is recorded");
    }

    #[test]
    fn wipe_all_empties_every_fts_kind() {
        let conn = crate::db::test_support::memory_db();
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
