use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::params;

use super::restructure_apply::RestructureApply;
use crate::ipc::RestructureMove;

fn plan(file_id: i64, source: &Path, destination: &Path) -> RestructureMove {
    RestructureMove {
        file_id,
        source: source.to_string_lossy().into_owned(),
        destination: destination.to_string_lossy().into_owned(),
        category: "Sorted".to_string(),
        confidence: String::new(),
        ..Default::default()
    }
}

fn catalog_row(conn: &rusqlite::Connection, id: i64, path: &Path) {
    let file_ref = crate::platform::file_ref(path).map(|value| value as i64);
    conn.execute(
        "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, failed, file_ref) \
         VALUES (?1, ?2, ?1, 4, 0.0, 'image', 'jpg', 0, ?3)",
        params![id, path.to_string_lossy(), file_ref],
    )
    .unwrap();
}

// Path equality follows the mounted volume: on a case-insensitive volume a
// case-only spelling is the same file, on a case-sensitive one it is another
// file and a plan that names the wrong one must not move or overwrite anything.
#[test]
fn a_case_only_spelling_of_the_planned_path_follows_the_volumes_case_rules() {
    let root = std::env::temp_dir().join(format!("fileid-case-only-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let planned = root.join("Report.jpg");
    let live = root.join("report.jpg");
    std::fs::write(&planned, b"planned-original").unwrap();
    let case_insensitive = live.exists();
    if !case_insensitive {
        std::fs::write(&live, b"replacement").unwrap();
    }

    let conn = crate::db::test_support::memory_db();
    catalog_row(&conn, 1, &live);
    let apply = RestructureApply::new(Arc::new(Mutex::new(conn)), root.clone(), false);
    let destination = root.join("Sorted").join("report.jpg");
    let result = apply.apply(&[plan(1, &planned, &destination)]).unwrap();

    if case_insensitive {
        assert_eq!((result.applied, result.failed), (1, 0), "the spelling names the same file");
        assert_eq!(std::fs::read(&destination).unwrap(), b"planned-original");
        assert!(!planned.exists());
    } else {
        assert_eq!((result.applied, result.failed), (0, 1), "a distinct file is not the planned one");
        assert_eq!(std::fs::read(&planned).unwrap(), b"planned-original");
        assert_eq!(std::fs::read(&live).unwrap(), b"replacement");
        assert!(!destination.exists());
    }
    std::fs::remove_dir_all(root).ok();
}
