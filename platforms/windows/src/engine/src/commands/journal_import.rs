//! One-shot import of the macOS Swift engine's journals when the Mac app
//! switches to this engine: `trash-journal.json` (Trash undo batches) becomes
//! signed `trash_log.json` entries, and pending `path-moves.ndjson` rename
//! intents are repaired exactly as the Swift engine's startup would. Both live
//! in the shared state folder (`paths::root()`), so only the app-hosted build
//! calls this. The Swift files are left in place for a switch back.

use std::path::Path;

use super::trash_log::{self, SnapshotRow, TrashLogEntry, TrashLogItem};
use crate::paths;
use crate::util::hmac;

#[derive(serde::Deserialize)]
struct SwiftBatch {
    id: String,
    #[serde(rename = "createdAt", default)]
    created_at: f64,
    items: Vec<SwiftItem>,
}

#[derive(serde::Deserialize)]
struct SwiftItem {
    #[serde(rename = "fileID")]
    file_id: i64,
    #[serde(rename = "originalPath")]
    original_path: String,
    #[serde(rename = "trashedPath", default)]
    trashed_path: String,
    #[serde(default)]
    rows: Vec<SnapshotRow>,
}

#[derive(serde::Deserialize)]
struct SwiftIntent {
    #[serde(rename = "id")]
    id: String,
    #[serde(rename = "fileID")]
    file_id: i64,
    from: String,
    to: String,
    #[serde(rename = "fileRef", default)]
    file_ref: Option<i64>,
}

#[derive(serde::Deserialize)]
struct SwiftMoveLine {
    #[serde(default)]
    intent: Option<SwiftIntent>,
    #[serde(default)]
    done: Option<String>,
}

pub(crate) const SWIFT_TRASH_JOURNAL: &str = "trash-journal.json";
pub(crate) const SWIFT_PATH_MOVES: &str = "path-moves.ndjson";
const IMPORT_MARKER: &str = "trash-journal.json.imported";

/// Imports both Swift journals from the state folder. Failures are logged:
/// the Swift files stay, so the next start retries.
pub(crate) fn import_swift_journals(db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>) {
    let Ok(root) = paths::root() else {
        return;
    };
    match (paths::trash_log_path(), hmac::log_hmac_key()) {
        (Ok(log), Ok(key)) => match import_swift_trash_journal(&root, &log, &key) {
            Ok(0) => {}
            Ok(imported) => tracing::info!(imported, "imported Trash batches from the Swift engine's journal"),
            Err(error) => tracing::warn!(?error, "could not import the Swift engine's Trash journal"),
        },
        _ => tracing::warn!("no Trash log or key; the Swift engine's Trash journal was not imported"),
    }
    let repaired = repair_swift_path_moves(&root.join(SWIFT_PATH_MOVES), &db.lock());
    if repaired > 0 {
        tracing::info!(repaired, "repaired renames the Swift engine left half done");
    }
}

fn file_stamp(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{}:{}", metadata.len(), modified.as_nanos()))
}

/// Appends every Swift batch the log does not have yet (by batch id), then
/// records the journal's size and mtime so an unchanged file is not read again.
/// Batches the Swift engine dual-wrote are already in the log and are skipped.
pub(crate) fn import_swift_trash_journal(root: &Path, log: &Path, key: &[u8]) -> anyhow::Result<usize> {
    let swift = root.join(SWIFT_TRASH_JOURNAL);
    let marker = root.join(IMPORT_MARKER);
    let Some(stamp) = file_stamp(&swift) else {
        return Ok(0);
    };
    if std::fs::read_to_string(&marker).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(0);
    }
    let batches: Vec<SwiftBatch> = serde_json::from_slice(&std::fs::read(&swift)?)?;
    let known: std::collections::HashSet<String> = std::fs::read(log)
        .map(|raw| trash_log::latest_entries(&raw, key).into_iter().map(|entry| entry.batch_id).collect())
        .unwrap_or_default();
    let mut imported = 0;
    for batch in batches {
        if batch.items.is_empty() || known.contains(&batch.id) {
            continue;
        }
        let entry = TrashLogEntry {
            batch_id: batch.id,
            timestamp: batch.created_at,
            items: batch.items.into_iter().map(convert_item).collect(),
        };
        trash_log::append_at(log, key, &entry)?;
        imported += 1;
    }
    std::fs::write(&marker, stamp)?;
    Ok(imported)
}

fn convert_item(item: SwiftItem) -> TrashLogItem {
    let mut converted = TrashLogItem {
        file_id: item.file_id,
        original_path: item.original_path,
        recycle_bin_id: None,
        recycle_physical_path: None,
        source_identity: None,
        trashed_path: (!item.trashed_path.is_empty()).then_some(item.trashed_path),
        rows: item.rows,
    };
    // The Swift journal keeps only the inode; bind the volume too while the
    // item is where the journal says (in the Trash, or never moved).
    if let Some(inode) = converted.expected_inode() {
        converted.source_identity = [converted.trashed_path.as_deref(), Some(converted.original_path.as_str())]
            .into_iter()
            .flatten()
            .filter_map(|path| crate::platform::file_identity(Path::new(path)))
            .find(|identity| identity.file == inode);
    }
    converted
}

/// The Swift engine's `PathMoveJournal.reconcile`: a rename intent without its
/// "done" line whose source is gone and whose destination holds the same inode
/// points the catalog row at the destination. The journal is then removed, as
/// the Swift engine removes it after its own pass. Returns rows repaired.
pub(crate) fn repair_swift_path_moves(journal: &Path, conn: &rusqlite::Connection) -> usize {
    let Ok(raw) = std::fs::read(journal) else {
        return 0;
    };
    let mut intents = Vec::new();
    let mut done = std::collections::HashSet::new();
    for line in raw.split(|byte| *byte == b'\n') {
        let Ok(line) = serde_json::from_slice::<SwiftMoveLine>(line) else {
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
    let _ = std::fs::remove_file(journal);
    repaired
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fileid-import-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // A Swift `trash-journal.json` exactly as `JSONEncoder` writes `[TrashJournal.Batch]`.
    fn swift_journal(batch: &str, original: &Path, trashed: &Path, inode: u64) -> String {
        serde_json::json!([{
            "id": batch,
            "createdAt": 1_700_000_000.5,
            "items": [{
                "fileID": 7,
                "originalPath": original.to_string_lossy(),
                "trashedPath": trashed.to_string_lossy(),
                "rows": [
                    {"table": "files", "columns": {
                        "id": {"int": 7}, "path_text": {"text": original.to_string_lossy()},
                        "path_hash": {"int": 1}, "size_bytes": {"int": 5}, "scanned_at": {"real": 2.0},
                        "kind": {"text": "photo"}, "extension": {"text": "jpg"}, "modified_at": {"real": 1.0}, "file_ref": {"int": inode as i64}, "vlm_description": {}
                    }},
                    {"table": "tags", "columns": {"file_id": {"int": 7}, "tag": {"text": "beach"},
                        "source": {"text": "user"}, "score": {}}},
                    {"table": "clip_embeddings", "columns": {"file_id": {"int": 7},
                        "embedding": {"blob": "AAECAw=="}, "model": {"text": "clip"}}}
                ]
            }]
        }])
        .to_string()
    }

    // The app's last Trash batch, made by the Swift engine, restores after the
    // switch: it imports once, identity-bound, with its catalog snapshot intact.
    #[test]
    fn swift_trash_batch_imports_once_with_identity_and_snapshot() {
        let root = scratch("trash");
        let library = root.join("lib");
        let trash = root.join("Trash");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&trash).unwrap();
        let original = library.join("photo.jpg");
        let trashed = trash.join("photo.jpg");
        std::fs::write(&trashed, b"bytes").unwrap();
        let identity = crate::platform::file_identity(&trashed).unwrap();
        let journal = swift_journal("SWIFT-BATCH", &original, &trashed, identity.file);
        std::fs::write(root.join(SWIFT_TRASH_JOURNAL), &journal).unwrap();
        let log = root.join("trash_log.json");
        let key = b"import-test-key";

        assert_eq!(import_swift_trash_journal(&root, &log, key).unwrap(), 1);
        assert_eq!(import_swift_trash_journal(&root, &log, key).unwrap(), 0, "unchanged journal is not reread");
        std::fs::remove_file(root.join(IMPORT_MARKER)).unwrap();
        assert_eq!(import_swift_trash_journal(&root, &log, key).unwrap(), 0, "a batch already in the log is skipped");

        let entries = trash_log::latest_entries(&std::fs::read(&log).unwrap(), key);
        assert_eq!(entries.len(), 1);
        let item = &entries[0].items[0];
        assert_eq!(entries[0].batch_id, "SWIFT-BATCH");
        assert_eq!(item.file_id, 7);
        assert_eq!(item.original_path, original.to_string_lossy());
        assert_eq!(item.trashed_path.as_deref(), Some(trashed.to_string_lossy().as_ref()));
        assert_eq!(item.source_identity, Some(identity));
        assert_eq!(item.recycle_bin_id, None, "no claim: startup must never move it out of the Trash");
        assert_eq!(item.rows.len(), 3);

        let conn = crate::db::test_support::memory_db();
        assert!(trash_log::restore_snapshot(&conn, 7, &item.original_path, &item.rows).unwrap());
        let tag: String = conn.query_row("SELECT tag FROM tags WHERE file_id = 7", [], |row| row.get(0)).unwrap();
        let blob: Vec<u8> = conn
            .query_row("SELECT embedding FROM clip_embeddings WHERE file_id = 7", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tag, "beach");
        assert_eq!(blob, vec![0, 1, 2, 3]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn dual_written_batch_is_not_imported_twice() {
        let root = scratch("dual");
        let original = root.join("a.jpg");
        let trashed = root.join("gone.jpg");
        std::fs::write(root.join(SWIFT_TRASH_JOURNAL), swift_journal("DUAL", &original, &trashed, 1)).unwrap();
        let log = root.join("trash_log.json");
        let key = b"k";
        let dual = TrashLogEntry {
            batch_id: "DUAL".into(),
            timestamp: 1.0,
            items: vec![TrashLogItem { file_id: 7, original_path: "x".into(), ..Default::default() }],
        };
        trash_log::append_at(&log, key, &dual).unwrap();
        assert_eq!(import_swift_trash_journal(&root, &log, key).unwrap(), 0);
        assert_eq!(trash_log::latest_entries(&std::fs::read(&log).unwrap(), key)[0].items[0].original_path, "x");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unreadable_swift_journal_is_retried_later() {
        let root = scratch("torn");
        std::fs::write(root.join(SWIFT_TRASH_JOURNAL), b"[{\"id\":").unwrap();
        assert!(import_swift_trash_journal(&root, &root.join("trash_log.json"), b"k").is_err());
        assert!(!root.join(IMPORT_MARKER).exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn pending_swift_rename_is_repaired_and_the_journal_cleared() {
        let root = scratch("moves");
        let from = root.join("IMG_1.JPG");
        let to = root.join("Beach.JPG");
        let stale = root.join("Other.JPG");
        std::fs::write(&to, b"moved").unwrap();
        std::fs::write(&stale, b"stays").unwrap();
        let inode = crate::platform::file_ref(&to).unwrap() as i64;
        let conn = crate::db::test_support::memory_db();
        crate::db::test_support::insert_file(&conn, 1, &from.to_string_lossy(), "photo");
        crate::db::test_support::insert_file(&conn, 2, &stale.to_string_lossy(), "photo");
        let journal = root.join(SWIFT_PATH_MOVES);
        let lines = [
            serde_json::json!({"intent": {"id": "A", "fileID": 1, "from": from.to_string_lossy(), "to": to.to_string_lossy(), "fileRef": inode}}),
            // Finished: never replayed.
            serde_json::json!({"intent": {"id": "B", "fileID": 2, "from": stale.to_string_lossy(), "to": to.to_string_lossy(), "fileRef": inode}}),
            serde_json::json!({"done": "B"}),
        ];
        let mut text: String = lines.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        text.push_str("\n{\"intent\":");
        std::fs::write(&journal, text).unwrap();

        assert_eq!(repair_swift_path_moves(&journal, &conn), 1);
        let path: String = conn.query_row("SELECT path_text FROM files WHERE id = 1", [], |row| row.get(0)).unwrap();
        let other: String = conn.query_row("SELECT path_text FROM files WHERE id = 2", [], |row| row.get(0)).unwrap();
        assert_eq!(path, to.to_string_lossy());
        assert_eq!(other, stale.to_string_lossy());
        assert!(!journal.exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rename_to_a_different_file_is_not_repaired() {
        let root = scratch("moves-other");
        let from = root.join("a.jpg");
        let to = root.join("b.jpg");
        std::fs::write(&to, b"someone else").unwrap();
        let conn = crate::db::test_support::memory_db();
        crate::db::test_support::insert_file(&conn, 1, &from.to_string_lossy(), "photo");
        let journal = root.join(SWIFT_PATH_MOVES);
        let wrong = crate::platform::file_ref(&to).unwrap() as i64 + 1;
        std::fs::write(
            &journal,
            format!("{}\n", serde_json::json!({"intent": {"id": "A", "fileID": 1, "from": from.to_string_lossy(), "to": to.to_string_lossy(), "fileRef": wrong}})),
        )
        .unwrap();
        assert_eq!(repair_swift_path_moves(&journal, &conn), 0);
        std::fs::remove_dir_all(root).ok();
    }
}
