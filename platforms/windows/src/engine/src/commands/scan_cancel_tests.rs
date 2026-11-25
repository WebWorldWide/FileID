use std::sync::Arc;

use parking_lot::Mutex;

use super::scan::handle_start_scan;
use crate::ipc::{self, sink::Sink, EventPayload};

// A cancel that arrives while files are being read ends the scan with a
// Cancelled phase and a scanComplete, and frees the scan slot, so the app never
// waits for a result that will not come and can start the next scan.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_cancelled_part_way_ends_with_cancelled_and_scan_complete_and_frees_its_slot() {
    let root = std::env::temp_dir().join(format!("fileid-scan-cancel-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..400 {
        std::fs::write(root.join(format!("note-{i:03}.txt")), format!("file number {i}")).unwrap();
    }
    let db = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
    let scan_state = Arc::new(Mutex::new(None));
    let (sink, mut events) = Sink::channel_for_test(1 << 14);
    let payload = ipc::StartScanPayload {
        root_path: root.to_string_lossy().into_owned(),
        root_bookmark: None,
        root_display: None,
        rescan: false,
        excluded_paths: None,
    };
    let task = tokio::spawn(handle_start_scan(
        sink,
        db,
        scan_state.clone(),
        payload,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
        None,
    ));

    let mut seen = Vec::new();
    let mut cancelled = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let event = tokio::time::timeout(left, events.recv()).await.expect("the scan ends within a minute");
        let Some(event) = event else { break };
        let label = match &event.payload {
            EventPayload::PhaseChanged(phase) => format!("phase:{:?}", phase.inner),
            EventPayload::ScanComplete(_) => "scanComplete".to_string(),
            EventPayload::Progress(_) => "progress".to_string(),
            EventPayload::Error(error) => format!("error:{}:{}", error.inner.kind, error.inner.message),
            other => format!("{:?}", std::mem::discriminant(other)),
        };
        if !cancelled && label == "progress" && let Some(coord) = scan_state.lock().as_ref() {
            coord.request_cancel();
            cancelled = true;
        }
        let done = label == "scanComplete";
        seen.push(label);
        if done {
            break;
        }
    }
    let completed = task.await.unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert!(cancelled, "the scan reached a point where it could be cancelled: {seen:?}");
    assert!(seen.iter().any(|l| l == "phase:Cancelled"), "a Cancelled phase is reported: {seen:?}");
    assert_eq!(seen.last().map(String::as_str), Some("scanComplete"), "scanComplete is the last event: {seen:?}");
    assert!(!completed, "a cancelled scan does not report that it ran to the end");
    assert!(scan_state.lock().is_none(), "the scan slot is free");
}
