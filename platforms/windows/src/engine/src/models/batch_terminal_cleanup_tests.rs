use super::*;
use crate::commands::deep_analyze::emit_batch_terminal_after_cleanup;
use crate::ipc::{sink::Sink, DeepAnalyzeComplete, EventPayload};
use crate::resource_scheduler::{Demand, Priority, ResourceScheduler};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

struct GateChild {
    gate: tokio::sync::watch::Receiver<bool>,
    killed: Arc<AtomicUsize>,
    kill_fails: bool,
}
impl NativeChild for GateChild {
    fn start_kill(&mut self) -> std::io::Result<()> {
        self.killed.fetch_add(1, Ordering::Relaxed);
        if self.kill_fails {
            Err(std::io::Error::other("generated batch cleanup kill failure"))
        } else {
            Ok(())
        }
    }
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = std::io::Result<std::process::ExitStatus>> + Send + '_>> {
        Box::pin(async move {
            while !*self.gate.borrow() {
                self.gate.changed().await.map_err(std::io::Error::other)?;
            }
            Ok(std::process::ExitStatus::from_raw(0))
        })
    }
}
fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "fileid-batch-cleanup-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    println!("Generated batch cleanup state retained: {}", path.display());
    path
}
fn fixture(
    kill_fails: bool,
) -> (
    OwnedProcess,
    ProcessTracker,
    tokio::sync::watch::Sender<bool>,
    Arc<AtomicUsize>,
    Arc<ResourceScheduler>,
) {
    let s = ResourceScheduler::new(|| (16384, 12000), 1, 1);
    let lease = Arc::new(
        s.reserve(
            Demand {
                memory_mb: 64,
                cpu: 0,
                io: 0,
            },
            Priority::Interactive,
            || false,
        )
        .unwrap(),
    );
    let tracker = ProcessTracker::default();
    let killed = Arc::new(AtomicUsize::new(0));
    let (gate, receive) = tokio::sync::watch::channel(false);
    let mut registry = tracker.0.lock();
    let child = OwnedProcess::own(
        GateChild {
            gate: receive,
            killed: killed.clone(),
            kill_fails,
        },
        Some(lease),
        tracker.0.clone(),
        &mut registry.handles,
        None,
        None,
        Duration::from_secs(5),
    );
    drop(registry);
    (child, tracker, gate, killed, s)
}
fn complete(cancelled: bool, failed: u64) -> DeepAnalyzeComplete {
    DeepAnalyzeComplete {
        processed: 4,
        failed,
        cancelled,
        total_seconds: 7.0,
        model_kind: "generated-batch-model".into(),
    }
}
fn assert_complete(event: EventPayload, cancelled: bool, failed: u64) {
    match event {
        EventPayload::DeepAnalyzeComplete(c) => {
            assert_eq!(
                (c.inner.processed, c.inner.failed, c.inner.cancelled),
                (4, failed, cancelled)
            );
            assert_eq!(c.inner.model_kind, "generated-batch-model");
            assert!(c.inner.total_seconds >= 7.0, "cleanup time is added, never subtracted");
        }
        _ => panic!("expected exactly one owned batch completion"),
    }
}
async fn delayed(cancelled: bool, failed: u64, message: Option<&str>) {
    let root = root();
    let (child, tracker, gate, killed, s) = fixture(false);
    let (sink, mut rx) = Sink::channel_for_test(8);
    let reason = message.map(str::to_owned);
    let task = tokio::spawn(async move {
        emit_batch_terminal_after_cleanup(&sink, complete(cancelled, failed), reason, child.shutdown()).await;
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while killed.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(rx.try_recv().is_err());
    assert!(!task.is_finished());
    assert_eq!(s.snapshot().memory_mb, 64);
    assert!(!tracker.allows_fallback());
    gate.send_replace(true);
    task.await.unwrap();
    assert_eq!(s.snapshot().memory_mb, 0);
    if let Some(message) = message {
        match rx.recv().await.unwrap().payload {
            EventPayload::Error(error) => {
                assert_eq!(error.inner.kind, "deep_analyze_failed");
                assert_eq!(error.inner.model_kind.as_deref(), Some("generated-batch-model"));
                assert_eq!(error.inner.message, message);
            }
            _ => panic!("owned failure must precede completion, after clean reap"),
        }
    }
    assert_complete(rx.recv().await.unwrap().payload, cancelled, failed);
    assert!(rx.recv().await.is_none());
    assert!(tracker.allows_fallback());
    tracker.shutdown().await.unwrap();
    std::fs::write(root.join("receipt.json"), serde_json::to_vec(&serde_json::json!({"terminalAndFutureReturnBeforeReap": false, "residentHeldWhilePending": true, "cancelled": cancelled, "failedFiles": failed, "batchReason": message, "cleanedBeforeTerminal": true})).unwrap()).unwrap();
}
#[tokio::test]
async fn batch_cleanup_success_terminal_and_future_return_wait_for_confirmed_reap() {
    delayed(false, 0, None).await;
}
#[tokio::test]
async fn batch_cleanup_cancel_terminal_preserves_counts_after_confirmed_reap() {
    delayed(true, 2, None).await;
}
#[tokio::test]
async fn batch_cleanup_scope_error_terminal_preserves_reason_and_counts_after_reap() {
    delayed(false, 3, Some("generated scope root changed")).await;
}
#[tokio::test]
async fn batch_cleanup_host_error_terminal_preserves_reason_and_cancel_after_reap() {
    delayed(true, 1, Some("generated host admission refused")).await;
}

async fn fault(cancelled: bool, message: Option<&str>) {
    let root = root();
    let (child, tracker, gate, _, s) = fixture(true);
    let resources = child.handle.resources.clone();
    let state = child.handle.state.clone();
    let (sink, mut rx) = Sink::channel_for_test(8);
    emit_batch_terminal_after_cleanup(
        &sink,
        complete(cancelled, 0),
        message.map(str::to_owned),
        child.shutdown(),
    )
    .await;
    match rx.recv().await.unwrap().payload {
        EventPayload::Error(error) => {
            assert_eq!(error.inner.kind, "deep_analyze_failed");
            assert_eq!(error.inner.model_kind.as_deref(), Some("generated-batch-model"));
            assert!(error.inner.message.contains("generated batch cleanup kill failure"));
            if let Some(message) = message {
                assert!(error.inner.message.starts_with(message));
            }
        }
        _ => panic!("cleanup failure cannot emit a clean terminal only"),
    }
    assert_complete(rx.recv().await.unwrap().payload, cancelled, 0);
    assert_eq!(s.snapshot().memory_mb, 64);
    assert!(!resources.lock().reaped);
    assert!(!tracker.allows_fallback());
    gate.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.borrow().exit.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(s.snapshot().memory_mb, 0);
    assert!(tracker.shutdown().await.is_err());
    std::fs::write(root.join("receipt.json"), serde_json::to_vec(&serde_json::json!({"ownedCleanupErrorBeforeCompletion": true, "fabricatedFailedFile": false, "cancelled": cancelled, "unconfirmedResidencyRetainedUntilReap": true, "stickyFailureReportedAtShutdown": true})).unwrap()).unwrap();
}
#[tokio::test]
async fn batch_cleanup_failure_after_success_emits_owned_error_without_fabricating_failed_file() {
    fault(false, None).await;
}
#[tokio::test]
async fn batch_cleanup_failure_preserves_original_reason_and_cancel_without_unconfirmed_release() {
    fault(true, Some("generated original batch error")).await;
}
#[tokio::test]
async fn batch_cleanup_empty_owner_terminal_requires_no_native_side_effect() {
    let root = root();
    let (sink, mut rx) = Sink::channel_for_test(4);
    emit_batch_terminal_after_cleanup(&sink, complete(false, 0), None, async { Ok(()) }).await;
    assert_complete(rx.recv().await.unwrap().payload, false, 0);
    assert!(rx.try_recv().is_err());
    std::fs::write(root.join("receipt.json"), b"{\"nativeOwner\":null,\"terminalCount\":1}").unwrap();
}
#[test]
fn batch_cleanup_actual_handler_terminals_and_dispatch_guard_await_finalization() {
    let commands = include_str!("../commands/deep_analyze.rs");
    let begin = commands
        .find("for (idx, file_id) in file_ids.iter().copied().enumerate()")
        .unwrap();
    let end = begin + commands[begin..].find("#[cfg(test)]").unwrap();
    let body = &commands[begin..end];
    assert_eq!(body.matches("finalize_batch(&sink, &mut server,").count(), 4);
    assert!(!body.contains("sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete"));
    let helper_begin = commands
        .find("pub(crate) async fn emit_batch_terminal_after_cleanup")
        .unwrap();
    let helper_end = commands.find("async fn run_deep_analyze_batch").unwrap();
    let helper = &commands[helper_begin..helper_end];
    assert!(helper.find("cleanup.await").unwrap() < helper.find("emit_scope_failure").unwrap());
    assert!(helper.contains("server.finish(keep).await"));
    assert!(helper.contains("server.take()"));
    assert!(!helper.contains("complete.failed ="));
    assert!(!helper.contains("complete.cancelled ="));
    let main = include_str!("../server.rs");
    let begin = main.find("CommandPayload::DeepAnalyzeAll(payload) =>").unwrap();
    let end = begin
        + main[begin..]
            .find("handle_deep_analyze_all(")
            .unwrap();
    assert!(main[begin..end].contains("let _guard = guard;"));
    assert!(!main[begin..end].contains("drop(guard)"));
}
