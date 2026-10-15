use super::*;
use crate::models::native_admission::startup_allows_cli_fallback;
use crate::resource_scheduler::{Demand, Priority, ResourceScheduler};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt;

struct GateChild { gate: tokio::sync::watch::Receiver<bool>, kill_fails: bool }
impl NativeChild for GateChild {
    fn start_kill(&mut self) -> std::io::Result<()> {
        if self.kill_fails { Err(std::io::Error::other("generated sticky fallback kill failure")) } else { Ok(()) }
    }
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = std::io::Result<std::process::ExitStatus>> + Send + '_>> {
        Box::pin(async move {
            while !*self.gate.borrow() { self.gate.changed().await.map_err(std::io::Error::other)?; }
            Ok(std::process::ExitStatus::from_raw(0))
        })
    }
}
fn fixture(kill_fails: bool) -> (OwnedProcess, ProcessTracker, tokio::sync::watch::Sender<bool>, Arc<ResourceScheduler>) {
    let s = ResourceScheduler::new(|| (16384, 12000), 1, 1);
    let lease = Arc::new(s.reserve(Demand { memory_mb: 64, cpu: 0, io: 0 }, Priority::Interactive, || false).unwrap());
    let tracker = ProcessTracker::default();
    let (gate, receive) = tokio::sync::watch::channel(false);
    let mut registry = tracker.0.lock();
    let child = OwnedProcess::own(GateChild { gate: receive, kill_fails }, Some(lease), tracker.0.clone(), &mut registry.handles, None, None, Duration::from_secs(5));
    drop(registry);
    (child, tracker, gate, s)
}
async fn wait_reaped(child: &OwnedProcess) {
    tokio::time::timeout(Duration::from_secs(5), async { while child.handle.state.borrow().exit.is_none() { tokio::task::yield_now().await; } }).await.unwrap();
}
fn runtime_error() -> anyhow::Error { anyhow!("generated runtime unavailable").context("candidate startup") }

#[tokio::test]
async fn fallback_guard_sticky_failed_reaped_denies_candidate_and_cli_without_erasing_failure() {
    let (child, tracker, gate, s) = fixture(true);
    child.request_stop();
    tokio::time::timeout(Duration::from_secs(5), async { while child.handle.state.borrow().failure.is_none() { tokio::task::yield_now().await; } }).await.unwrap();
    assert_eq!(s.snapshot().memory_mb, 64); assert!(tracker.has_unreaped()); assert!(!tracker.allows_fallback());
    gate.send_replace(true); wait_reaped(&child).await;
    assert_eq!(s.snapshot().memory_mb, 0); assert!(!tracker.has_unreaped()); assert!(!tracker.allows_fallback());
    assert!(!startup_allows_cli_fallback(&runtime_error(), &tracker));
    let mut command = Command::new(std::env::current_exe().unwrap()); command.arg("--help");
    assert!(OwnedProcess::spawn(&mut command, None, &tracker).is_err());
    assert!(child.poll_exit().is_err());
    let error = tracker.shutdown().await.unwrap_err(); assert!(error.to_string().contains("generated sticky fallback kill failure"));
    assert!(!tracker.allows_fallback()); assert!(!startup_allows_cli_fallback(&runtime_error(), &tracker));
}
#[tokio::test]
async fn fallback_guard_stopping_unconfirmed_denies_and_retains_until_clean_exit() {
    let (child, tracker, gate, s) = fixture(false); child.request_stop();
    assert!(tracker.has_unreaped()); assert!(!tracker.allows_fallback());
    assert!(!startup_allows_cli_fallback(&runtime_error(), &tracker)); assert_eq!(s.snapshot().memory_mb, 64);
    gate.send_replace(true); wait_reaped(&child).await;
    assert_eq!(s.snapshot().memory_mb, 0); assert!(tracker.allows_fallback());
    assert!(startup_allows_cli_fallback(&runtime_error(), &tracker)); tracker.shutdown().await.unwrap();
}
#[tokio::test]
async fn fallback_guard_ordinary_confirmed_exit_remains_eligible_before_shutdown() {
    let (child, tracker, gate, s) = fixture(false); assert!(!tracker.allows_fallback());
    gate.send_replace(true); wait_reaped(&child).await;
    assert_eq!(s.snapshot().memory_mb, 0); assert!(tracker.allows_fallback());
    assert!(startup_allows_cli_fallback(&runtime_error(), &tracker)); tracker.shutdown().await.unwrap();
}
#[tokio::test]
async fn fallback_guard_empty_shutdown_closes_candidate_and_cli_eligibility() {
    let tracker = ProcessTracker::default(); assert!(tracker.allows_fallback()); tracker.shutdown().await.unwrap();
    assert!(!tracker.has_unreaped()); assert!(!tracker.allows_fallback());
    assert!(!startup_allows_cli_fallback(&runtime_error(), &tracker));
}
#[test]
fn fallback_guard_actual_consumers_preserve_probe_type_and_closed_sticky_eligibility() {
    let server = include_str!("vlm_server.rs");
    assert!(server.contains("process_gpu_device_removed() || !ProcessTracker::shared().allows_fallback()"));
    let commands = include_str!("../commands/deep_analyze.rs");
    let start = commands.find("let probe = match server.as_ref()").unwrap();
    let end = start + commands[start..].find("use_server = false;").unwrap();
    let block = &commands[start..end];
    assert!(block.contains("probe_requests_fallback(")); assert!(!block.contains(".is_err()"));
    let refused = &block[block.find("Err(probe_error) =>").unwrap()..block.find("if server_dead {").unwrap()];
    assert!(refused.find("srv.shutdown().await").unwrap() < refused.find("emit_scope_failure").unwrap());
    assert!(refused.contains("return;")); assert!(block.contains("ProcessTracker::shared().allows_fallback()"));
    assert!(commands.contains("admission_failed || cleanup.is_err() || !crate::models::native_process::ProcessTracker::shared().allows_fallback()"));
}
