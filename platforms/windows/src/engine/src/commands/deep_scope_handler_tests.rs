use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

struct Fixture {
    root: PathBuf,
    db: Arc<Mutex<rusqlite::Connection>>,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!("fileid-deep-handler-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&root).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE files(id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, kind TEXT NOT NULL, failed INTEGER NOT NULL DEFAULT 0);").unwrap();
        println!("Generated Deep handler fixture retained: {}", root.display());
        Self { root, db: Arc::new(Mutex::new(db)) }
    }
    fn folder(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::create_dir(&path).unwrap();
        path
    }
    fn file(&self, directory: &Path, name: &str) -> PathBuf {
        assert!(directory.starts_with(&self.root));
        let path = directory.join(name);
        std::fs::write(&path, b"generated scope fixture; not decoded or inferred").unwrap();
        path
    }
    fn row(&self, id: i64, path: &Path, kind: &str, failed: i64) {
        self.db.lock().execute("INSERT INTO files(id,path_text,kind,failed) VALUES(?1,?2,?3,?4)", rusqlite::params![id, path.to_str().unwrap(), kind, failed]).unwrap();
    }
}
fn payload(ids: Option<Vec<i64>>, folders: Option<Vec<String>>) -> ipc::DeepAnalyzeAllPayload {
    ipc::DeepAnalyzeAllPayload {
        model_kind: "generated-no-installed-model".into(), skip_existing: false,
        tags_only: false, propose_renames: false, file_ids: ids, excluded_folders: folders,
    }
}

#[test]
fn handler_scope_selected_order_dedup_and_supported_nonfailed_filter() {
    let f = Fixture::new();
    let private = f.folder("private");
    let public = f.folder("public");
    f.row(1, &f.file(&public, "one.png"), "image", 0);
    f.row(2, &f.file(&private, "two.png"), "image", 0);
    f.row(3, &f.file(&public, "audio.bin"), "audio", 0);
    f.row(4, &f.file(&public, "failed.png"), "image", 1);
    let scope = DeepScope::from_fields(Some(&[2, 1, 2, 99, 3, 4]), Some(&[private.to_str().unwrap().into()])).unwrap();
    assert_eq!(collect_scoped_file_ids(&f.db, &scope).unwrap(), vec![2, 1]);
    assert!(scoped_catalog_path(&f.db, &scope, 2).is_ok());
    assert!(scoped_catalog_path(&f.db, &scope, 3).is_err());
}

#[test]
fn handler_scope_whole_library_exclusion_uses_directory_boundaries() {
    let f = Fixture::new();
    let private = f.folder("private");
    let sibling = f.folder("private-sibling");
    f.row(1, &f.file(&private, "private.png"), "image", 0);
    f.row(2, &f.file(&sibling, "public.png"), "image", 0);
    let scope = DeepScope::from_fields(None, Some(&[private.to_str().unwrap().into()])).unwrap();
    assert_eq!(collect_scoped_file_ids(&f.db, &scope).unwrap(), vec![2]);
    assert!(scoped_catalog_path(&f.db, &scope, 1).unwrap_err().downcast_ref::<ScopeError>().is_some());
    assert!(scoped_catalog_path(&f.db, &scope, 2).is_ok());
}

#[test]
fn handler_scope_disappeared_root_fails_before_target_planning() {
    let f = Fixture::new();
    let excluded = f.folder("excluded");
    let scope = DeepScope::from_fields(None, Some(&[excluded.to_str().unwrap().into()])).unwrap();
    std::fs::remove_dir(&excluded).unwrap();
    let error = collect_scoped_file_ids(&f.db, &scope).unwrap_err();
    assert_eq!(error.downcast_ref::<ScopeError>(), Some(&ScopeError::UnavailableFolder));
}

#[tokio::test]
async fn handler_scope_empty_selection_has_only_terminal_without_model_lookup() {
    let f = Fixture::new();
    f.row(1, &f.file(&f.root, "one.png"), "image", 0);
    let (sink, mut rx) = Sink::channel_for_test(8);
    handle_deep_analyze_all(sink, f.db, payload(Some(vec![]), None), Arc::new(AtomicBool::new(false))).await;
    match rx.recv().await.unwrap().payload {
        EventPayload::DeepAnalyzeComplete(c) => {
            assert_eq!((c.inner.processed, c.inner.failed), (0, 0));
            assert!(!c.inner.cancelled);
        }
        _ => panic!("Empty selection must complete without Starting/runtime/model errors"),
    }
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn handler_scope_invalid_fields_emit_owned_error_then_terminal() {
    let f = Fixture::new();
    let (sink, mut rx) = Sink::channel_for_test(8);
    handle_deep_analyze_all(sink, f.db, payload(Some(vec![0]), None), Arc::new(AtomicBool::new(false))).await;
    match rx.recv().await.unwrap().payload {
        EventPayload::Error(e) => {
            assert_eq!(e.inner.kind, "deep_analyze_failed");
            assert_eq!(e.inner.model_kind.as_deref(), Some("generated-no-installed-model"));
            assert!(e.inner.message.contains("positive"));
        }
        _ => panic!("Invalid scope must emit an owned analysis error"),
    }
    match rx.recv().await.unwrap().payload {
        EventPayload::DeepAnalyzeComplete(c) => {
            assert_eq!((c.inner.processed, c.inner.failed), (0, 1));
            assert!(!c.inner.cancelled);
        }
        _ => panic!("Invalid scope must terminate without loading"),
    }
    assert!(rx.recv().await.is_none());
}

#[cfg(unix)]
#[test]
fn handler_scope_retargeted_exclusion_root_stops_current_run() {
    let f = Fixture::new();
    let first = f.folder("first");
    let second = f.folder("second");
    let link = f.root.join("excluded-link");
    std::os::unix::fs::symlink(&first, &link).unwrap();
    f.row(1, &f.file(&second, "one.png"), "image", 0);
    let scope = DeepScope::from_fields(None, Some(&[link.to_str().unwrap().into()])).unwrap();
    assert_eq!(collect_scoped_file_ids(&f.db, &scope).unwrap(), vec![1]);
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&second, &link).unwrap();
    assert_eq!(collect_scoped_file_ids(&f.db, &scope).unwrap_err().downcast_ref::<ScopeError>(), Some(&ScopeError::UnavailableFolder));
}

#[cfg(unix)]
#[tokio::test]
async fn handler_scope_raster_rechecks_alias_and_uses_resolved_source() {
    let f = Fixture::new();
    let private = f.folder("private");
    let allowed = f.folder("allowed");
    let public_file = f.file(&allowed, "public.png");
    let secret_file = f.file(&private, "secret.png");
    let alias = f.root.join("alias.png");
    std::os::unix::fs::symlink(&public_file, &alias).unwrap();
    f.row(1, &alias, "image", 0);
    let scope = DeepScope::from_fields(None, Some(&[private.to_str().unwrap().into()])).unwrap();
    assert_eq!(collect_scoped_file_ids(&f.db, &scope).unwrap(), vec![1]);
    let (source, temporary) = crate::pipeline::deep_analyze::rasterize_for_vlm(&f.db, 1, Some(&scope)).await.unwrap();
    assert_eq!(source, public_file.canonicalize().unwrap());
    assert!(temporary.is_none());
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&secret_file, &alias).unwrap();
    let error = crate::pipeline::deep_analyze::rasterize_for_vlm(&f.db, 1, Some(&scope)).await.unwrap_err();
    assert_eq!(error.downcast_ref::<ScopeError>(), Some(&ScopeError::ExcludedTarget));
}
