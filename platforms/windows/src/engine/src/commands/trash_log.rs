//! Sidecar undo log for `trashFiles` -> `restoreFromTrash`: append-only NDJSON capped at the last 1024 entries so the
//! UndoStack survives restarts without bloating the SQLite schema. The canonical Trash journal on every platform: the
//! Swift engine dual-writes it and `journal_import` converts its older `trash-journal.json`.

use std::collections::BTreeMap;
use std::io::Write;

use crate::paths;
use crate::util::hmac;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct TrashLogEntry {
    pub(crate) batch_id: String,
    pub(crate) timestamp: f64,
    pub(crate) items: Vec<TrashLogItem>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct TrashLogItem {
    pub(crate) file_id: i64,
    pub(crate) original_path: String,
    /// So a put back still finds its anchor after the catalog (and its scan sessions) was wiped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) library_root: Option<String>,
    /// Identity-bound deletion first claims a file under a unique sibling name; Windows restore looks up that claimed
    /// path, then renames it back to `original_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recycle_bin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recycle_physical_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_identity: Option<crate::platform::FileIdentity>,
    /// A write-ahead entry has none; restore and startup recovery then find the item by its inode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) trashed_path: Option<String>,
    /// Put back on restore so tags, CLIP vector and take signals return at once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) rows: Vec<SnapshotRow>,
    /// Set by the closing line of a batch that finished without moving this item, so startup recovery never reads a
    /// finished batch's write-ahead line as an interrupted one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) settled: bool,
}

impl TrashLogItem {
    #[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
    pub(crate) fn expected_inode(&self) -> Option<u64> {
        self.source_identity.map(|identity| identity.file).or_else(|| {
            self.rows
                .iter()
                .find(|row| row.table == "files")
                .and_then(|row| row.columns.get("file_ref"))
                .and_then(|value| value.int)
                .map(|inode| inode as u64)
        })
    }
}

/// `files` comes first so the other rows' foreign keys resolve on restore.
pub(crate) const SNAPSHOT_TABLES: [(&str, &str); 5] = [
    ("files", "id"),
    ("clip_embeddings", "file_id"),
    ("tags", "file_id"),
    ("take_quality", "file_id"),
    ("face_prints", "file_id"),
];

/// Same JSON shape as Swift's `TrashJournal.Value` (null is `{}`, blobs standard base64) so either engine reads the
/// other's.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SnapshotValue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) int: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) real: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) blob: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SnapshotRow {
    pub(crate) table: String,
    pub(crate) columns: BTreeMap<String, SnapshotValue>,
}

impl SnapshotValue {
    fn from_sql(value: rusqlite::types::ValueRef<'_>) -> Self {
        use base64::Engine as _;
        use rusqlite::types::ValueRef;
        match value {
            ValueRef::Null => Self::default(),
            ValueRef::Integer(int) => Self { int: Some(int), ..Self::default() },
            ValueRef::Real(real) => Self { real: Some(real), ..Self::default() },
            ValueRef::Text(text) => Self {
                text: Some(String::from_utf8_lossy(text).into_owned()),
                ..Self::default()
            },
            ValueRef::Blob(blob) => Self {
                blob: Some(base64::engine::general_purpose::STANDARD.encode(blob)),
                ..Self::default()
            },
        }
    }

    fn to_sql(&self) -> anyhow::Result<rusqlite::types::Value> {
        use base64::Engine as _;
        use rusqlite::types::Value;
        Ok(if let Some(int) = self.int {
            Value::Integer(int)
        } else if let Some(real) = self.real {
            Value::Real(real)
        } else if let Some(text) = &self.text {
            Value::Text(text.clone())
        } else if let Some(blob) = &self.blob {
            Value::Blob(base64::engine::general_purpose::STANDARD.decode(blob)?)
        } else {
            Value::Null
        })
    }
}

fn table_columns(conn: &rusqlite::Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare_cached("SELECT name FROM pragma_table_info(?1)")?;
    let names = stmt.query_map([table], |row| row.get::<_, String>(0))?;
    names.collect()
}

pub(crate) fn snapshot_rows(
    conn: &rusqlite::Connection,
    file_id: i64,
) -> rusqlite::Result<Vec<SnapshotRow>> {
    let mut out = Vec::new();
    for (table, key) in SNAPSHOT_TABLES {
        if table_columns(conn, table)?.is_empty() {
            continue;
        }
        let mut stmt = conn.prepare_cached(&format!("SELECT * FROM \"{table}\" WHERE \"{key}\" = ?1"))?;
        let names: Vec<String> = stmt.column_names().map(str::to_owned).collect();
        let mut rows = stmt.query([file_id])?;
        while let Some(row) = rows.next()? {
            let mut columns = BTreeMap::new();
            for (index, name) in names.iter().enumerate() {
                columns.insert(name.clone(), SnapshotValue::from_sql(row.get_ref(index)?));
            }
            out.push(SnapshotRow { table: table.to_owned(), columns });
        }
    }
    Ok(out)
}

/// As Swift: only when neither the id nor the path is taken, every row `INSERT OR IGNORE`, only columns this catalog
/// still has. Returns whether the `files` row was inserted.
pub(crate) fn restore_snapshot(
    conn: &rusqlite::Connection,
    file_id: i64,
    original_path: &str,
    rows: &[SnapshotRow],
) -> anyhow::Result<bool> {
    if !rows.iter().any(|row| row.table == "files") {
        return Ok(false);
    }
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 OR path_text = ?2)",
        rusqlite::params![file_id, original_path],
        |row| row.get(0),
    )?;
    if taken {
        return Ok(false);
    }
    let mut inserted_file = false;
    let mut restored_persons = Vec::new();
    for row in rows {
        let Some((table, key)) = SNAPSHOT_TABLES.iter().find(|(table, _)| *table == row.table) else {
            continue;
        };
        if row.columns.get(*key).and_then(|value| value.int) != Some(file_id) {
            anyhow::bail!("Trash snapshot row for {table} names another file");
        }
        let existing = table_columns(conn, table)?;
        let names: Vec<&String> = row.columns.keys().filter(|name| existing.contains(name)).collect();
        if names.is_empty() {
            continue;
        }
        let mut values = names
            .iter()
            .map(|name| row.columns[*name].to_sql())
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut face_person = None;
        if *table == "face_prints"
            && let Some(slot) = names.iter().position(|name| name.as_str() == "person_id")
            && let rusqlite::types::Value::Integer(person) = values[slot]
        {
            let person_exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM persons WHERE id = ?1)",
                [person],
                |row| row.get(0),
            )?;
            if person_exists {
                face_person = Some(person);
            } else {
                values[slot] = rusqlite::types::Value::Null;
            }
        }
        let columns = names.iter().map(|name| format!("\"{name}\"")).collect::<Vec<_>>().join(",");
        let marks = vec!["?"; names.len()].join(",");
        let changed = conn.execute(
            &format!("INSERT OR IGNORE INTO \"{table}\"({columns}) VALUES({marks})"),
            rusqlite::params_from_iter(values),
        )?;
        if *table == "files" {
            inserted_file = changed == 1;
            if !inserted_file {
                return Ok(false);
            }
        }
        if changed == 1 && let Some(person) = face_person {
            restored_persons.push(person);
        }
    }
    if !rows.iter().any(|row| row.table == "face_prints") {
        // A journal from before face rows were snapshotted: a NULL `modified_at` keeps the file out of the next scan's
        // skip set, which detects its faces again.
        conn.execute(
            "UPDATE files SET has_faces = 0, modified_at = NULL WHERE id = ?1 AND has_faces <> 0",
            [file_id],
        )?;
    }
    restored_persons.sort_unstable();
    restored_persons.dedup();
    crate::db::person_counts::reconcile(conn, &restored_persons)?;
    Ok(inserted_file)
}

/// Trimming drops whole oldest lines verbatim; retained lines keep their HMAC, so `read_batch` still verifies them.
const MAX_ENTRIES: usize = 1024;

#[cfg(unix)]
fn sync_parent(path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
        if let Some(grandparent) = parent.parent() {
            std::fs::File::open(grandparent)?.sync_all()?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}

pub(crate) fn append(entry: &TrashLogEntry) -> anyhow::Result<()> {
    append_at(&paths::trash_log_path()?, &hmac::log_hmac_key()?, entry)
}

pub(crate) fn append_at(
    path: &std::path::Path,
    key: &[u8],
    entry: &TrashLogEntry,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string(entry)?;
    // HMAC-sign each entry so a local attacker's forged append can't be accepted by restoreFromTrash. Format:
    // `{json}\t{hex_hmac}`.
    let mac = hmac::hmac_sha256_hex(key, json.as_bytes());
    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(file, "{json}\t{mac}")?;
        // Force flush so a crash right after delete-to-trash doesn't lose the entry (which would orphan the Recycle Bin
        // items).
        file.sync_all()?;
    }
    sync_parent(path)?;
    // Best-effort: a trim failure must not fail the delete (the entry is already durably appended).
    trim_to_cap(path).ok();
    Ok(())
}

/// A later receipt supersedes the write-ahead line; oldest batch first.
pub(crate) fn latest_entries(raw: &[u8], key: &[u8]) -> Vec<TrashLogEntry> {
    let mut order: Vec<String> = Vec::new();
    let mut latest: std::collections::HashMap<String, TrashLogEntry> = std::collections::HashMap::new();
    for line in raw.split(|b| *b == b'\n') {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        let Some((payload, mac)) = line.trim_end_matches('\r').split_once('\t') else {
            continue;
        };
        if !hmac::constant_time_eq_str(&hmac::hmac_sha256_hex(key, payload.as_bytes()), mac) {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<TrashLogEntry>(payload) else {
            continue;
        };
        order.retain(|id| id != &entry.batch_id);
        order.push(entry.batch_id.clone());
        latest.insert(entry.batch_id.clone(), entry);
    }
    order.into_iter().filter_map(|id| latest.remove(&id)).collect()
}

fn trim_to_cap(path: &std::path::Path) -> anyhow::Result<()> {
    // Read bytes, not read_to_string: one torn line cut mid-codepoint (power loss before append's fsync) would fail the
    // WHOLE read forever and silently disable the cap. Undecodable lines are dropped per line, like the HMAC-reject
    // path.
    let raw = std::fs::read(path)?;
    let lines: Vec<&str> = raw
        .split(|b| *b == b'\n')
        .filter_map(|l| std::str::from_utf8(l).ok())
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.len() <= MAX_ENTRIES {
        return Ok(());
    }
    let keep = &lines[lines.len() - MAX_ENTRIES..];
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("trash_log.json");
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        for l in keep {
            writeln!(f, "{l}")?;
        }
        f.sync_all()?;
    }
    // std::fs::rename replaces atomically on Windows (MoveFileEx REPLACE).
    std::fs::rename(&tmp, path)?;
    sync_parent(path)?;
    Ok(())
}

pub(crate) fn read_batch(batch_id: &str) -> anyhow::Result<Option<TrashLogEntry>> {
    let path = paths::trash_log_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let key = hmac::log_hmac_key()?;
    // Read bytes, not read_to_string: a torn line ending mid-character (power loss mid-append) would return InvalidData
    // for the whole file and wedge restore for EVERY batch.
    let raw = std::fs::read(&path)?;
    Ok(find_batch_in(&raw, &key, batch_id))
}

fn find_batch_in(raw: &[u8], key: &[u8], batch_id: &str) -> Option<TrashLogEntry> {
    for line in raw.split(|b| *b == b'\n').rev() {
        let Ok(line) = std::str::from_utf8(line) else {
            tracing::warn!("trash_log line is not valid UTF-8 (torn append?) -- skipping");
            continue;
        };
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        // Entries without the HMAC suffix (legacy writes or forged appends) are rejected.
        let Some(tab) = line.find('\t') else {
            tracing::warn!("trash_log entry missing HMAC suffix -- rejecting");
            continue;
        };
        let (payload, expected) = (&line[..tab], &line[tab + 1..]);
        let actual = hmac::hmac_sha256_hex(key, payload.as_bytes());
        if !hmac::constant_time_eq_str(&actual, expected) {
            tracing::warn!("trash_log entry HMAC mismatch -- rejecting forged entry");
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<TrashLogEntry>(payload)
            && entry.batch_id == batch_id {
                return Some(entry);
            }
    }
    None
}

/// Claimed but not yet in the Trash or renamed back. Returns how many.
pub(crate) fn recover_stranded_claims() -> usize {
    let Some(raw) = paths::trash_log_path().ok().and_then(|path| std::fs::read(path).ok()) else {
        return 0;
    };
    hmac::log_hmac_key()
        .map(|key| recover_claims_in(&raw, &key))
        .unwrap_or(0)
}

/// Before the app reads the library: claims a killed `trashFiles` left behind, renames a kill left half done, the Swift
/// Trash journal when the Mac app hosts this engine, and (macOS) catalog rows of files a kill left in the Trash.
/// Returns how many claims were put back.
pub(crate) fn recover_at_startup(
    db: Option<&std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>>,
) -> usize {
    let reclaimed = recover_stranded_claims();
    if let Some(db) = db {
        if let Some(journal) = super::path_moves::journal_path() {
            let repaired = super::path_moves::repair(&journal, &db.lock());
            if repaired > 0 {
                tracing::warn!(repaired, "pointed catalog rows at files a killed rename had already moved");
            }
        }
        if paths::APP_HOSTED {
            super::journal_import::import_swift_journals();
        }
        #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
        {
            if let (Ok(path), Ok(key)) = (paths::trash_log_path(), hmac::log_hmac_key()) {
                let put_back = put_back_interrupted_batch(&path, &key, db, |batch_id| super::trash::restore_batch(db, batch_id));
                if put_back > 0 {
                    tracing::warn!(put_back, "put back files a killed trashFiles had already moved to the Trash");
                }
            }
            let dropped = reconcile_trashed_rows(db, crate::shell::trash::locate);
            if dropped > 0 {
                tracing::warn!(dropped, "dropped catalog rows of files a killed trashFiles left in the Trash");
            }
        }
    }
    reclaimed
}

/// A `trashFiles` killed partway leaves its write-ahead line as the newest word on the batch (no item carries a Trash
/// location) and some files already gone from their folders while their catalog rows remain, since the catalog commits
/// only after every move. Puts the whole batch back, as the Windows claim recovery does, so the user's folder is as it
/// was before the interrupted delete. Returns how many files `restore` put back; a batch it can't find in the Trash stays
/// as it is.
#[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
pub(crate) fn put_back_interrupted_batch(
    path: &std::path::Path,
    key: &[u8],
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    restore: impl Fn(&str) -> anyhow::Result<crate::ipc::BulkActionResult>,
) -> usize {
    let Ok(raw) = std::fs::read(path) else {
        return 0;
    };
    let Some(newest) = latest_entries(&raw, key).pop() else {
        return 0;
    };
    if newest.items.iter().any(|item| item.settled) {
        return 0;
    }
    if newest.items.iter().any(|item| item.trashed_path.is_some()) {
        return 0;
    }
    let stranded: Vec<&TrashLogItem> = newest
        .items
        .iter()
        .filter(|item| {
            std::fs::symlink_metadata(&item.original_path).is_err()
                && db
                    .lock()
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 AND path_text = ?2)",
                        rusqlite::params![item.file_id, item.original_path],
                        |row| row.get::<_, bool>(0),
                    )
                    .unwrap_or(false)
        })
        .collect();
    if stranded.is_empty() {
        return 0;
    }
    if let Err(error) = restore(&newest.batch_id) {
        tracing::warn!(?error, "could not put back an interrupted Trash batch");
        return 0;
    }
    stranded
        .iter()
        .filter(|item| std::fs::symlink_metadata(&item.original_path).is_ok())
        .count()
}

#[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
const RECONCILE_BATCHES: usize = 50;

/// A kill between the Trash move and the catalog commit leaves a row for a file now in the Trash. For the newest
/// batches, drops each row whose file is gone from its path and provably in the Trash (`locate` checks identity), and
/// journals a found location the write-ahead entry lacked.
#[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
pub(crate) fn reconcile_trashed_rows(
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    locate: impl Fn(&TrashLogItem) -> Option<std::path::PathBuf>,
) -> usize {
    let (Ok(path), Ok(key)) = (paths::trash_log_path(), hmac::log_hmac_key()) else {
        return 0;
    };
    let Ok(raw) = std::fs::read(&path) else {
        return 0;
    };
    let entries = latest_entries(&raw, &key);
    let mut dropped = 0;
    for mut entry in entries.into_iter().rev().take(RECONCILE_BATCHES) {
        let mut found_new = false;
        for item in &mut entry.items {
            let ghost = db
                .lock()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 AND path_text = ?2)",
                    rusqlite::params![item.file_id, item.original_path],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap_or(false)
                && std::fs::symlink_metadata(&item.original_path).is_err();
            if !ghost {
                continue;
            }
            let Some(found) = locate(item) else {
                continue;
            };
            let found = found.to_string_lossy().into_owned();
            if item.trashed_path.as_deref() != Some(found.as_str()) {
                item.trashed_path = Some(found);
                found_new = true;
            }
            let conn = db.lock();
            let persons = crate::db::person_counts::persons_of_files(&conn, &[item.file_id]).unwrap_or_default();
            if let Ok(1) = conn.execute(
                "DELETE FROM files WHERE id = ?1 AND path_text = ?2",
                rusqlite::params![item.file_id, item.original_path],
            ) {
                dropped += 1;
                if let Err(error) = crate::db::person_counts::reconcile(&conn, &persons) {
                    tracing::warn!(?error, "person recount after dropping a trashed row failed");
                }
            }
        }
        if found_new && let Err(error) = append_at(&path, &key, &entry) {
            tracing::warn!(?error, "could not journal recovered Trash locations");
        }
    }
    dropped
}

fn recover_claims_in(raw: &[u8], key: &[u8]) -> usize {
    let mut restored = 0;
    for line in raw.split(|b| *b == b'\n') {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        let Some((payload, mac)) = line.trim_end_matches('\r').split_once('\t') else {
            continue;
        };
        if !hmac::constant_time_eq_str(&hmac::hmac_sha256_hex(key, payload.as_bytes()), mac) {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<TrashLogEntry>(payload) else {
            continue;
        };
        for item in entry.items {
            let (Some(claim), Some(identity)) = (item.recycle_bin_id, item.source_identity) else {
                continue;
            };
            let claim = std::path::Path::new(&claim);
            if crate::platform::file_identity(claim) == Some(identity)
                && crate::util::rename_no_replace(claim, std::path::Path::new(&item.original_path))
                    .is_ok()
            {
                restored += 1;
            }
        }
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::hmac::{hmac_sha256_hex, log_hmac_key};

    fn claimed_fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("fileid-claim-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("photo.jpg");
        std::fs::write(&original, b"bytes").unwrap();
        let identity = crate::platform::file_identity(&original).unwrap();
        let claim = dir.join(format!(".fileid-trash-{}", uuid::Uuid::new_v4()));
        std::fs::rename(&original, &claim).unwrap();
        let mut entry = make_entry("claimed");
        entry.items[0].original_path = original.to_string_lossy().into_owned();
        entry.items[0].recycle_bin_id = Some(claim.to_string_lossy().into_owned());
        entry.items[0].source_identity = Some(identity);
        let json = serde_json::to_string(&entry).unwrap();
        let mac = hmac_sha256_hex(&log_hmac_key().unwrap(), json.as_bytes());
        (dir, original, claim, format!("{json}\t{mac}\n"))
    }

    #[test]
    fn startup_puts_a_stranded_trash_claim_back() {
        let (dir, original, claim, raw) = claimed_fixture("back");
        assert_eq!(recover_claims_in(raw.as_bytes(), &log_hmac_key().unwrap()), 1);
        assert_eq!(std::fs::read(&original).unwrap(), b"bytes");
        assert!(!claim.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn stranded_claim_never_replaces_an_occupied_original() {
        let (dir, original, claim, raw) = claimed_fixture("occupied");
        std::fs::write(&original, b"newer").unwrap();
        assert_eq!(recover_claims_in(raw.as_bytes(), &log_hmac_key().unwrap()), 0);
        assert_eq!(std::fs::read(&original).unwrap(), b"newer");
        assert_eq!(std::fs::read(&claim).unwrap(), b"bytes");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unsigned_claim_record_is_ignored() {
        let (dir, original, claim, raw) = claimed_fixture("forged");
        let forged = raw.replace('\t', "\tff");
        assert_eq!(recover_claims_in(forged.as_bytes(), &log_hmac_key().unwrap()), 0);
        assert!(!original.exists() && claim.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    fn make_entry(batch_id: &str) -> TrashLogEntry {
        TrashLogEntry {
            batch_id: batch_id.to_string(),
            timestamp: 1700000000.0,
            items: vec![TrashLogItem {
                file_id: 42,
                original_path: r"C:\Users\u\Pictures\cat.jpg".to_string(),
                recycle_bin_id: None,
                recycle_physical_path: None,
                source_identity: None,
                ..Default::default()
            }],
        }
    }

    #[test]
    fn a_dropped_trashed_row_recounts_its_people() {
        let gone = std::env::temp_dir().join(format!("fileid-gone-{}", uuid::Uuid::new_v4())).join("face.jpg");
        let gone_path = gone.to_string_lossy().into_owned();
        let conn = crate::db::test_support::memory_db();
        crate::db::test_support::insert_file(&conn, 9_001, &gone_path, "image");
        crate::db::test_support::insert_file(&conn, 9_002, "/kept/other.jpg", "image");
        conn.execute_batch(
            "INSERT INTO persons(id,created_at,representative_face_id,file_count) VALUES(1,0,1,2),(2,0,3,1);
             INSERT INTO face_prints(id,file_id,person_id,print_data,bbox) VALUES(1,9001,1,X'00','[]'),(2,9002,1,X'00','[]'),(3,9001,2,X'00','[]');",
        )
        .unwrap();
        let mut entry = make_entry(&format!("recount-{}", uuid::Uuid::new_v4()));
        entry.items[0].file_id = 9_001;
        entry.items[0].original_path = gone_path.clone();
        append(&entry).unwrap();
        let db = std::sync::Arc::new(parking_lot::Mutex::new(conn));

        let dropped = reconcile_trashed_rows(&db, |item| (item.original_path == gone_path).then(|| std::path::PathBuf::from("/Trash/face.jpg")));

        assert_eq!(dropped, 1);
        let conn = db.lock();
        let mut stmt = conn.prepare("SELECT id,file_count,representative_face_id FROM persons ORDER BY id").unwrap();
        let persons: Vec<(i64, i64, Option<i64>)> =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect();
        assert_eq!(persons, vec![(1, 1, Some(2)), (2, 0, None)]);
    }

    fn interrupted_batch_fixture(
        items: usize,
        in_trash: usize,
    ) -> (std::path::PathBuf, std::path::PathBuf, Vec<std::path::PathBuf>, TrashLogEntry, std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>) {
        let dir = std::env::temp_dir().join(format!("fileid-interrupted-trash-{}", uuid::Uuid::new_v4()));
        let trash = dir.join("Trash");
        std::fs::create_dir_all(&trash).unwrap();
        let conn = crate::db::test_support::memory_db();
        let mut entry = make_entry(&format!("interrupted-{}", uuid::Uuid::new_v4()));
        entry.items.clear();
        let mut originals = Vec::new();
        for index in 0..items {
            let original = dir.join(format!("photo-{index}.jpg"));
            std::fs::write(&original, format!("bytes {index}")).unwrap();
            crate::db::test_support::insert_file(&conn, index as i64 + 1, &original.to_string_lossy(), "image");
            entry.items.push(TrashLogItem {
                file_id: index as i64 + 1,
                original_path: original.to_string_lossy().into_owned(),
                ..Default::default()
            });
            if index < in_trash {
                std::fs::rename(&original, trash.join(original.file_name().unwrap())).unwrap();
            }
            originals.push(original);
        }
        (dir, trash, originals, entry, std::sync::Arc::new(parking_lot::Mutex::new(conn)))
    }

    fn put_everything_back_from(trash: &std::path::Path, originals: &[std::path::PathBuf]) -> impl Fn(&str) -> anyhow::Result<crate::ipc::BulkActionResult> {
        let trash = trash.to_path_buf();
        let originals = originals.to_vec();
        move |_| {
            for original in &originals {
                let parked = trash.join(original.file_name().unwrap());
                if parked.exists() {
                    std::fs::rename(parked, original)?;
                }
            }
            Ok(crate::ipc::BulkActionResult { command_id: None, action: "restoreFromTrash".into(), succeeded: 0, failed: 0, messages: Vec::new() })
        }
    }

    #[test]
    fn a_trash_batch_killed_partway_is_put_back_at_the_next_start() {
        let (dir, trash, originals, entry, db) = interrupted_batch_fixture(5, 2);
        let (log, key) = (dir.join("trash_log.json"), b"test key".to_vec());
        append_at(&log, &key, &entry).unwrap();

        let put_back = put_back_interrupted_batch(&log, &key, &db, put_everything_back_from(&trash, &originals));

        assert_eq!(put_back, 2, "the two files already in the Trash came back");
        assert!(originals.iter().all(|original| original.exists()), "the folder is as it was before the delete");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_batch_closed_as_moving_nothing_is_never_put_back_at_the_next_start() {
        let (dir, trash, originals, mut entry, db) = interrupted_batch_fixture(3, 2);
        entry.items.iter_mut().for_each(|item| item.settled = true);
        let (log, key) = (dir.join("trash_log.json"), b"test key".to_vec());
        append_at(&log, &key, &entry).unwrap();

        assert_eq!(put_back_interrupted_batch(&log, &key, &db, put_everything_back_from(&trash, &originals)), 0);
        assert!(originals.iter().any(|original| !original.exists()), "a file the user trashed later stays in the Trash");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_finished_or_untouched_trash_batch_is_left_alone() {
        let called = std::cell::Cell::new(0);
        let count = |_: &str| -> anyhow::Result<crate::ipc::BulkActionResult> {
            called.set(called.get() + 1);
            anyhow::bail!("must not be called")
        };

        let (dir, _trash, _originals, entry, db) = interrupted_batch_fixture(3, 0);
        let (log, key) = (dir.join("trash_log.json"), b"test key".to_vec());
        append_at(&log, &key, &entry).unwrap();
        assert_eq!(put_back_interrupted_batch(&log, &key, &db, count), 0, "nothing left its folder");

        let (dir2, _trash2, _originals2, mut receipted, db2) = interrupted_batch_fixture(3, 3);
        receipted.items[0].trashed_path = Some("/Trash/photo-0.jpg".into());
        let log2 = dir2.join("trash_log.json");
        append_at(&log2, &key, &receipted).unwrap();
        assert_eq!(put_back_interrupted_batch(&log2, &key, &db2, count), 0, "a batch with a receipt finished normally");
        assert_eq!(called.get(), 0);
        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(dir2).ok();
    }

    #[test]
    fn the_recorded_library_root_round_trips_and_older_entries_have_none() {
        let mut entry = make_entry("batch-root");
        let without = serde_json::to_string(&entry).unwrap();
        assert!(!without.contains("library_root"));
        assert_eq!(serde_json::from_str::<TrashLogEntry>(&without).unwrap().items[0].library_root, None);
        entry.items[0].library_root = Some("/lib".into());
        let decoded: TrashLogEntry = serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
        assert_eq!(decoded.items[0].library_root.as_deref(), Some("/lib"));
    }

    #[test]
    fn entry_serde_round_trip() {
        let mut entry = make_entry("batch-1");
        entry.items[0].recycle_physical_path = Some(r"C:\$Recycle.Bin\$Rcat.jpg".into());
        entry.items[0].source_identity = Some(crate::platform::FileIdentity {
            volume: 7,
            file: 42,
        });
        let json = serde_json::to_string(&entry).unwrap();
        let decoded: TrashLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.batch_id, "batch-1");
        assert_eq!(decoded.items.len(), 1);
        assert_eq!(decoded.items[0].file_id, 42);
        assert_eq!(
            decoded.items[0].recycle_physical_path,
            entry.items[0].recycle_physical_path
        );
        assert_eq!(decoded.items[0].source_identity, entry.items[0].source_identity);

        let legacy = json.replace(
            ",\"source_identity\":{\"volume\":7,\"file\":42}",
            "",
        );
        let decoded: TrashLogEntry = serde_json::from_str(&legacy).unwrap();
        assert_eq!(decoded.items[0].source_identity, None);
    }

    #[test]
    fn forged_entry_rejected_by_hmac_check() {
        // A hostile appender's forged line must fail verification.
        let key = log_hmac_key().expect("hmac key");

        let real_entry = make_entry("real-batch");
        let real_json = serde_json::to_string(&real_entry).unwrap();
        let real_mac = hmac_sha256_hex(&key, real_json.as_bytes());

        let forged_entry = make_entry("forged-batch");
        let forged_json = serde_json::to_string(&forged_entry).unwrap();
        let wrong_mac = hmac_sha256_hex(b"different-key", forged_json.as_bytes());

        let lines = vec![
            format!("{real_json}\t{real_mac}"),
            format!("{forged_json}\t{wrong_mac}"),
        ];
        let mut accepted_batches = Vec::new();
        for line in &lines {
            if let Some(idx) = line.find('\t') {
                let payload = &line[..idx];
                let mac_hex = &line[idx + 1..];
                let actual = hmac_sha256_hex(&key, payload.as_bytes());
                if !crate::util::hmac::constant_time_eq_str(&actual, mac_hex) {
                    continue;
                }
                let entry: TrashLogEntry = serde_json::from_str(payload).unwrap();
                accepted_batches.push(entry.batch_id);
            }
        }
        assert_eq!(accepted_batches, vec!["real-batch".to_string()]);
    }

    // A signed entry must be findable by batch id before the bulk trash path commits its irreversible DELETE, or a
    // trashed file could be unrecoverable.
    #[test]
    fn latest_signed_receipt_for_batch_wins() {
        let key = log_hmac_key().expect("hmac key");
        let first = make_entry("undo-receipt");
        let mut receipt = first.clone();
        receipt.items[0].recycle_physical_path = Some(r"C:\$Recycle.Bin\$Rabc.jpg".into());
        let signed = |entry: &TrashLogEntry| {
            let json = serde_json::to_string(entry).unwrap();
            let mac = hmac_sha256_hex(&key, json.as_bytes());
            format!("{json}\t{mac}\n")
        };
        let raw = format!("{}{}", signed(&first), signed(&receipt));

        let found = find_batch_in(raw.as_bytes(), &key, "undo-receipt").unwrap();

        assert_eq!(
            found.items[0].recycle_physical_path,
            receipt.items[0].recycle_physical_path
        );
    }

    #[test]
    fn appended_entry_is_findable_by_batch_id() {
        let key = log_hmac_key().expect("hmac key");
        let entry = make_entry("undo-1");
        let json = serde_json::to_string(&entry).unwrap();
        let mac = hmac_sha256_hex(&key, json.as_bytes());
        let raw = format!("{json}\t{mac}\n");

        let found = find_batch_in(raw.as_bytes(), &key, "undo-1")
            .expect("a signed, appended entry must be recoverable by restore");
        assert_eq!(found.items.len(), 1);
        assert_eq!(found.items[0].original_path, entry.items[0].original_path);

        // A never-appended batch is NOT findable: the bulk path must guarantee the append before claiming the trash
        // succeeded.
        assert!(find_batch_in(raw.as_bytes(), &key, "never-written").is_none());
    }

    fn trashed_faces_catalog() -> rusqlite::Connection {
        let conn = crate::db::test_support::memory_db();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        crate::db::test_support::insert_file(&conn, 1, "/photos/a.jpg", "image");
        crate::db::test_support::insert_file(&conn, 2, "/photos/b.jpg", "image");
        conn.execute_batch(
            "UPDATE files SET has_faces = 1;
             INSERT INTO persons(id,created_at,representative_face_id,file_count) VALUES(1,0,1,2),(2,0,3,1);
             INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,face_quality,excluded)
               VALUES(1,1,1,X'00','[0,0,1,1]',0.9,0),(2,2,1,X'00','[0,0,1,1]',0.8,0),(3,1,2,X'00','[0,0,1,1]',0.7,1);",
        )
        .unwrap();
        conn
    }

    fn face_rows(conn: &rusqlite::Connection) -> Vec<(i64, i64, Option<i64>, Option<f64>, i64)> {
        let mut stmt = conn
            .prepare("SELECT id,file_id,person_id,face_quality,COALESCE(excluded,0) FROM face_prints ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn person_counts(conn: &rusqlite::Connection) -> Vec<(i64, i64, Option<i64>)> {
        let mut stmt = conn.prepare("SELECT id,file_count,representative_face_id FROM persons ORDER BY id").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect()
    }

    #[test]
    fn a_snapshot_keeps_the_files_faces_and_put_back_restores_them() {
        let conn = trashed_faces_catalog();
        let rows = snapshot_rows(&conn, 1).unwrap();
        assert_eq!(rows.iter().filter(|row| row.table == "face_prints").count(), 2);
        let persons = crate::db::person_counts::persons_of_files(&conn, &[1]).unwrap();
        conn.execute("DELETE FROM files WHERE id = 1", []).unwrap();
        crate::db::person_counts::reconcile(&conn, &persons).unwrap();
        assert_eq!(face_rows(&conn).len(), 1);
        assert_eq!(person_counts(&conn), vec![(1, 1, Some(2)), (2, 0, None)]);

        assert!(restore_snapshot(&conn, 1, "/photos/a.jpg", &rows).unwrap());

        assert_eq!(
            face_rows(&conn),
            vec![(1, 1, Some(1), Some(0.9), 0), (2, 2, Some(1), Some(0.8), 0), (3, 1, Some(2), Some(0.7), 1)]
        );
        assert_eq!(person_counts(&conn), vec![(1, 2, Some(2)), (2, 0, Some(3))]);
    }

    #[test]
    fn put_back_leaves_a_face_unassigned_when_its_person_is_gone() {
        let conn = trashed_faces_catalog();
        let rows = snapshot_rows(&conn, 1).unwrap();
        conn.execute("DELETE FROM files WHERE id = 1", []).unwrap();
        conn.execute("UPDATE face_prints SET person_id = NULL WHERE person_id = 2", []).unwrap();
        conn.execute("DELETE FROM persons WHERE id = 2", []).unwrap();

        assert!(restore_snapshot(&conn, 1, "/photos/a.jpg", &rows).unwrap());

        assert_eq!(face_rows(&conn)[2], (3, 1, None, Some(0.7), 1));
        assert_eq!(person_counts(&conn), vec![(1, 2, Some(1))]);
    }

    #[test]
    fn a_journal_from_before_faces_were_snapshotted_still_restores() {
        let conn = trashed_faces_catalog();
        let rows: Vec<SnapshotRow> =
            snapshot_rows(&conn, 1).unwrap().into_iter().filter(|row| row.table != "face_prints").collect();
        conn.execute("DELETE FROM files WHERE id = 1", []).unwrap();

        assert!(restore_snapshot(&conn, 1, "/photos/a.jpg", &rows).unwrap());

        assert_eq!(face_rows(&conn).len(), 1);
        let restored: i64 = conn.query_row("SELECT COUNT(*) FROM files WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(restored, 1);
    }

    #[test]
    fn a_journal_from_before_faces_were_snapshotted_leaves_the_file_for_the_next_scan_to_detect() {
        let conn = trashed_faces_catalog();
        conn.execute("UPDATE files SET modified_at = 5.0", []).unwrap();
        let rows: Vec<SnapshotRow> =
            snapshot_rows(&conn, 1).unwrap().into_iter().filter(|row| row.table != "face_prints").collect();
        conn.execute("DELETE FROM files WHERE id = 1", []).unwrap();

        assert!(restore_snapshot(&conn, 1, "/photos/a.jpg", &rows).unwrap());

        let (has_faces, modified): (i64, Option<f64>) = conn
            .query_row("SELECT has_faces, modified_at FROM files WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((has_faces, modified), (0, None));
        let untouched: (i64, Option<f64>) = conn
            .query_row("SELECT has_faces, modified_at FROM files WHERE id = 2", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(untouched, (1, Some(5.0)));
    }

    #[test]
    fn a_snapshot_with_faces_keeps_its_has_faces_and_modified_at() {
        let conn = trashed_faces_catalog();
        conn.execute("UPDATE files SET modified_at = 5.0", []).unwrap();
        let rows = snapshot_rows(&conn, 1).unwrap();
        conn.execute("DELETE FROM files WHERE id = 1", []).unwrap();

        assert!(restore_snapshot(&conn, 1, "/photos/a.jpg", &rows).unwrap());

        let restored: (i64, Option<f64>) = conn
            .query_row("SELECT has_faces, modified_at FROM files WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(restored, (1, Some(5.0)));
    }
}

#[cfg(test)]
mod reconcile_tests {
    use super::*;
    use crate::util::hmac::{hmac_sha256_hex, log_hmac_key};
    use std::sync::Arc;

    fn entry(batch_id: &str, items: &[(i64, &std::path::Path)]) -> TrashLogEntry {
        TrashLogEntry {
            batch_id: batch_id.to_string(),
            timestamp: 1_700_000_000.0,
            items: items
                .iter()
                .map(|(file_id, path)| TrashLogItem {
                    file_id: *file_id,
                    original_path: path.to_string_lossy().into_owned(),
                    ..Default::default()
                })
                .collect(),
        }
    }

    fn append_raw_signed_by(key: &[u8], entry: &TrashLogEntry) {
        let json = serde_json::to_string(entry).unwrap();
        let line = format!("{json}\t{}\n", hmac_sha256_hex(key, json.as_bytes()));
        let path = paths::trash_log_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        file.write_all(line.as_bytes()).unwrap();
    }

    fn rows(db: &Arc<parking_lot::Mutex<rusqlite::Connection>>) -> Vec<i64> {
        let conn = db.lock();
        let mut stmt = conn.prepare("SELECT id FROM files ORDER BY id").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    // A kill between the Trash move and the catalog commit leaves a row for a file that is now in the Trash.
    #[test]
    fn startup_drops_only_rows_of_signed_batches_whose_file_is_gone_and_found_in_the_trash() {
        let dir = std::env::temp_dir().join(format!("fileid-reconcile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let gone = dir.join("gone.jpg");
        let forged = dir.join("forged.jpg");
        let unfound = dir.join("unfound.jpg");
        let present = dir.join("present.jpg");
        std::fs::write(&present, b"still here").unwrap();

        let conn = crate::db::test_support::memory_db();
        for (id, path) in [(1, &gone), (2, &forged), (3, &unfound), (4, &present)] {
            crate::db::test_support::insert_file(&conn, id, &path.to_string_lossy(), "image");
        }
        conn.execute("INSERT INTO persons (id, name, file_count, created_at) VALUES (9, 'Kim', 1, 0)", []).unwrap();
        conn.execute("INSERT INTO face_prints (file_id, person_id, print_data, bbox) VALUES (1, 9, X'00', '{}')", []).unwrap();
        let db = Arc::new(parking_lot::Mutex::new(conn));

        let key = log_hmac_key().unwrap();
        let batch = uuid::Uuid::new_v4().to_string();
        let signed = entry(&format!("ok-{batch}"), &[(1, &gone), (3, &unfound), (4, &present)]);
        append_at(&paths::trash_log_path().unwrap(), &key, &signed).unwrap();
        append_raw_signed_by(b"another key entirely", &entry(&format!("forged-{batch}"), &[(2, &forged)]));

        let trashed = dir.join("in-the-trash.jpg");
        let found = {
            let (unfound, trashed) = (unfound.clone(), trashed.clone());
            move |item: &TrashLogItem| (item.original_path != unfound.to_string_lossy()).then(|| trashed.clone())
        };
        assert_eq!(reconcile_trashed_rows(&db, found), 1);

        assert_eq!(rows(&db), vec![2, 3, 4], "only the signed, vanished, located file loses its row");
        let count: i64 = db.lock().query_row("SELECT file_count FROM persons WHERE id = 9", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "the person's photo count follows");

        let raw = std::fs::read(paths::trash_log_path().unwrap()).unwrap();
        let journaled = find_batch_in(&raw, &key, &format!("ok-{batch}")).unwrap();
        let first = journaled.items.iter().find(|item| item.file_id == 1).unwrap();
        assert_eq!(first.trashed_path.as_deref(), Some(trashed.to_string_lossy().as_ref()), "the found location is journaled");
        assert!(find_batch_in(&raw, &key, &format!("forged-{batch}")).is_none());
        std::fs::remove_dir_all(dir).ok();
    }
}
