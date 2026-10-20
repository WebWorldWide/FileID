use super::*;
use crate::resource_scheduler::{Demand, Priority, ResourceScheduler};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!("fileid-native-owner-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir(&root).unwrap();
    println!("Generated native ownership state retained: {}", root.display());
    root
}
fn command(ready: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "models::native_process::integration_tests::generated_owned_child_fixture", "--ignored", "--nocapture"])
        .env("FILEID_GENERATED_OWNER_READY", ready).env("FILEID_GENERATED_OWNER_MODE", mode)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    command
}
async fn ready(path: &Path) -> u32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Ok(pid) = text.parse() { return pid; }
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap()
}
async fn retired(tracker: &ProcessTracker, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while tracker.0.lock().handles.len() != count { tokio::task::yield_now().await; }
    }).await.unwrap();
}
fn admission() -> (Arc<ResourceScheduler>, Arc<Lease>) {
    let s = ResourceScheduler::new(|| (16384, 12000), 1, 1);
    let lease = Arc::new(s.reserve(Demand { memory_mb: 64, cpu: 1, io: 1 }, Priority::Interactive, || false).unwrap());
    (s, lease)
}

#[tokio::test]
#[ignore = "new generated executable fixture; never a model or installed vendor runtime"]
async fn generated_owned_child_fixture() {
    let ready = PathBuf::from(std::env::var_os("FILEID_GENERATED_OWNER_READY").unwrap());
    let mode = std::env::var("FILEID_GENERATED_OWNER_MODE").unwrap();
    std::fs::write(&ready, std::process::id().to_string()).unwrap();
    if mode == "parent" {
        let grand = ready.with_extension("grandchild");
        let mut cmd = command(&grand, "hold");
        let _child = OwnedProcess::spawn(&mut cmd, None, &ProcessTracker::default()).unwrap();
        let _ = self::ready(&grand).await;
        std::fs::write(ready.with_extension("parent-ready"), std::process::id().to_string()).unwrap();
    }
    tokio::time::sleep(if mode == "exit" { Duration::from_millis(50) } else { Duration::from_secs(60) }).await;
}

#[tokio::test]
async fn shipping_owner_live_ordinary_exit_retires_registration_without_shutdown() {
    let root = root();
    let tracker = ProcessTracker::default();
    for i in 0..3 {
        let (s, lease) = admission();
        let mut cmd = command(&root.join(format!("exit-{i}")), "exit");
        let process = OwnedProcess::spawn(&mut cmd, Some(lease), &tracker).unwrap();
        assert!(process.wait_exit().await.unwrap().success());
        retired(&tracker, 0).await;
        assert_eq!(s.snapshot().active, 0);
        assert_eq!(s.snapshot().memory_mb, 0);
    }
}

#[tokio::test]
async fn shipping_owner_live_drop_reaps_and_retires_owned_work() {
    let root = root();
    let marker = root.join("hold");
    let tracker = ProcessTracker::default();
    let (s, lease) = admission();
    lease.retain_memory();
    let mut cmd = command(&marker, "hold");
    let process = OwnedProcess::spawn(&mut cmd, Some(lease), &tracker).unwrap();
    let _ = ready(&marker).await;
    let work = Arc::new(s.reserve(Demand { memory_mb: 32, cpu: 1, io: 1 }, Priority::Interactive, || false).unwrap());
    process.retain_work(work).unwrap();
    assert_eq!((s.snapshot().memory_mb, s.snapshot().active), (96, 2));
    drop(process);
    retired(&tracker, 0).await;
    assert_eq!((s.snapshot().memory_mb, s.snapshot().active), (0, 0));
}

#[tokio::test]
async fn shipping_owner_live_completed_work_release_does_not_release_resident() {
    let root = root();
    let marker = root.join("work");
    let tracker = ProcessTracker::default();
    let (s, lease) = admission();
    lease.retain_memory();
    let mut cmd = command(&marker, "hold");
    let process = OwnedProcess::spawn(&mut cmd, Some(lease), &tracker).unwrap();
    let _ = ready(&marker).await;
    process.retain_work(Arc::new(s.reserve(Demand { memory_mb: 32, cpu: 1, io: 1 }, Priority::Interactive, || false).unwrap())).unwrap();
    process.release_work();
    assert_eq!((s.snapshot().memory_mb, s.snapshot().active), (64, 1));
    assert!(process.poll_exit().unwrap().is_none());
    process.shutdown().await.unwrap();
    retired(&tracker, 0).await;
    assert_eq!(s.snapshot().active, 0);
}

#[tokio::test]
async fn shipping_owner_shutdown_closes_spawn_before_native_side_effect() {
    let root = root();
    let marker = root.join("must-not-exist");
    let tracker = ProcessTracker::default();
    tracker.shutdown().await.unwrap();
    let mut cmd = command(&marker, "hold");
    assert!(OwnedProcess::spawn(&mut cmd, None, &tracker).err().unwrap().to_string().contains("closed at shutdown"));
    assert!(!marker.exists());
    assert!(tracker.0.lock().closed);
    assert!(tracker.0.lock().handles.is_empty());
}

struct FaultChild { gate: tokio::sync::watch::Receiver<bool>, fail: bool }
impl NativeChild for FaultChild {
    fn start_kill(&mut self) -> std::io::Result<()> { Ok(()) }
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = std::io::Result<std::process::ExitStatus>> + Send + '_>> {
        Box::pin(async move {
            if std::mem::take(&mut self.fail) { return Err(std::io::Error::other("new shipping ownership failure")); }
            while !*self.gate.borrow() { self.gate.changed().await.map_err(|_| std::io::Error::other("generated gate closed"))?; }
            #[cfg(windows)]
            { use std::os::windows::process::ExitStatusExt; Ok(std::process::ExitStatus::from_raw(0)) }
            #[cfg(unix)]
            { use std::os::unix::process::ExitStatusExt; Ok(std::process::ExitStatus::from_raw(0)) }
        })
    }
}

#[tokio::test]
async fn shipping_owner_unconfirmed_fault_blocks_spawn_and_stays_charged_until_reap() {
    let root = root();
    let tracker = ProcessTracker::default();
    let (s, lease) = admission();
    let (release, gate) = tokio::sync::watch::channel(false);
    let process = {
        let mut registry = tracker.0.lock();
        OwnedProcess::own(FaultChild { gate, fail: true }, Some(lease), tracker.0.clone(), &mut registry.handles, None, None, Duration::from_secs(3))
    };
    assert!(process.handle.wait(Duration::from_secs(1)).await.unwrap_err().to_string().contains("new shipping ownership failure"));
    let mut cmd = command(&root.join("blocked"), "hold");
    assert!(OwnedProcess::spawn(&mut cmd, None, &tracker).is_err());
    assert!(!root.join("blocked").exists());
    assert_eq!((s.snapshot().memory_mb, s.snapshot().active), (64, 1));
    release.send_replace(true);
    tokio::time::timeout(Duration::from_secs(3), async {
        while tracker.has_unreaped() { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(s.snapshot().active, 0);
    retired(&tracker, 1).await;
    assert!(tracker.shutdown().await.unwrap_err().to_string().contains("new shipping ownership failure"));
    retired(&tracker, 0).await;
}

#[tokio::test]
async fn shipping_owner_concurrent_spawn_and_shutdown_share_closed_registration_barrier() {
    let root = root();
    let tracker = Arc::new(ProcessTracker::default());
    let mut tasks = Vec::new();
    for i in 0..4 {
        let tracker = tracker.clone();
        let marker = root.join(format!("racer-{i}"));
        tasks.push(tokio::spawn(async move { OwnedProcess::spawn(&mut command(&marker, "hold"), None, &tracker) }));
    }
    tokio::task::yield_now().await;
    tracker.shutdown().await.unwrap();
    for task in tasks {
        match task.await.unwrap() {
            Ok(process) => assert!(process.poll_exit().unwrap().is_some()),
            Err(error) => assert!(error.to_string().contains("closed at shutdown")),
        }
    }
    retired(&tracker, 0).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shipping_owner_linux_parent_death_kills_generated_grandchild_without_path_tools() {
    let root = root();
    let marker = root.join("parent");
    let tracker = ProcessTracker::default();
    let process = OwnedProcess::spawn(&mut command(&marker, "parent"), None, &tracker).unwrap();
    let _ = ready(&marker.with_extension("parent-ready")).await;
    let grand = ready(&marker.with_extension("grandchild")).await;
    process.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::fs::read_to_string(format!("/proc/{grand}/stat")).is_ok_and(|stat| !stat.split(')').nth(1).unwrap_or("").trim_start().starts_with('Z')) {
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    retired(&tracker, 0).await;
}

#[test]
fn shipping_owner_original_server_and_shutdown_seams_do_not_hide_cleanup_failures() {
    let server = include_str!("vlm_server.rs");
    assert!(server.contains("OwnedProcess::spawn(&mut cmd, Some(resources.clone()), ProcessTracker::shared())"));
    assert!(!server.contains("async fn stop_child"));
    assert!(server.contains("child.shutdown().await?"));
    let main = include_str!("../server.rs");
    let start = main.find("main_shutdown.await;").unwrap();
    let end = main[start..].find("FileIDEngine exiting cleanly").unwrap() + start;
    let body = &main[start..end];
    assert!(body.contains("deep_analyze_cancel.store(true"));
    assert!(body.contains("ProcessTracker::shared().shutdown().await.err()"));
    assert!(body.contains("native_cleanup_failed_at_shutdown"));
    assert!(body.contains("return Err(error)"));
}
