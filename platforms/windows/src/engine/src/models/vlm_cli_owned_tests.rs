use super::*;
use crate::resource_scheduler::{Demand, Priority, ResourceScheduler};
use std::{path::PathBuf, sync::atomic::AtomicU64};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!("fileid-cli-owned-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir(&root).unwrap(); println!("Generated CLI ownership state retained: {}", root.display()); root
}
fn command(root: &std::path::Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "models::vlm::cli_output::tests::generated_cli_owned_child", "--ignored", "--nocapture"])
        .env("FILEID_GENERATED_CLI_MODE", mode).env("FILEID_GENERATED_CLI_READY", root.join("ready.json"))
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).stdin(std::process::Stdio::null()); command
}
fn scheduler() -> Arc<crate::resource_scheduler::ResourceScheduler> { ResourceScheduler::new(|| (16384, 12000), 1, 1) }
fn lease(s: &Arc<ResourceScheduler>) -> Arc<Lease> { Arc::new(s.reserve(Demand { memory_mb: 64, cpu: 1, io: 1 }, Priority::Interactive, || false).unwrap()) }
async fn ready(root: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.join("ready.json").is_file() { tokio::task::yield_now().await; }
    }).await.unwrap();
}
#[test]
#[ignore = "generated own-executable child, not model inference"]
fn generated_cli_owned_child() {
    let Ok(mode) = std::env::var("FILEID_GENERATED_CLI_MODE") else { return };
    std::fs::write(std::env::var_os("FILEID_GENERATED_CLI_READY").unwrap(), format!("{{\"pid\":{},\"notInference\":true}}", std::process::id())).unwrap();
    match mode.as_str() {
        "normal" => { println!("generated token one"); println!("generated token two"); eprintln!("generated diagnostic only"); }
        "exit" => std::process::exit(7),
        "stdout" => { print!("{}", "x".repeat(MAX_STDOUT as usize + 2)); }
        "stderr" => { eprint!("{}", "x".repeat(MAX_STDERR as usize + 2)); }
        "hold" => std::thread::sleep(Duration::from_secs(60)),
        _ => panic!("unknown generated mode"),
    }
}
async fn terminal(mode: &str, expected: Option<&str>) {
    let root = root(); let s = scheduler(); let tracker = ProcessTracker::default(); let mut tokens = Vec::new();
    let result = run_caption_command(command(&root, mode), lease(&s), Arc::new(AtomicBool::new(false)), &root.join("fixture-image"), |line| {
        assert_eq!((s.snapshot().memory_mb, s.snapshot().cpu, s.snapshot().io), (64, 1, 1)); tokens.push(line.to_owned());
    }, &tracker).await;
    match expected {
        Some(reason) => assert!(result.unwrap_err().to_string().contains(reason)),
        None => { assert!(result.unwrap().contains("generated token one")); assert!(tokens.iter().any(|line| line.contains("generated token two"))); }
    }
    assert_eq!((s.snapshot().memory_mb, s.snapshot().cpu, s.snapshot().io), (0, 0, 0));
    assert!(tracker.allows_fallback()); tracker.shutdown().await.unwrap();
    std::fs::write(root.join("receipt.json"), serde_json::to_vec(&serde_json::json!({"mode":mode,"notInference":true,"confirmedCleanupBeforeReturn":true,"resourcesAtReturn":[0,0,0],"streamCallbackInvoked":!tokens.is_empty()})).unwrap()).unwrap();
}
#[tokio::test]
async fn cli_owned_success_streams_and_holds_host_lease_through_callback_then_reaps() { terminal("normal", None).await; }
#[tokio::test]
async fn cli_owned_nonzero_exit_fails_after_confirmed_cleanup() { terminal("exit", Some("VLM exited")).await; }
#[tokio::test]
async fn cli_owned_stdout_cap_fails_after_confirmed_cleanup() { terminal("stdout", Some("stdout exceeds bound")).await; }
#[tokio::test]
async fn cli_owned_stderr_cap_fails_after_confirmed_cleanup() { terminal("stderr", Some("stderr exceeds bound")).await; }
#[tokio::test]
async fn cli_owned_cancel_pending_pipe_awaits_reap_and_releases_one_charge() {
    let root = root(); let s = scheduler(); let tracker = Arc::new(ProcessTracker::default()); let child_tracker = tracker.clone();
    let cancel = Arc::new(AtomicBool::new(false)); let flag = cancel.clone(); let cmd = command(&root, "hold"); let image = root.join("fixture-image"); let resources = lease(&s);
    let task = tokio::spawn(async move { run_caption_command(cmd, resources, flag, &image, |_| {}, &child_tracker).await });
    ready(&root).await; assert_eq!(s.snapshot().memory_mb, 64); cancel.store(true, Ordering::Relaxed);
    assert!(tokio::time::timeout(Duration::from_secs(8), task).await.unwrap().unwrap().unwrap_err().to_string().contains("cancelled"));
    assert_eq!((s.snapshot().memory_mb, s.snapshot().cpu, s.snapshot().io), (0, 0, 0)); tracker.shutdown().await.unwrap();
    std::fs::write(root.join("receipt.json"), b"{\"cancelled\":true,\"confirmedReapAtReturn\":true,\"notInference\":true}").unwrap();
}
#[tokio::test]
async fn cli_owned_abandoned_future_has_shared_shutdown_owner_and_no_pipe_task_leak() {
    let root = root(); let s = scheduler(); let tracker = Arc::new(ProcessTracker::default()); let child_tracker = tracker.clone();
    let cmd = command(&root, "hold"); let image = root.join("fixture-image"); let resources = lease(&s);
    let task = tokio::spawn(async move { run_caption_command(cmd, resources, Arc::new(AtomicBool::new(false)), &image, |_| {}, &child_tracker).await });
    ready(&root).await; assert_eq!(s.snapshot().memory_mb, 64); task.abort(); assert!(task.await.unwrap_err().is_cancelled());
    tracker.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async { while s.snapshot().memory_mb != 0 { tokio::task::yield_now().await; } }).await.unwrap();
    assert_eq!((s.snapshot().cpu, s.snapshot().io), (0, 0));
    std::fs::write(root.join("receipt.json"), b"{\"abandonedCaller\":true,\"sharedOwnerConfirmedShutdown\":true,\"pipeTaskResourcesReleased\":true}").unwrap();
}
#[tokio::test]
async fn cli_owned_closed_tracker_refuses_before_spawn() {
    let root = root(); let s = scheduler(); let tracker = ProcessTracker::default(); tracker.shutdown().await.unwrap();
    let result = run_caption_command(command(&root, "normal"), lease(&s), Arc::new(AtomicBool::new(false)), &root.join("fixture-image"), |_| {}, &tracker).await;
    assert!(format!("{:#}", result.unwrap_err()).contains("spawning closed")); assert!(!root.join("ready.json").exists()); assert_eq!(s.snapshot().memory_mb, 0);
    std::fs::write(root.join("receipt.json"), b"{\"spawned\":false,\"closedTracker\":true}").unwrap();
}
#[tokio::test]
async fn cli_owned_predispatch_cancel_refuses_before_spawn() {
    let root = root(); let s = scheduler(); let tracker = ProcessTracker::default();
    let result = run_caption_command(command(&root, "normal"), lease(&s), Arc::new(AtomicBool::new(true)), &root.join("fixture-image"), |_| {}, &tracker).await;
    assert!(result.unwrap_err().to_string().contains("startup cancelled")); assert!(!root.join("ready.json").exists()); assert_eq!(s.snapshot().memory_mb, 0); tracker.shutdown().await.unwrap();
    std::fs::write(root.join("receipt.json"), b"{\"spawned\":false,\"predispatchCancelled\":true}").unwrap();
}
#[tokio::test]
async fn cli_owned_actual_caption_missing_model_is_typed_before_probe_or_child() {
    let root = root(); let runner = super::super::VlmRunner::new(std::env::current_exe().unwrap());
    let req = super::super::CaptionRequest { gguf_path: root.join("missing-model"), mmproj_path: root.join("missing-projection"), image_path: root.join("fixture-image"), prompt: "generated prompt".into(), max_tokens: 4, greedy: true };
    let error = super::super::caption(&runner, &req, Arc::new(AtomicBool::new(false)), |_| {}).await.unwrap_err();
    assert!(error.downcast_ref::<crate::models::native_admission::HostAdmissionError>().is_some());
    std::fs::write(root.join("receipt.json"), b"{\"actualCaptionPreflight\":true,\"typedHostRefusal\":true,\"notInference\":true}").unwrap();
}
#[test]
fn cli_owned_actual_consumers_reserve_after_probe_and_share_tracker_atomic_pipe_spawn() {
    let caller = include_str!("vlm.rs").replace("\r\n", "\n"); let begin = caller.find("#[cfg(not(feature = \"vlm-native\"))]").unwrap(); let end = caller[begin..].find("#[cfg(feature = \"vlm-native\")]\nmod native").unwrap() + begin; let caller = &caller[begin..end];
    assert!(caller.find("discrete_gpu_device").unwrap() < caller.find("reserve_host").unwrap()); assert!(caller.contains("model_mb.checked_add(512)"));
    assert!(caller.contains("run_caption_command")); assert!(caller.contains("ProcessTracker::shared()")); assert!(!caller.contains("cmd.spawn()"));
    let owner = include_str!("native_process.rs").replace("\r\n", "\n"); let begin = owner.find("pub(crate) fn spawn_piped").unwrap(); let owner = &owner[begin..owner.find("fn own(mut child").unwrap()];
    assert!(owner.find("tracker.0.lock()").unwrap() < owner.find("command.spawn()").unwrap());
    assert!(owner.contains("registry.closed")); assert!(owner.contains("child.stdout.take()")); assert!(owner.contains("child.stderr.take()"));
}
