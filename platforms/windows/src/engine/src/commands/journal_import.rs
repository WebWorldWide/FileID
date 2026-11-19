//! One-shot import of the Swift engine's Trash journal when the Mac app switches to this engine: `trash-journal.json`
//! batches become signed `trash_log.json` entries in the shared state folder (`paths::root()`), so only the app-hosted
//! build calls this. The Swift file is left for a switch back.

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

pub(crate) const SWIFT_TRASH_JOURNAL: &str = "trash-journal.json";
const IMPORT_MARKER: &str = "trash-journal.json.imported";

/// Failures are logged and the Swift file stays, so the next start retries (`path-moves.ndjson` is shared by both
/// engines: see `path_moves`).
pub(crate) fn import_swift_journals() {
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
}

fn file_stamp(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{}:{}", metadata.len(), modified.as_nanos()))
}

/// Appends every Swift batch not yet in the log (by batch id; batches the Swift engine dual-wrote are skipped), then
/// records the journal's size and mtime so an unchanged file is not read again.
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
        library_root: None,
        recycle_bin_id: None,
        recycle_physical_path: None,
        source_identity: None,
        trashed_path: (!item.trashed_path.is_empty()).then_some(item.trashed_path),
        rows: item.rows,
    };
    // The Swift journal keeps only the inode; bind the volume too while the item is where the journal says.
    if let Some(inode) = converted.expected_inode() {
        converted.source_identity = [converted.trashed_path.as_deref(), Some(converted.original_path.as_str())]
            .into_iter()
            .flatten()
            .filter_map(|path| crate::platform::file_identity(Path::new(path)))
            .find(|identity| identity.file == inode);
    }
    converted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{rebase_paths, swift_written};

    const FIXTURE_FOLDER: &str = "/private/tmp/fileid-conformance-fixture/journal-trash";
    const BATCH: &str = "258da9fe-545c-4d96-90f4-f9427b7f6a83";

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fileid-import-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // The `trash-journal.json` the Swift engine wrote when it trashed
    // `docs/trash me.txt` (test Trash root), moved into `root`; the item's
    // recorded inode becomes `inode`, the one the file has here.
    fn swift_journal(journal: &str, root: &Path, inode: u64) -> String {
        let mut batches: serde_json::Value = serde_json::from_slice(&std::fs::read(swift_written(journal)).unwrap()).unwrap();
        rebase_paths(&mut batches, FIXTURE_FOLDER, &root.to_string_lossy());
        for row in batches[0]["items"][0]["rows"].as_array_mut().unwrap() {
            if row["table"] == "files" {
                row["columns"]["file_ref"]["int"] = serde_json::json!(inode);
            }
        }
        batches.to_string()
    }

    // The app's last Trash batch made by the Swift engine restores after the switch: imported once, identity-bound,
    // catalog snapshot intact.
    #[test]
    fn swift_trash_batch_imports_once_with_identity_and_snapshot() {
        let root = scratch("trash");
        let trashed = root.join("state/Trash/trash me.txt");
        std::fs::create_dir_all(trashed.parent().unwrap()).unwrap();
        std::fs::write(&trashed, b"trash me\n").unwrap();
        let identity = crate::platform::file_identity(&trashed).unwrap();
        let journal = swift_journal("journals/trash-journal.json", &root, identity.file);
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
        assert_eq!(entries[0].batch_id, BATCH);
        assert_eq!(item.file_id, 1);
        assert_eq!(item.original_path, format!("{}/tree/docs/trash me.txt", root.to_string_lossy()));
        let journal_trashed = format!("{}/state/Trash/trash me.txt", root.to_string_lossy());
        assert_eq!(item.trashed_path.as_deref(), Some(journal_trashed.as_str()));
        assert_eq!(item.source_identity, Some(identity));
        assert_eq!(item.recycle_bin_id, None, "no claim: startup must never move it out of the Trash");
        assert_eq!(item.rows.len(), 2);

        let conn = crate::db::test_support::memory_db();
        assert!(trash_log::restore_snapshot(&conn, 1, &item.original_path, &item.rows).unwrap());
        let tag: String = conn.query_row("SELECT tag FROM tags WHERE file_id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(tag, "keep me");
        std::fs::remove_dir_all(root).ok();
    }

    // A batch that carries faces: the face rows (blob vectors included) restore.
    #[test]
    fn swift_trash_batch_with_faces_restores_them() {
        let root = scratch("faces");
        let folder = FIXTURE_FOLDER.replace("journal-trash", "journal-trash-faces");
        let mut batches: serde_json::Value =
            serde_json::from_slice(&std::fs::read(swift_written("journals/trash-journal-faces.json")).unwrap()).unwrap();
        rebase_paths(&mut batches, &folder, &root.to_string_lossy());
        std::fs::write(root.join(SWIFT_TRASH_JOURNAL), batches.to_string()).unwrap();
        let log = root.join("trash_log.json");
        let key = b"import-test-key";
        assert_eq!(import_swift_trash_journal(&root, &log, key).unwrap(), 1);

        let entries = trash_log::latest_entries(&std::fs::read(&log).unwrap(), key);
        let item = &entries[0].items[0];
        let conn = crate::db::test_support::memory_db();
        conn.execute("INSERT INTO persons(id, name, created_at) VALUES (1, 'A', 0), (2, 'B', 0)", []).unwrap();
        assert!(trash_log::restore_snapshot(&conn, item.file_id, &item.original_path, &item.rows).unwrap());
        let faces: Vec<(i64, Option<i64>, Vec<u8>)> = conn
            .prepare("SELECT id, person_id, arcface_embedding FROM face_prints WHERE file_id = 1 ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(faces, vec![(1, Some(1), vec![0, 0, 128, 63]), (3, Some(2), vec![0, 0, 128, 63])]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn dual_written_batch_is_not_imported_twice() {
        let root = scratch("dual");
        std::fs::write(root.join(SWIFT_TRASH_JOURNAL), swift_journal("journals/trash-journal.json", &root, 1)).unwrap();
        let log = root.join("trash_log.json");
        let key = b"k";
        let dual = TrashLogEntry {
            batch_id: BATCH.into(),
            timestamp: 1.0,
            items: vec![TrashLogItem { file_id: 1, original_path: "x".into(), ..Default::default() }],
        };
        trash_log::append_at(&log, key, &dual).unwrap();
        assert_eq!(import_swift_trash_journal(&root, &log, key).unwrap(), 0);
        assert_eq!(trash_log::latest_entries(&std::fs::read(&log).unwrap(), key)[0].items[0].original_path, "x");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unreadable_swift_journal_is_retried_later() {
        let root = scratch("torn");
        let torn = std::fs::read(swift_written("journals/trash-journal.json")).unwrap();
        std::fs::write(root.join(SWIFT_TRASH_JOURNAL), &torn[..torn.len() / 2]).unwrap();
        assert!(import_swift_trash_journal(&root, &root.join("trash_log.json"), b"k").is_err());
        assert!(!root.join(IMPORT_MARKER).exists());
        std::fs::remove_dir_all(root).ok();
    }
}
