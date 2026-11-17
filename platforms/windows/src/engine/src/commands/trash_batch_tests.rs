use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::Connection;

use super::bulk::handle_trash_files;
#[cfg(target_os = "macos")]
use super::trash_log;
use crate::ipc::{self, sink::Sink, BulkActionResult, EventPayload};

fn catalog_with(files: &[(i64, &std::path::Path)]) -> Arc<Mutex<Connection>> {
    let conn = crate::db::test_support::memory_db();
    for (id, path) in files {
        let size = std::fs::metadata(path).unwrap().len();
        conn.execute(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, file_ref) \
             VALUES (?1, ?2, ?1, ?3, 0, 'image', 'jpg', ?4)",
            rusqlite::params![id, path.to_string_lossy(), size as i64, crate::platform::file_ref(path).map(|value| value as i64)],
        )
        .unwrap();
    }
    Arc::new(Mutex::new(conn))
}

async fn trash(db: &Arc<Mutex<Connection>>, file_ids: Vec<i64>) -> BulkActionResult {
    let (sink, mut events) = Sink::channel_for_test(8);
    handle_trash_files(sink, db.clone(), ipc::TrashFilesPayload { file_ids, exact_identities: None }).await;
    while let Some(event) = events.recv().await {
        if let EventPayload::BulkActionResult(result) = event.payload {
            return result.inner;
        }
    }
    panic!("trashFiles sent no result");
}

#[cfg(target_os = "macos")]
fn journaled(batch_id: &str) -> Option<trash_log::TrashLogEntry> {
    trash_log::read_batch(batch_id).ok().flatten()
}

// A request that names a file twice trashes it once, answers in first-seen
// order, and gives every refusal a reason the app can group by.
#[tokio::test]
async fn trash_ids_are_unique_in_first_seen_order_and_refusals_carry_a_reason() {
    let db = catalog_with(&[]);
    let result = trash(&db, vec![3, 1, 3, 2, 1]).await;
    let ids: Vec<_> = result.messages.iter().map(|item| item.file_id).collect();
    assert_eq!(ids, vec![Some(3), Some(1), Some(2)]);
    assert_eq!((result.succeeded, result.failed), (0, 3));
    assert!(result.messages.iter().all(|item| !item.ok && item.reason.as_deref() == Some("missing")));
}

// macOS only: the unit-test Trash backends refuse (no test Trash root is set),
// so a file is never really moved and the real Trash is never reached.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn the_recovery_journal_names_every_file_of_a_batch_even_when_its_move_fails() {
    let dir = std::env::temp_dir().join(format!("fileid-trash-batch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let (first, second) = (dir.join("a.jpg"), dir.join("b.jpg"));
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();
    let db = catalog_with(&[(1, &first), (2, &second)]);

    let result = trash(&db, vec![1, 2]).await;

    let batch_id = result.action.strip_prefix("trashFiles:").expect("the result names its batch").to_string();
    let entry = journaled(&batch_id).expect("the batch is journaled before any move");
    let mut paths: Vec<_> = entry.items.iter().map(|item| (item.file_id, item.original_path.clone())).collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![(1, first.to_string_lossy().into_owned()), (2, second.to_string_lossy().into_owned())]
    );
    assert!(entry.items.iter().all(|item| item.source_identity.is_some()), "each item carries the identity restore checks");
    assert_eq!((result.succeeded, result.failed), (0, 2), "the refusing backend moved nothing");
    assert_eq!(std::fs::read(&first).unwrap(), b"first");
    assert_eq!(std::fs::read(&second).unwrap(), b"second");
    let rows: i64 = db.lock().query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 2, "a file that was not trashed keeps its catalog row");
    std::fs::remove_dir_all(dir).ok();
}
