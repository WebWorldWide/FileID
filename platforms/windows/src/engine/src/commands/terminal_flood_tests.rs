use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use parking_lot::Mutex;
use rusqlite::params;

use crate::ipc::{self, sink::Sink, EventPayload, IpcEvent};

fn filler() -> IpcEvent {
    IpcEvent::now(EventPayload::PhaseChanged(ipc::Wrap::new(ipc::ScanPhase::Tagging)))
}

fn is_filler(payload: &EventPayload) -> bool {
    matches!(payload, EventPayload::PhaseChanged(phase) if phase.inner == ipc::ScanPhase::Tagging)
}

// The channel holds one event and is refilled with a progress filler the moment
// the reader takes one, so whenever the handler runs the channel is full: a
// droppable send of its terminal event is lost, an awaited one is queued ahead of
// the filler. Single-threaded runtime: nothing runs between a read and its refill.
async fn events_under_a_progress_flood<F, Fut>(run: F) -> Vec<EventPayload>
where
    F: FnOnce(Sink) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (sink, mut rx) = Sink::channel_for_test(1);
    sink.try_send(filler()).unwrap();
    let handler = tokio::spawn(run(sink.clone()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut seen = Vec::new();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        if handler.is_finished() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the handler did not finish");
        let event = rx.recv().await.expect("sink open");
        let _ = sink.try_send(filler());
        if !is_filler(&event.payload) {
            seen.push(event.payload);
        }
    }
    handler.await.unwrap();
    while let Ok(event) = rx.try_recv() {
        if !is_filler(&event.payload) {
            seen.push(event.payload);
        }
    }
    seen
}

fn temp_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("fileid-flood-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("Inbox")).unwrap();
    root
}

fn catalog_with(path: &Path) -> Arc<Mutex<rusqlite::Connection>> {
    let conn = crate::db::test_support::memory_db();
    let file_ref = crate::platform::file_ref(path).map(|value| value as i64);
    conn.execute(
        "INSERT INTO files (id, path_text, path_hash, size_bytes, created_at, modified_at, scanned_at, kind, extension, failed, file_ref) \
         VALUES (1, ?1, 1, 4, 1710504000.0, 1710504000.0, 1710504000.0, 'other', 'bin', 0, ?2)",
        params![path.to_string_lossy(), file_ref],
    )
    .unwrap();
    Arc::new(Mutex::new(conn))
}

#[tokio::test]
async fn an_organize_folders_move_result_reaches_the_app_under_a_progress_flood() {
    let root = temp_root("apply");
    let source = root.join("Inbox").join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = root.join("Sorted").join("a.bin");
    let db = catalog_with(&source);
    let payload = ipc::ApplyRestructurePayload {
        library_root: root.to_string_lossy().into_owned(),
        plan_id: None,
        moves: vec![ipc::RestructureMove {
            file_id: 1,
            source: source.to_string_lossy().into_owned(),
            destination: destination.to_string_lossy().into_owned(),
            category: "Sorted".to_string(),
            ..Default::default()
        }],
        use_symlinks: false,
    };
    let events = events_under_a_progress_flood(move |sink| {
        super::restructure::handle_apply_restructure(sink, db, payload, Arc::new(AtomicBool::new(false)))
    })
    .await;
    let moved = destination.exists();
    std::fs::remove_dir_all(&root).ok();
    let results: Vec<_> = events
        .iter()
        .filter_map(|payload| match payload {
            EventPayload::RestructureApplyResult(result) => Some(result.inner.applied),
            _ => None,
        })
        .collect();
    assert!(moved, "the file was moved");
    assert_eq!(results, vec![1], "the move result is on the wire: {events:?}");
}

#[tokio::test]
async fn an_organize_folders_plan_reaches_the_app_under_a_progress_flood() {
    let root = temp_root("plan");
    let source = root.join("Inbox").join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let db = catalog_with(&source);
    let payload = ipc::PlanRestructurePayload {
        library_root: root.to_string_lossy().into_owned(),
        supports_paged_plans: false,
    };
    let events = events_under_a_progress_flood(move |sink| {
        super::restructure::handle_plan_restructure(sink, db, payload, Arc::new(AtomicBool::new(false)))
    })
    .await;
    std::fs::remove_dir_all(&root).ok();
    let plans: Vec<_> = events
        .iter()
        .filter_map(|payload| match payload {
            EventPayload::RestructurePlan(plan) => Some(plan.inner.moves.len()),
            _ => None,
        })
        .collect();
    assert_eq!(plans, vec![1], "the plan is on the wire: {events:?}");
}

#[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
#[tokio::test]
async fn a_describe_files_completion_reaches_the_app_under_a_progress_flood() {
    let db = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
    let payload = ipc::DeepAnalyzeAllPayload {
        model_kind: "qwen2_5_vl_7b".to_string(),
        skip_existing: false,
        file_ids: Some(Vec::new()),
        excluded_folders: None,
        tags_only: false,
        propose_renames: true,
    };
    let events = events_under_a_progress_flood(move |sink| {
        super::deep_analyze::handle_deep_analyze_all(sink, db, payload, Arc::new(AtomicBool::new(false)))
    })
    .await;
    let completions = events.iter().filter(|payload| matches!(payload, EventPayload::DeepAnalyzeComplete(_))).count();
    assert_eq!(completions, 1, "the completion is on the wire: {events:?}");
}
