use super::*;
use std::sync::{Barrier, atomic::AtomicU64};
use std::time::Duration;

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fileid-model-provenance-{}-{}",std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));
    std::fs::create_dir(&path).unwrap();println!("Generated model provenance state retained: {}",path.display());path
}
fn files(root: &Path) -> Vec<registry::FileEntry> {
    ["model.generated","projector.generated"].iter().map(|name| {
        let data=format!("generated model bytes for {name}; not usable weights").into_bytes();let dest=root.join(name);std::fs::write(&dest,&data).unwrap();
        registry::FileEntry { url:"fixture://generated-not-downloaded".into(),dest,sha256:Some(hex::encode(Sha256::digest(&data))),approx_bytes:data.len() as u64 }
    }).collect()
}
fn scheduler() -> Arc<ResourceScheduler> { ResourceScheduler::new(|| (16384,12000),1,1) }
fn receipt(root: &Path,value:serde_json::Value) {std::fs::write(root.join("receipt.json"),serde_json::to_vec(&value).unwrap()).unwrap();}
async fn verify(files:Vec<registry::FileEntry>,s:Arc<ResourceScheduler>) -> Result<VerifiedModel> {
    run_verification(s,Arc::new(AtomicBool::new(false)),move |cancelled| verify_files("generated-model","generated-registry",files,cancelled)).await
}
#[tokio::test]
async fn model_provenance_matching_pinned_pair_returns_canonical_paths_without_loading() {
    let root=root();let entries=files(&root);let paths=entries.iter().map(|f|std::fs::canonicalize(&f.dest).unwrap()).collect::<Vec<_>>();let s=scheduler();
    let model=verify(entries,s.clone()).await.unwrap();let(a,b)=model.paths();assert_eq!(a,paths[0]);assert_eq!(b,paths[1]);assert_eq!(model.registry_id,"generated-registry");model.trace_verified_bytes();
    assert_eq!((s.snapshot().memory_mb,s.snapshot().cpu,s.snapshot().io),(0,0,0));
    receipt(&root,serde_json::json!({"pinnedPairMatched":true,"nativeLoaded":false,"activeModel":false,"resourcesBeforeModelAdmission":0}));
}
#[tokio::test]
async fn model_provenance_pin_mismatch_is_typed_and_never_fallback_permission() {
    let root=root();let mut entries=files(&root);entries[0].sha256=Some("0".repeat(64));let s=scheduler();let error=verify(entries,s.clone()).await.unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());assert!(format!("{error:#}").contains("pinned SHA256"));
    assert!(!super::super::native_admission::startup_allows_cli_fallback(&error,&super::super::native_process::ProcessTracker::default()));assert_eq!(s.snapshot().memory_mb,0);
    receipt(&root,serde_json::json!({"badPinTyped":true,"fallbackFromRefusal":false,"nativeLoaded":false}));
}
#[test]
fn model_provenance_missing_malformed_pins_wrong_arity_and_duplicate_files_refuse() {
    let root=root();let mut entries=files(&root);entries[0].sha256=None;assert!(verify_files("fixture","fixture",entries, &|| false).is_err());
    let mut entries=files(&root);entries[0].sha256=Some("not-a-pin".into());assert!(verify_files("fixture","fixture",entries,&|| false).is_err());
    let entries=files(&root);assert!(verify_files("fixture","fixture",vec![entries[0].clone()],&|| false).is_err());
    assert!(verify_files("fixture","fixture",vec![entries[0].clone(),entries[0].clone()],&|| false).is_err());
    receipt(&root,serde_json::json!({"unverifiableInputsRefused":true,"nativeLoaded":false}));
}
#[test]
fn model_provenance_missing_empty_directory_and_relative_sources_refuse() {
    let root=root();let entries=files(&root);let mut missing=entries[0].clone();missing.dest=root.join("missing");assert!(verify_file(missing,&|| false).is_err());
    let mut empty=entries[0].clone();empty.dest=root.join("empty");std::fs::write(&empty.dest,[]).unwrap();assert!(verify_file(empty,&|| false).is_err());
    let mut directory=entries[0].clone();directory.dest=root.clone();assert!(verify_file(directory,&|| false).is_err());
    let mut relative=entries[0].clone();relative.dest=PathBuf::from("relative");assert!(verify_file(relative,&|| false).is_err());
    receipt(&root,serde_json::json!({"unsafeSourcesRefused":true,"nativeLoaded":false}));
}
#[cfg(unix)]
#[test]
fn model_provenance_symlinked_model_is_not_a_regular_pinned_artifact() {
    let root=root();let mut entry=files(&root).remove(0);let link=root.join("alias");std::os::unix::fs::symlink(&entry.dest,&link).unwrap();entry.dest=link;
    assert!(format!("{:#}",verify_file(entry,&|| false).unwrap_err()).contains("regular files"));
    std::fs::write(root.join("symlink-metadata.json"),b"{\"generatedSymlink\":true,\"accepted\":false}").unwrap();
    receipt(&root,serde_json::json!({"symlinkRefused":true,"nativeLoaded":false}));
}
#[cfg(unix)]
#[test]
fn model_provenance_path_replacement_with_same_bytes_is_detected_during_hash() {
    let root=root();let entry=files(&root).remove(0);let path=entry.dest.clone();let moved=root.join("opened-original");let content=std::fs::read(&path).unwrap();let calls=std::sync::atomic::AtomicUsize::new(0);
    let cancelled=|| { if calls.fetch_add(1,Ordering::Relaxed)==1 {std::fs::rename(&path,&moved).unwrap();std::fs::write(&path,&content).unwrap();} false };
    let error=verify_file(entry,&cancelled).unwrap_err();assert!(format!("{error:#}").contains("changed during verification"));
    receipt(&root,serde_json::json!({"sameContentDifferentFileDetected":true,"postVerificationLoaderRaceAccepted":false}));
}
#[tokio::test]
async fn model_provenance_predispatch_cancel_has_no_verification_side_effects() {
    let root=root();let s=scheduler();let marker=root.join("should-not-run");
    let error=run_verification(s.clone(),Arc::new(AtomicBool::new(true)),move |_| {std::fs::write(marker,b"unexpected").unwrap();bail!("unexpected verification")}).await.unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());assert!(!root.join("should-not-run").exists());assert_eq!(s.snapshot().memory_mb,0);
    receipt(&root,serde_json::json!({"cancelledBeforeWork":true,"nativeLoaded":false}));
}
#[tokio::test]
async fn model_provenance_queued_cancel_releases_waiter_without_hash_or_native_work() {
    let root=root();let s=scheduler();let blocker=s.reserve(Demand {memory_mb:8,cpu:1,io:1},crate::resource_scheduler::Priority::Interactive,|| false).unwrap();let worker=s.clone();let cancel=Arc::new(AtomicBool::new(false));let flag=cancel.clone();let marker=root.join("should-not-run");
    let task=tokio::spawn(async move {run_verification(worker,flag,move |_| {std::fs::write(marker,b"unexpected").unwrap();bail!("unexpected verification")}).await});
    tokio::time::timeout(Duration::from_secs(5),async {while s.snapshot().waiting==0 {tokio::task::yield_now().await;}}).await.unwrap();cancel.store(true,Ordering::Relaxed);
    let error=task.await.unwrap().unwrap_err();assert!(error.downcast_ref::<HostAdmissionError>().is_some());assert!(!root.join("should-not-run").exists());assert_eq!(s.snapshot().waiting,0);assert_eq!(s.snapshot().memory_mb,8);drop(blocker);
    receipt(&root,serde_json::json!({"queuedCancelNoWork":true,"remainingWaiters":0}));
}
#[tokio::test]
async fn model_provenance_abandoned_worker_retains_actual_host_charge_until_end() {
    let root=root();let s=scheduler();let worker=s.clone();let entered=Arc::new(AtomicBool::new(false));let started=entered.clone();let release=Arc::new(Barrier::new(2));let gate=release.clone();let abandoned=Arc::new(AtomicBool::new(false));let observed=abandoned.clone();
    let task=tokio::spawn(async move {run_verification(worker,Arc::new(AtomicBool::new(false)),move |cancelled| {started.store(true,Ordering::Relaxed);gate.wait();observed.store(cancelled(),Ordering::Relaxed);bail!("generated verification worker ended")}).await});
    tokio::time::timeout(Duration::from_secs(5),async {while !entered.load(Ordering::Relaxed) {tokio::task::yield_now().await;}}).await.unwrap();
    assert_eq!((s.snapshot().memory_mb,s.snapshot().cpu,s.snapshot().io),(16,1,1));task.abort();assert!(task.await.unwrap_err().is_cancelled());assert_eq!(s.snapshot().memory_mb,16);release.wait();
    tokio::time::timeout(Duration::from_secs(5),async {while s.snapshot().memory_mb>0 {tokio::task::yield_now().await;}}).await.unwrap();assert!(abandoned.load(Ordering::Relaxed));
    receipt(&root,serde_json::json!({"callerAbandoned":true,"one16MiBChargeHeldThroughActualBlockingWork":true,"workerObservedAbandonment":true,"nativeLoaded":false}));
}
#[tokio::test]
async fn model_provenance_actual_registry_rejects_non_vlm_without_loading_or_download() {
    let error=verify_installed("bge_text",Arc::new(AtomicBool::new(false))).await.unwrap_err();assert!(error.downcast_ref::<HostAdmissionError>().is_some());assert!(format!("{error:#}").contains("Unknown caption/text"));
}
#[tokio::test]
async fn model_provenance_actual_verified_start_missing_weights_stops_before_runtime_probe() {
    let root=root();let models=std::env::var_os("FILEID_MODELS_DIR").unwrap();assert_eq!(std::fs::read_dir(&models).unwrap().count(),0);
    let error=super::super::vlm_server::VlmServer::start_verified("qwen3_vl_4b",&Arc::new(AtomicBool::new(false))).await.err().unwrap();assert!(error.downcast_ref::<HostAdmissionError>().is_some());assert!(format!("{error:#}").contains("Installed model is missing"));
    assert_eq!(std::fs::read_dir(models).unwrap().count(),0);receipt(&root,serde_json::json!({"actualVerifiedStartMissingWeightsTyped":true,"runtimeProbeReached":false,"noDownloadOrModelLoad":true}));
}
#[test]
fn model_provenance_actual_persistent_consumer_uses_verified_paths_before_start_admission() {
    let server=include_str!("vlm_server.rs");let start=server.find("async fn start_verified(").unwrap();let end=server[start..].find("async fn server_binaries").unwrap()+start;let function=&server[start..end];assert!(function.find("verify_installed(").unwrap()<function.find("Self::start(").unwrap());assert!(function.contains("verified.paths()"));
    let handler=include_str!("../commands/deep_analyze.rs");assert!(handler.contains("ResidentService::shared().acquire(model_kind, cancel.clone()).await"));assert!(!handler.contains("VlmServer::start(&gguf, &mmproj, &cancel).await"));
    let admission=include_str!("native_admission.rs");assert!(admission.contains("HostAdmissionError"));assert!(admission.contains("tracker.allows_fallback()"));
    let verifier=include_str!("model_provenance.rs");assert!(!verifier.contains("reqwest"));assert!(!verifier.contains("Demand { memory_mb: 384"));
}
