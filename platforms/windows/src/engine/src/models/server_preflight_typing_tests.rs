use super::*;
use super::super::native_admission::{HostAdmissionError, startup_allows_cli_fallback};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fileid-preflight-typing-{}-{}",std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));
    std::fs::create_dir(&path).unwrap();
    println!("Generated preflight state retained: {}",path.display());
    path
}
async fn original_start_refuses(gguf: &Path, projection: &Path) {
    let scheduler = ResourceScheduler::shared();
    let before = scheduler.snapshot();
    let cancel = Arc::new(AtomicBool::new(false));
    let error = VlmServer::start(gguf, projection, &cancel).await.err().expect("invalid model preflight must stop original startup");
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert!(!startup_allows_cli_fallback(&error, ProcessTracker::shared()));
    let context = error.context("native startup phase");
    assert!(!startup_allows_cli_fallback(&context, ProcessTracker::shared()));
    assert!(!ProcessTracker::shared().has_unreaped());
    let after = scheduler.snapshot();
    assert_eq!((before.memory_mb,before.cpu,before.io,before.active,before.waiting),(after.memory_mb,after.cpu,after.io,after.active,after.waiting));
}
#[tokio::test]
async fn preflight_typing_actual_start_missing_weights_never_falls_through() {
    let root = root();
    std::fs::write(root.join("projection.gguf"), b"generated fixture, not weights").unwrap();
    original_start_refuses(&root.join("missing.gguf"), &root.join("projection.gguf")).await;
}
#[tokio::test]
async fn preflight_typing_actual_start_empty_model_never_falls_through() {
    let root = root();
    std::fs::write(root.join("empty.gguf"), b"").unwrap();
    std::fs::write(root.join("projection.gguf"), b"generated fixture, not weights").unwrap();
    original_start_refuses(&root.join("empty.gguf"), &root.join("projection.gguf")).await;
}
#[tokio::test]
async fn preflight_typing_actual_start_directory_projection_never_falls_through() {
    let root = root();
    std::fs::write(root.join("generated.gguf"), b"generated fixture, not weights").unwrap();
    std::fs::create_dir(root.join("projection")).unwrap();
    original_start_refuses(&root.join("generated.gguf"), &root.join("projection")).await;
}
#[test]
fn preflight_typing_classifier_keeps_runtime_fallback_distinct_from_host_refusal() {
    let tracker=ProcessTracker::default();
    assert!(startup_allows_cli_fallback(&anyhow!("runtime binary unavailable"),&tracker));
    let refused=anyhow::Error::new(HostAdmissionError(anyhow!("available host memory too low"))).context("startup model preflight");
    assert!(!startup_allows_cli_fallback(&refused,&tracker));
    let commands=include_str!("../commands/deep_analyze.rs");
    assert!(commands.contains("if !crate::models::native_admission::startup_allows_cli_fallback("));
}
