use super::super::native_runtime::{discover_at, validate_binary};
use super::*;
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicU64,
};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "fileid-owned-probe-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    println!("Generated owned probe state retained: {}", path.display());
    path
}
fn scheduler() -> Arc<ResourceScheduler> {
    ResourceScheduler::new(|| (16384, 12000), 1, 1)
}
fn command(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "models::native_probe::tests::generated_owned_probe_child",
            "--ignored",
            "--nocapture",
        ])
        .env("FILEID_GENERATED_PROBE_MODE", mode)
        .env("FILEID_GENERATED_PROBE_READY", root.join("ready.json"));
    command
}
fn receipt(root: &Path, value: serde_json::Value) {
    std::fs::write(root.join("receipt.json"), serde_json::to_vec(&value).unwrap()).unwrap();
}
#[test]
#[ignore = "generated own test executable, not an installed runtime"]
fn generated_owned_probe_child() {
    let Ok(mode) = std::env::var("FILEID_GENERATED_PROBE_MODE") else {
        return;
    };
    std::fs::write(
        std::env::var_os("FILEID_GENERATED_PROBE_READY").unwrap(),
        format!("{{\"pid\":{},\"notRuntimeInference\":true}}", std::process::id()),
    )
    .unwrap();
    match mode.as_str() {
        "normal" => {
            println!("generated owned version/listing, not provider acceptance");
            eprintln!("generated bounded diagnostic");
        }
        "nonzero" => std::process::exit(7),
        "stdout" => print!("{}", "x".repeat(MAX_PROBE_BYTES as usize + 2)),
        "stderr" => eprint!("{}", "x".repeat(MAX_PROBE_BYTES as usize + 2)),
        "hold" => std::thread::sleep(Duration::from_secs(60)),
        _ => panic!("unknown generated probe mode"),
    }
}
async fn ready(root: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.join("ready.json").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn successful(mode: &str, success: bool) {
    let root = root();
    let s = scheduler();
    let tracker = ProcessTracker::default();
    let output = run_probe(
        command(&root, mode),
        s.clone(),
        Arc::new(AtomicBool::new(false)),
        &tracker,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(output.status.success(), success);
    assert!(tracker.allows_fallback());
    assert_eq!(
        (
            s.snapshot().memory_mb,
            s.snapshot().cpu,
            s.snapshot().io,
            s.snapshot().active
        ),
        (128, 1, 1, 1)
    );
    if success {
        assert!(String::from_utf8_lossy(&output.stdout).contains("generated owned version"));
    }
    drop(output);
    assert_eq!(s.snapshot().memory_mb, 0);
    tracker.shutdown().await.unwrap();
    receipt(
        &root,
        serde_json::json!({"mode":mode,"confirmedReap":true,"oneHostChargeHeldThroughReturnedBuffers":true,"notProviderOrInference":true}),
    );
}
#[tokio::test]
async fn owned_probe_success_keeps_returned_host_buffers_charged_after_native_reap() {
    successful("normal", true).await;
}
#[tokio::test]
async fn owned_probe_clean_nonzero_status_is_available_to_optional_probe_policy() {
    successful("nonzero", false).await;
}
async fn failure(mode: &str, deadline: Duration, reason: &str) {
    let root = root();
    let s = scheduler();
    let tracker = ProcessTracker::default();
    let error = run_probe(
        command(&root, mode),
        s.clone(),
        Arc::new(AtomicBool::new(false)),
        &tracker,
        deadline,
    )
    .await
    .unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert!(format!("{error:#}").contains(reason));
    assert!(!super::super::native_admission::startup_allows_cli_fallback(
        &error, &tracker
    ));
    assert_eq!((s.snapshot().memory_mb, s.snapshot().cpu, s.snapshot().io), (0, 0, 0));
    assert!(tracker.allows_fallback());
    tracker.shutdown().await.unwrap();
    receipt(
        &root,
        serde_json::json!({"mode":mode,"typedControlRefusal":true,"cleanedBeforeErrorReturn":true,"fallbackAllowedFromError":false}),
    );
}
#[tokio::test]
async fn owned_probe_stdout_cap_refuses_only_after_confirmed_cleanup() {
    failure("stdout", Duration::from_secs(5), "exceeds bound").await;
}
#[tokio::test]
async fn owned_probe_stderr_cap_refuses_only_after_confirmed_cleanup() {
    failure("stderr", Duration::from_secs(5), "exceeds bound").await;
}
#[tokio::test]
async fn owned_probe_timeout_is_typed_and_awaits_cleanup_not_early_question_mark_return() {
    failure("hold", Duration::from_millis(250), "timed out").await;
}
#[tokio::test]
async fn owned_probe_active_cancel_is_typed_and_confirmed_before_return() {
    let root = root();
    let s = scheduler();
    let tracker = Arc::new(ProcessTracker::default());
    let worker_tracker = tracker.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let cmd = command(&root, "hold");
    let scheduler = s.clone();
    let task =
        tokio::spawn(async move { run_probe(cmd, scheduler, flag, &worker_tracker, Duration::from_secs(5)).await });
    ready(&root).await;
    assert_eq!(s.snapshot().memory_mb, 128);
    cancel.store(true, Ordering::Relaxed);
    let error = task.await.unwrap().unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert_eq!(s.snapshot().memory_mb, 0);
    tracker.shutdown().await.unwrap();
    receipt(
        &root,
        serde_json::json!({"cancelled":true,"confirmedBeforeReturn":true}),
    );
}
#[tokio::test]
async fn owned_probe_queued_cancel_has_no_spawn_or_nested_admission() {
    let root = root();
    let s = scheduler();
    let tracker = Arc::new(ProcessTracker::default());
    let worker_tracker = tracker.clone();
    let blocker = s
        .reserve(
            Demand {
                memory_mb: 16,
                cpu: 1,
                io: 1,
            },
            crate::resource_scheduler::Priority::Interactive,
            || false,
        )
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let cmd = command(&root, "normal");
    let scheduler = s.clone();
    let task =
        tokio::spawn(async move { run_probe(cmd, scheduler, flag, &worker_tracker, Duration::from_secs(5)).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while s.snapshot().waiting == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.store(true, Ordering::Relaxed);
    let error = task.await.unwrap().unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert!(!root.join("ready.json").exists());
    assert_eq!(s.snapshot().memory_mb, 16);
    assert_eq!(s.snapshot().waiting, 0);
    drop(blocker);
    tracker.shutdown().await.unwrap();
    receipt(&root, serde_json::json!({"queuedRefusal":true,"spawned":false}));
}
#[tokio::test]
async fn owned_probe_closed_tracker_refuses_before_spawn_and_releases_host_admission() {
    let root = root();
    let s = scheduler();
    let tracker = ProcessTracker::default();
    tracker.shutdown().await.unwrap();
    let error = run_probe(
        command(&root, "normal"),
        s.clone(),
        Arc::new(AtomicBool::new(false)),
        &tracker,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert!(!root.join("ready.json").exists());
    assert_eq!(s.snapshot().memory_mb, 0);
    receipt(&root, serde_json::json!({"closedTracker":true,"spawned":false}));
}
#[tokio::test]
async fn owned_probe_predispatch_cancel_does_not_touch_native_runtime() {
    let root = root();
    let s = scheduler();
    let tracker = ProcessTracker::default();
    let error = run_probe(
        command(&root, "normal"),
        s.clone(),
        Arc::new(AtomicBool::new(true)),
        &tracker,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert!(!root.join("ready.json").exists());
    assert_eq!(s.snapshot().memory_mb, 0);
    tracker.shutdown().await.unwrap();
    receipt(&root, serde_json::json!({"predispatchCancelled":true,"spawned":false}));
}
fn synthetic_host_binary(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = vec![0; 20000];
    #[cfg(windows)]
    {
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64_u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        let machine: u16 = if cfg!(target_arch = "aarch64") { 0xaa64 } else { 0x8664 };
        bytes[68..70].copy_from_slice(&machine.to_le_bytes());
    }
    #[cfg(target_os = "linux")]
    {
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        let machine: u16 = if cfg!(target_arch = "aarch64") { 183 } else { 62 };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    }
    #[cfg(target_os = "macos")]
    {
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        let cpu: u32 = if cfg!(target_arch = "aarch64") {
            0x0100000c
        } else {
            0x01000007
        };
        bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
#[test]
fn owned_probe_host_header_validation_is_metadata_only_not_executable_acceptance() {
    let root = root();
    let binary = root.join("metadata-only-runtime");
    synthetic_host_binary(&binary);
    assert_eq!(
        validate_binary(&binary).unwrap(),
        std::fs::canonicalize(&binary).unwrap()
    );
    receipt(
        &root,
        serde_json::json!({"syntheticHeaderValid":true,"executedSyntheticHeader":false,"inferenceCapabilityAccepted":false}),
    );
}
#[test]
fn owned_probe_host_validator_rejects_relative_directory_short_foreign_and_nonexecutable() {
    let root = root();
    assert!(validate_binary(Path::new("relative-runtime")).is_err());
    assert!(validate_binary(&root).is_err());
    let short = root.join("short");
    std::fs::write(&short, b"MZ").unwrap();
    assert!(validate_binary(&short).is_err());
    let foreign = root.join("foreign");
    synthetic_host_binary(&foreign);
    std::fs::write(&foreign, vec![0; 20000]).unwrap();
    assert!(validate_binary(&foreign).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let not_exec = root.join("not-executable");
        synthetic_host_binary(&not_exec);
        std::fs::set_permissions(&not_exec, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(validate_binary(&not_exec).is_err());
    }
    receipt(
        &root,
        serde_json::json!({"unsafeMetadataRejected":true,"notExecuted":true}),
    );
}
#[test]
fn owned_probe_explicit_runtime_is_canonical_and_takes_precedence_without_path_search() {
    let root = root();
    let explicit = root.join("chosen-runtime");
    synthetic_host_binary(&explicit);
    let paths = discover_at(&root, None, "llama-server", Some(explicit.clone())).unwrap();
    assert_eq!(paths, vec![std::fs::canonicalize(explicit).unwrap()]);
    receipt(
        &root,
        serde_json::json!({"explicitCanonicalOverride":true,"pathSearch":false}),
    );
}
#[test]
fn owned_probe_invalid_explicit_runtime_never_silently_uses_valid_bundle() {
    let root = root();
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let bundled = root.join("llama.cpp").join(format!("llama-server{suffix}"));
    synthetic_host_binary(&bundled);
    assert!(discover_at(&root, None, "llama-server", Some(root.join("missing-configured"))).is_err());
    receipt(
        &root,
        serde_json::json!({"invalidConfiguredRefused":true,"silentlyUsedBundle":false}),
    );
}
#[test]
fn owned_probe_bundle_priority_and_application_fallback_use_host_suffix() {
    let root = root();
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let name = format!("llama-server{suffix}");
    let cuda = root.join("models/llama.cpp-cuda").join(&name);
    let vulkan = root.join("models/llama.cpp/bin").join(&name);
    let app = root.join("app/RuntimeBundles/llama.cpp").join(&name);
    for path in [&cuda, &vulkan, &app] {
        synthetic_host_binary(path);
    }
    let result = discover_at(&root.join("models"), Some(&root.join("app")), "llama-server", None).unwrap();
    assert_eq!(result, [cuda, vulkan, app].map(|p| std::fs::canonicalize(p).unwrap()));
    receipt(
        &root,
        serde_json::json!({"order":"CUDA,universal,application bundle","hostSuffix":suffix,"metadataOnly":true}),
    );
}
#[test]
fn owned_probe_actual_consumer_order_and_refusals_remain_before_model_reservation() {
    let server = include_str!("vlm_server.rs").replace("\r\n", "\n");
    let start = server.find("pub async fn start(").unwrap();
    let end = server.find("async fn start_with_binary").unwrap();
    let start = &server[start..end];
    assert!(start.find("server_binaries(cancel).await?").unwrap() < start.find("reserve_host(").unwrap());
    assert!(start.find("discrete_gpu_device(&bin, cancel).await?").unwrap() < start.find("reserve_host(").unwrap());
    assert!(!server[end..].contains("discrete_gpu_device"));
    let cli = include_str!("vlm.rs");
    assert!(cli.contains("FILEID_LLAMA_CLI"));
    assert!(!cli.contains("std::process::Command"));
    assert!(!cli.contains("wait_with_output"));
    assert!(cli.contains("cancel).await?"));
    let handler = include_str!("../commands/deep_analyze.rs");
    assert!(!handler.contains("VlmRunner::find().ok()"));
    let start = handler
        .find("let runner = match crate::models::vlm::VlmRunner::find(&cancel).await")
        .unwrap();
    assert!(handler[start..].contains("startup_allows_cli_fallback"));
}
