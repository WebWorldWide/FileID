//! Write-ahead journal for Library renames, in the macOS Swift engine's
//! `PathMoveJournal` format (`path-moves.ndjson` in the state folder): an
//! intent line, synced, before each move, and a "done" line once the catalog
//! row names the new path. A kill in between leaves the intent; at the next
//! start the row is pointed at the destination when the file there is provably
//! the same one (same inode) and the source is gone, then the file is removed.
//! On the Mac the app-hosted engine shares the file with the Swift engine, so
//! either engine repairs a rename the other left half done.
//!
//! A recovery hint checked against the file's inode, not an authority to move
//! files, so it is not HMAC-signed (like `rename_recover.ndjson`).

use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) const FILE_NAME: &str = "path-moves.ndjson";

#[derive(serde::Serialize, serde::Deserialize)]
struct Intent {
    id: String,
    #[serde(rename = "fileID")]
    file_id: i64,
    from: String,
    to: String,
    #[serde(rename = "fileRef", default, skip_serializing_if = "Option::is_none")]
    file_ref: Option<i64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Line {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    intent: Option<Intent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    done: Option<String>,
}

static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

pub(crate) fn journal_path() -> Option<PathBuf> {
    crate::paths::root().ok().map(|root| root.join(FILE_NAME))
}

fn append(path: &Path, line: &Line, sync: bool) -> std::io::Result<()> {
    let mut text = serde_json::to_string(line).map_err(std::io::Error::other)?;
    text.push('\n');
    let _guard = LOCK.lock();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    if sync {
        file.sync_all()?;
    }
    Ok(())
}

/// Records a rename before it happens. Returns the intent id, or `None` when
/// the journal could not be written (the rename still runs: a rescan heals it
/// through `file_ref`, as before this journal existed).
pub(crate) fn begin_at(journal: &Path, file_id: i64, from: &Path, to: &Path, inode: u64) -> Option<String> {
    let id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let line = Line {
        intent: Some(Intent {
            id: id.clone(),
            file_id,
            from: from.to_string_lossy().into_owned(),
            to: to.to_string_lossy().into_owned(),
            file_ref: Some(inode as i64),
        }),
        done: None,
    };
    match append(journal, &line, true) {
        Ok(()) => Some(id),
        Err(error) => {
            tracing::warn!(?error, "could not write the rename intent; a rescan will heal a kill mid-rename");
            None
        }
    }
}

pub(crate) fn begin(file_id: i64, from: &Path, to: &Path, inode: u64) -> Option<String> {
    begin_at(&journal_path()?, file_id, from, to, inode)
}

/// Marks a rename settled. Unsynced: a lost "done" line only costs one
/// harmless check at the next start.
pub(crate) fn finish_at(journal: &Path, id: &str) {
    let _ = append(journal, &Line { intent: None, done: Some(id.to_owned()) }, false);
}

pub(crate) fn finish(id: &str) {
    if let Some(journal) = journal_path() {
        finish_at(&journal, id);
    }
}

/// Startup repair (the Swift engine's `PathMoveJournal.reconcile`): an intent
/// without its "done" line whose source is gone and whose destination holds
/// the same inode points the catalog row at the destination, if the row still
/// names the source. The journal is then removed. Returns rows repaired.
pub(crate) fn repair(journal: &Path, conn: &rusqlite::Connection) -> usize {
    let raw = {
        let _guard = LOCK.lock();
        match std::fs::read(journal) {
            Ok(raw) => raw,
            Err(_) => return 0,
        }
    };
    let mut intents = Vec::new();
    let mut done = std::collections::HashSet::new();
    for line in raw.split(|byte| *byte == b'\n') {
        let Ok(line) = serde_json::from_slice::<Line>(line) else {
            continue;
        };
        if let Some(intent) = line.intent {
            intents.push(intent);
        }
        if let Some(id) = line.done {
            done.insert(id);
        }
    }
    let mut repaired = 0;
    for intent in intents.iter().filter(|intent| !done.contains(&intent.id)) {
        let moved = std::fs::symlink_metadata(&intent.from).is_err()
            && intent.file_ref.is_some_and(|inode| {
                crate::platform::file_ref(Path::new(&intent.to)) == Some(inode as u64)
            });
        if !moved {
            continue;
        }
        let extension = Path::new(&intent.to)
            .extension()
            .map(|extension| extension.to_string_lossy().to_lowercase())
            .filter(|extension| !extension.is_empty());
        let updated = conn.execute(
            "UPDATE OR ABORT files SET path_text = ?1, path_hash = ?2, path_search = ?3, extension = ?4 \
             WHERE id = ?5 AND path_text = ?6",
            rusqlite::params![
                intent.to,
                crate::util::path_safety::stable_path_hash(&intent.to),
                crate::pipeline::dbwriter::nfc_path_search(&intent.to),
                extension,
                intent.file_id,
                intent.from,
            ],
        );
        if let Ok(1) = updated {
            repaired += 1;
        }
    }
    let _guard = LOCK.lock();
    let _ = std::fs::remove_file(journal);
    repaired
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fileid-path-moves-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // The Swift engine's own lines (JSONEncoder output) repair the same way.
    #[test]
    fn pending_swift_rename_is_repaired_and_the_journal_cleared() {
        let root = scratch("swift");
        let from = root.join("IMG_1.JPG");
        let to = root.join("Beach.JPG");
        let stale = root.join("Other.JPG");
        std::fs::write(&to, b"moved").unwrap();
        std::fs::write(&stale, b"stays").unwrap();
        let inode = crate::platform::file_ref(&to).unwrap() as i64;
        let conn = crate::db::test_support::memory_db();
        crate::db::test_support::insert_file(&conn, 1, &from.to_string_lossy(), "photo");
        crate::db::test_support::insert_file(&conn, 2, &stale.to_string_lossy(), "photo");
        let journal = root.join(FILE_NAME);
        let lines = [
            serde_json::json!({"intent": {"id": "A", "fileID": 1, "from": from.to_string_lossy(), "to": to.to_string_lossy(), "fileRef": inode}}),
            // Finished: never replayed.
            serde_json::json!({"intent": {"id": "B", "fileID": 2, "from": stale.to_string_lossy(), "to": to.to_string_lossy(), "fileRef": inode}}),
            serde_json::json!({"done": "B"}),
        ];
        let mut text: String = lines.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        text.push_str("\n{\"intent\":");
        std::fs::write(&journal, text).unwrap();

        assert_eq!(repair(&journal, &conn), 1);
        let path: String = conn.query_row("SELECT path_text FROM files WHERE id = 1", [], |row| row.get(0)).unwrap();
        let other: String = conn.query_row("SELECT path_text FROM files WHERE id = 2", [], |row| row.get(0)).unwrap();
        assert_eq!(path, to.to_string_lossy());
        assert_eq!(other, stale.to_string_lossy());
        assert!(!journal.exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rename_to_a_different_file_is_not_repaired() {
        let root = scratch("other");
        let from = root.join("a.jpg");
        let to = root.join("b.jpg");
        std::fs::write(&to, b"someone else").unwrap();
        let conn = crate::db::test_support::memory_db();
        crate::db::test_support::insert_file(&conn, 1, &from.to_string_lossy(), "photo");
        let journal = root.join(FILE_NAME);
        let wrong = crate::platform::file_ref(&to).unwrap() + 1;
        assert!(begin_at(&journal, 1, &from, &to, wrong).is_some());
        assert_eq!(repair(&journal, &conn), 0);
        std::fs::remove_dir_all(root).ok();
    }

    // Our own lines: a kill after the move, before the catalog update, heals
    // at startup; a settled rename or one that never moved changes nothing.
    #[test]
    fn rust_intents_round_trip_through_repair() {
        let root = scratch("rust");
        let journal = root.join(FILE_NAME);
        let conn = crate::db::test_support::memory_db();
        let killed_from = root.join("killed.jpg");
        let killed_to = root.join("renamed.jpg");
        let never_from = root.join("never.jpg");
        let never_to = root.join("never-renamed.jpg");
        std::fs::write(&killed_from, b"k").unwrap();
        std::fs::write(&never_from, b"n").unwrap();
        crate::db::test_support::insert_file(&conn, 1, &killed_from.to_string_lossy(), "photo");
        crate::db::test_support::insert_file(&conn, 2, &never_from.to_string_lossy(), "photo");
        let killed_inode = crate::platform::file_ref(&killed_from).unwrap();
        let never_inode = crate::platform::file_ref(&never_from).unwrap();

        begin_at(&journal, 1, &killed_from, &killed_to, killed_inode).unwrap();
        std::fs::rename(&killed_from, &killed_to).unwrap();
        begin_at(&journal, 2, &never_from, &never_to, never_inode).unwrap();
        let settled = begin_at(&journal, 2, &never_from, &never_to, never_inode).unwrap();
        finish_at(&journal, &settled);
        // Swift-compatible keys.
        let first: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&journal).unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(first["intent"]["fileID"], 1);
        assert_eq!(first["intent"]["fileRef"], killed_inode as i64);

        assert_eq!(repair(&journal, &conn), 1);
        let killed: String = conn.query_row("SELECT path_text FROM files WHERE id = 1", [], |row| row.get(0)).unwrap();
        let never: String = conn.query_row("SELECT path_text FROM files WHERE id = 2", [], |row| row.get(0)).unwrap();
        assert_eq!(killed, killed_to.to_string_lossy());
        assert_eq!(never, never_from.to_string_lossy());
        assert!(!journal.exists());
        std::fs::remove_dir_all(root).ok();
    }
}
