//! Sidecar undo log for `trashFiles` → `restoreFromTrash`.
//!
//! `trash_log.json` is an append-only NDJSON file capped at the last 1024
//! entries. Lets the app's UndoStack stay process-local across restarts
//! AND lets `restoreFromTrash` know which paths to bring back from the
//! Recycle Bin without bloating the SQLite schema.
//!
//! This is the canonical Trash journal on every platform (DECISIONS
//! 2026-11-10): the macOS Swift engine dual-writes it, and
//! `journal_import` converts the Swift engine's older `trash-journal.json`.

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
    /// Path the Trash backend recorded as the source. Identity-bound deletion
    /// first claims a file under a unique sibling name; Windows restore looks
    /// up that claimed path, then renames it back to `original_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recycle_bin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recycle_physical_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_identity: Option<crate::platform::FileIdentity>,
    /// Where the macOS Trash put the item, once known. A write-ahead entry has
    /// none; restore and startup recovery then find the item by its inode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) trashed_path: Option<String>,
    /// The file's catalog rows (`SNAPSHOT_TABLES`) as they were before Trash,
    /// put back on restore so tags, CLIP vector and take signals return at once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) rows: Vec<SnapshotRow>,
}

impl TrashLogItem {
    /// The inode the item had when it was trashed: the journaled identity, else
    /// the snapshot's `file_ref` (Swift journals carry only that).
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

/// Catalog tables a Trash snapshot keeps, with the column naming the file.
/// `files` comes first so the other rows' foreign keys resolve on restore.
pub(crate) const SNAPSHOT_TABLES: [(&str, &str); 4] = [
    ("files", "id"),
    ("clip_embeddings", "file_id"),
    ("tags", "file_id"),
    ("take_quality", "file_id"),
];

/// One SQLite value. Same JSON shape as the Swift engine's `TrashJournal.Value`
/// (null is `{}`, blobs are standard base64), so either engine reads the other's.
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

/// The file's rows in every `SNAPSHOT_TABLES` table this catalog has.
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

/// Puts a restored file's snapshot back, as the Swift engine does: only when
/// neither its id nor its path is taken, every row with `INSERT OR IGNORE`,
/// and only columns this catalog still has. Returns whether the `files` row
/// was inserted (false: no snapshot, or the catalog already has the file).
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
        let values = names
            .iter()
            .map(|name| row.columns[*name].to_sql())
            .collect::<anyhow::Result<Vec<_>>>()?;
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
    }
    Ok(inserted_file)
}

/// The append-only log is trimmed to the last `MAX_ENTRIES` lines so it can't
/// grow without bound over a long-lived install (the module doc promises this
/// cap). Trimming drops whole oldest lines verbatim — retained lines keep
/// their HMAC, so `read_batch` still verifies them.
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
    // HMAC-sign each entry so a local attacker who appends a forged
    // entry can't get it accepted by restoreFromTrash. Entry format is
    // `{json}\t{hex_hmac}`.
    let mac = hmac::hmac_sha256_hex(key, json.as_bytes());
    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(file, "{json}\t{mac}")?;
        // Force flush so a crash immediately after delete-to-trash doesn't lose
        // the log entry (which would orphan the Recycle Bin items).
        file.sync_all()?;
    }
    sync_parent(path)?;
    // Enforce the documented cap. Best-effort: a trim failure must not fail the
    // delete (the entry is already durably appended above).
    trim_to_cap(path).ok();
    Ok(())
}

/// Every signed entry, keeping only the newest line of each batch (a later
/// receipt supersedes the write-ahead line), oldest batch first.
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

/// Keep only the last `MAX_ENTRIES` non-empty lines, atomically (temp + rename).
fn trim_to_cap(path: &std::path::Path) -> anyhow::Result<()> {
    // Read bytes, not read_to_string: one torn line cut mid-codepoint (power
    // loss before append's fsync) would otherwise fail the WHOLE read forever
    // and silently disable the cap. Undecodable lines are dropped per-line,
    // like the HMAC-reject path.
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
    // Read bytes, not read_to_string: a torn line ending inside a multi-byte
    // character (power loss mid-append) would otherwise return InvalidData
    // for the whole file and wedge restore for EVERY batch, old and new.
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
        // Split {json}\t{hex_hmac}. Entries without the HMAC suffix
        // (legacy writes or forged appends) are rejected.
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

/// Puts back files a killed `trashFiles` left under their hidden claim name
/// (claimed, but not yet in the Trash or renamed back). Returns how many.
pub(crate) fn recover_stranded_claims() -> usize {
    let Some(raw) = paths::trash_log_path().ok().and_then(|path| std::fs::read(path).ok()) else {
        return 0;
    };
    hmac::log_hmac_key()
        .map(|key| recover_claims_in(&raw, &key))
        .unwrap_or(0)
}

/// Startup repair before the app reads the library: claims a killed
/// `trashFiles` left behind, renames a kill left half done, the Swift engine's
/// Trash journal when the Mac app hosts this engine, and (macOS) catalog rows of files a kill left in the
/// Trash. Returns how many claims were put back.
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
            let dropped = reconcile_trashed_rows(db, crate::shell::trash::locate);
            if dropped > 0 {
                tracing::warn!(dropped, "dropped catalog rows of files a killed trashFiles left in the Trash");
            }
        }
    }
    reclaimed
}

/// Batches startup recovery looks at (the Swift engine keeps 50).
#[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
const RECONCILE_BATCHES: usize = 50;

/// A kill between the Trash move and the catalog commit leaves a row for a file
/// that is now in the Trash. For the newest batches, drops each row whose file
/// is gone from its path and provably in the Trash (`locate` checks identity),
/// and journals a found location the write-ahead entry did not have yet.
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
            if let Ok(1) = db.lock().execute(
                "DELETE FROM files WHERE id = ?1 AND path_text = ?2",
                rusqlite::params![item.file_id, item.original_path],
            ) {
                dropped += 1;
            }
        }
        if found_new && let Err(error) = append_at(&path, &key, &entry) {
            tracing::warn!(?error, "could not journal recovered Trash locations");
        }
    }
    dropped
}

/// Only signed entries count, the claim must still be the journaled file, and an
/// occupied original is never replaced.
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

    // A kill between the claim rename and the Trash move left the file under a hidden name.
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
        // Manually simulate the file format:
        //   {valid_json}\t{valid_hmac}\n
        // Then a hostile appender:
        //   {forged_json}\t{wrong_hmac}\n
        // The hostile line must fail verification.
        let key = log_hmac_key().expect("hmac key");

        let real_entry = make_entry("real-batch");
        let real_json = serde_json::to_string(&real_entry).unwrap();
        let real_mac = hmac_sha256_hex(&key, real_json.as_bytes());

        let forged_entry = make_entry("forged-batch");
        let forged_json = serde_json::to_string(&forged_entry).unwrap();
        let wrong_mac = hmac_sha256_hex(b"different-key", forged_json.as_bytes());

        // Reconstruct the verification logic from read_batch inline so we
        // don't have to touch the filesystem.
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

    // Once an entry is appended (signed), restore can FIND it by batch
    // id. This is the recoverability guarantee the bulk trash path now orders
    // BEFORE the irreversible DELETE commit — so a trashed file always has a
    // findable undo-journal entry (or the whole batch is rolled back + surfaced
    // as an error, never silently unrecoverable). Pure: signs in-memory and
    // round-trips through find_batch_in (no filesystem).
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

        // A batch that was never appended is NOT findable — the bulk path must
        // therefore guarantee the append before claiming the trash succeeded.
        assert!(find_batch_in(raw.as_bytes(), &key, "never-written").is_none());
    }
}
