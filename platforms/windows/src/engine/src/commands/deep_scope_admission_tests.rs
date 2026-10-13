use super::*;
use crate::resource_scheduler::{Demand, Priority, ResourceScheduler};
use std::time::Duration;

fn scheduler() -> Arc<ResourceScheduler> {
    ResourceScheduler::new(|| (16384, 12000), 1, 1)
}
async fn wait_for(scheduler: &ResourceScheduler, active: usize, waiting: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while scheduler.snapshot().active != active || scheduler.snapshot().waiting != waiting {
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
}
fn generated_catalog() -> Arc<Mutex<rusqlite::Connection>> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!("fileid-scope-admission-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("generated.png");
    std::fs::write(&path, b"generated scope planning only; not decoded").unwrap();
    println!("Generated scope admission state retained: {}", root.display());
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE files(id INTEGER PRIMARY KEY,path_text TEXT NOT NULL,kind TEXT NOT NULL,failed INTEGER NOT NULL DEFAULT 0)").unwrap();
    for id in 1..=513 {
        db.execute("INSERT INTO files(id,path_text,kind) VALUES(?1,?2,'image')", rusqlite::params![id, path.to_str().unwrap()]).unwrap();
    }
    Arc::new(Mutex::new(db))
}

#[tokio::test]
async fn scope_admission_paged_work_is_charged_and_released_before_model_handoff() {
    let s = scheduler();
    let observed = s.clone();
    let db = generated_catalog();
    let ids = admitted_scope_plan(s.clone(), Arc::new(AtomicBool::new(false)), move |cancelled| {
        let snap = observed.snapshot();
        assert_eq!((snap.memory_mb, snap.cpu, snap.io, snap.active), (128, 1, 1, 1));
        let scope = DeepScope::from_fields_cancelable(None, None, cancelled)?;
        collect_scoped_file_ids_cancelable(&db, &scope, cancelled)
    }).await.unwrap();
    assert_eq!(ids, (1..=513).collect::<Vec<i64>>());
    assert_eq!((s.snapshot().memory_mb, s.snapshot().active), (0, 0));
    let model = s.reserve(Demand { memory_mb: 256, cpu: 1, io: 1 }, Priority::Interactive, || false).unwrap();
    assert_eq!((s.snapshot().memory_mb, s.snapshot().active), (256, 1));
    drop(model);
    assert_eq!(s.snapshot().active, 0);
}

#[tokio::test]
async fn scope_admission_cancelled_waiter_never_runs_or_leaks() {
    let s = scheduler();
    let holder = s.reserve(Demand { cpu: 1, io: 1, ..Demand::default() }, Priority::Interactive, || false).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let invoked = Arc::new(AtomicBool::new(false));
    let flag = invoked.clone();
    let task = tokio::spawn(admitted_scope_plan(s.clone(), cancel.clone(), move |_| { flag.store(true, Ordering::Relaxed); Ok(()) }));
    wait_for(&s, 1, 1).await;
    cancel.store(true, Ordering::Relaxed);
    let error = tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap().unwrap_err();
    assert_eq!(error.downcast_ref::<ScopeError>(), Some(&ScopeError::Cancelled));
    assert!(!invoked.load(Ordering::Relaxed));
    assert_eq!((s.snapshot().active, s.snapshot().waiting, s.snapshot().memory_mb), (1, 0, 0));
    drop(holder);
    assert_eq!(s.snapshot().active, 0);
}

#[tokio::test]
async fn scope_admission_abandoned_waiter_does_not_detach_uncancellable_work() {
    let s = scheduler();
    let holder = s.reserve(Demand { cpu: 1, io: 1, ..Demand::default() }, Priority::Interactive, || false).unwrap();
    let invoked = Arc::new(AtomicBool::new(false));
    let flag = invoked.clone();
    let task = tokio::spawn(admitted_scope_plan(s.clone(), Arc::new(AtomicBool::new(false)), move |_| { flag.store(true, Ordering::Relaxed); Ok(()) }));
    wait_for(&s, 1, 1).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    wait_for(&s, 1, 0).await;
    assert!(!invoked.load(Ordering::Relaxed));
    drop(holder);
}

#[tokio::test]
async fn scope_admission_abandoned_active_work_stays_charged_until_worker_finishes() {
    let s = scheduler();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task = tokio::spawn(admitted_scope_plan(s.clone(), Arc::new(AtomicBool::new(false)), move |cancelled| {
        entered_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !cancelled() { assert!(std::time::Instant::now() < deadline); std::thread::yield_now(); }
        cancel_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        Ok(())
    }));
    tokio::time::timeout(Duration::from_secs(2), entered_rx).await.unwrap().unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), cancel_rx).await.unwrap().unwrap();
    assert_eq!((s.snapshot().memory_mb, s.snapshot().cpu, s.snapshot().io, s.snapshot().active), (128, 1, 1, 1));
    release_tx.send(()).unwrap();
    wait_for(&s, 0, 0).await;
    assert_eq!(s.snapshot().memory_mb, 0);
}

#[tokio::test]
async fn scope_admission_cancellation_between_rows_returns_no_partial_targets() {
    let db = generated_catalog();
    let s = scheduler();
    let error = admitted_scope_plan(s.clone(), Arc::new(AtomicBool::new(false)), move |_| {
        let calls = std::cell::Cell::new(0usize);
        let cancelled = || { calls.set(calls.get() + 1); calls.get() >= 40 };
        let scope = DeepScope::from_fields_cancelable(None, None, &|| false)?;
        collect_scoped_file_ids_cancelable(&db, &scope, &cancelled)
    }).await.unwrap_err();
    assert_eq!(error.downcast_ref::<ScopeError>(), Some(&ScopeError::Cancelled));
    assert_eq!((s.snapshot().active, s.snapshot().waiting), (0, 0));
}

#[tokio::test]
async fn scope_admission_empty_request_bypasses_busy_scheduler_and_loader() {
    let s = ResourceScheduler::shared();
    let holder = s.reserve(Demand { cpu: num_cpus::get().saturating_sub(2).max(1), io: 2, ..Demand::default() }, Priority::Interactive, || false).unwrap();
    let (sink, mut rx) = Sink::channel_for_test(8);
    let db = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
    let request = ipc::DeepAnalyzeAllPayload { model_kind: "generated-no-model".into(), skip_existing: false, tags_only: false, propose_renames: false, file_ids: Some(vec![]), excluded_folders: None };
    tokio::time::timeout(Duration::from_secs(2), handle_deep_analyze_all(sink, db, request, Arc::new(AtomicBool::new(false)))).await.unwrap();
    match rx.recv().await.unwrap().payload {
        EventPayload::DeepAnalyzeComplete(c) => assert_eq!((c.inner.processed, c.inner.failed, c.inner.cancelled), (0, 0, false)),
        _ => panic!("empty request must not acquire planning/model admission"),
    }
    assert!(rx.recv().await.is_none());
    assert_eq!(s.snapshot().waiting, 0);
    drop(holder);
}

#[tokio::test]
async fn scope_admission_precancelled_handler_emits_only_confirmed_planning_terminal() {
    let (sink, mut rx) = Sink::channel_for_test(8);
    let db = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
    let request = ipc::DeepAnalyzeAllPayload { model_kind: "generated-no-model".into(), skip_existing: false, tags_only: false, propose_renames: false, file_ids: None, excluded_folders: None };
    handle_deep_analyze_all(sink, db, request, Arc::new(AtomicBool::new(true))).await;
    match rx.recv().await.unwrap().payload {
        EventPayload::DeepAnalyzeComplete(c) => assert_eq!((c.inner.processed, c.inner.failed, c.inner.cancelled), (0, 0, true)),
        _ => panic!("confirmed planning cancellation must not become a model error"),
    }
    assert!(rx.recv().await.is_none());
}

#[test]
fn scope_admission_path_size_checked_before_selected_or_paged_allocation() {
    let db = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
    db.lock().execute_batch("CREATE TABLE files(id INTEGER PRIMARY KEY,path_text TEXT,kind TEXT,failed INTEGER);INSERT INTO files VALUES(1,CAST(zeroblob(131073) AS TEXT),'image',0)").unwrap();
    for selected in [None, Some(vec![1])] {
        let scope = DeepScope::from_fields_cancelable(selected.as_deref(), None, &|| false).unwrap();
        assert!(collect_scoped_file_ids_cancelable(&db, &scope, &|| false).is_err());
    }
}
