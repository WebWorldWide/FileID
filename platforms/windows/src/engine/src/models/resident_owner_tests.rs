use super::super::{
    model_provenance::{self, VerifiedModel},
    resident_service::{Key, ResidentLease, ResidentService},
    runtime_cache,
};
use super::*;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, AtomicUsize},
};
#[derive(Clone)]
struct Fixture {
    root: PathBuf,
    s: Arc<ResourceScheduler>,
    tracker: Arc<ProcessTracker>,
    hashes: Arc<AtomicUsize>,
    loads: Arc<AtomicUsize>,
    available: Arc<AtomicU64>,
    listener: Arc<std::net::TcpListener>,
}
fn flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
fn fixture() -> Fixture {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "fileid-resident-owner-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    let root = crate::util::path_safety::strip_extended_length(&std::fs::canonicalize(&root).unwrap());
    for name in ["weights.fixture", "projector.fixture"] {
        std::fs::write(
            root.join(name),
            format!("generated proof bytes {name}, not model weights"),
        )
        .unwrap();
    }
    std::fs::write(root.join("runtime.fixture"), vec![1; 20000]).unwrap();
    std::fs::write(root.join("image.png"), [0x89, b'P', b'N', b'G']).unwrap();
    println!("Generated resident owner state retained: {}", root.display());
    let available = Arc::new(AtomicU64::new(12000));
    let a = available.clone();
    Fixture {
        root,
        s: ResourceScheduler::new(move || (16384, a.load(Ordering::Relaxed)), 1, 1),
        tracker: Arc::new(ProcessTracker::default()),
        hashes: Arc::new(AtomicUsize::new(0)),
        loads: Arc::new(AtomicUsize::new(0)),
        available,
        listener: Arc::new(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap()),
    }
}
fn proof(f: &Fixture) -> Result<VerifiedModel> {
    f.hashes.fetch_add(1, Ordering::Relaxed);
    let files = ["weights.fixture", "projector.fixture"]
        .into_iter()
        .map(|name| {
            let dest = f.root.join(name);
            let bytes = std::fs::read(&dest).unwrap();
            super::super::registry::FileEntry {
                url: "fixture://no-download".into(),
                dest,
                sha256: Some(hex::encode(Sha256::digest(&bytes))),
                approx_bytes: bytes.len() as u64,
            }
        })
        .collect();
    model_provenance::verify_files("qwen3_vl_4b", "generated-not-registry-pins", files, &|| false)
}
async fn load(f: &Fixture) -> Result<VlmServer> {
    let id = f.loads.fetch_add(1, Ordering::Relaxed);
    let lease = reserve_host(
        f.s.clone(),
        Demand {
            memory_mb: 64,
            cpu: 1,
            io: 1,
        },
        flag(),
    )
    .await?;
    let marker = f.root.join(format!("own-native-ready-{id}"));
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "models::vlm_server::resident_owner_tests::generated_resident_owner_child",
        "--ignored",
        "--nocapture",
    ])
    .env("FILEID_RESIDENT_OWNER_READY", &marker)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let process = OwnedProcess::spawn(&mut cmd, Some(lease.clone()), &f.tracker)?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    lease.retain_memory();
    Ok(VlmServer {
        last_timings: parking_lot::Mutex::new(None),
        process,
        base_url: format!("http://{}", f.listener.local_addr().unwrap()),
        api_key: "generated-auth".into(),
        client: build_loopback_client().unwrap(),
        scheduler: f.s.clone(),
        cancel: flag(),
        request_gate: tokio::sync::Mutex::new(()),
    })
}
async fn acquire(
    owner: &Arc<ResidentService>,
    f: &Fixture,
    cpu: bool,
    cancel: Arc<AtomicBool>,
) -> Result<ResidentLease> {
    owner
        .acquire_with(
            "qwen3_vl_4b",
            f.root.clone(),
            cancel,
            || async { proof(f) },
            || async {
                Ok(Key {
                    cpu,
                    runtime: if cpu {
                        Some(runtime_cache::capture_paths(vec![f.root.join("runtime.fixture")], flag()).await?)
                    } else {
                        None
                    },
                })
            },
            |_| async { load(f).await },
        )
        .await
}
fn owner(f: &Fixture, idle: Duration) -> Arc<ResidentService> {
    ResidentService::new(f.s.clone(), idle, || true)
}
async fn finish(owner: &ResidentService, f: &Fixture) {
    owner.shutdown().await.unwrap();
    f.tracker.shutdown().await.unwrap();
    assert_eq!(f.s.snapshot().memory_mb, 0);
}
fn receipt(f: &Fixture, value: serde_json::Value) {
    std::fs::write(f.root.join("receipt.json"), serde_json::to_vec(&value).unwrap()).unwrap();
}
async fn request(lease: &ResidentLease, f: &Fixture) {
    let listener = f.listener.try_clone().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            assert!(bytes.len() < 65536);
            if let Some(end) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let size: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + size {
                    assert!(headers.contains("Bearer generated-auth"));
                    break;
                }
            }
        }
        let body = r#"{"choices":[{"message":{"content":"generated resident response, not inference"}}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let text = lease
        .complete_with_cancel(&f.root.join("image.png"), "generated", 1, flag())
        .await
        .unwrap();
    assert!(text.starts_with("generated resident"));
    worker.join().unwrap();
}
#[tokio::test]
#[ignore = "own executable only, not native inference/model residency"]
async fn generated_resident_owner_child() {
    std::fs::write(
        std::env::var_os("FILEID_RESIDENT_OWNER_READY").unwrap(),
        std::process::id().to_string(),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_secs(60)).await;
}
#[tokio::test]
async fn resident_owner_two_jobs_reuse_same_owned_process_and_model_proof() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    let a = acquire(&o, &f, true, flag()).await.unwrap();
    assert!(a.reusable_idle());
    assert!(f.tracker.allows_discovery());
    assert!(!f.tracker.allows_fallback());
    request(&a, &f).await;
    a.finish(true).await.unwrap();
    assert_eq!(f.s.snapshot().memory_mb, 64);
    let b = acquire(&o, &f, true, flag()).await.unwrap();
    request(&b, &f).await;
    b.finish(true).await.unwrap();
    assert_eq!(f.hashes.load(Ordering::Relaxed), 1);
    assert_eq!(f.loads.load(Ordering::Relaxed), 1);
    receipt(
        &f,
        serde_json::json!({"twoJobsOneNativeOwner":true,"fullModelHashCalls":1,"actualGeneratedLoopbackWork":true,"vendorModelLoaded":false}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_changed_runtime_reaps_before_new_process() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    acquire(&o, &f, true, flag()).await.unwrap().finish(true).await.unwrap();
    std::fs::write(f.root.join("runtime.fixture"), vec![2; 20000]).unwrap();
    let b = acquire(&o, &f, true, flag()).await.unwrap();
    assert_eq!(f.loads.load(Ordering::Relaxed), 2);
    assert_eq!(f.hashes.load(Ordering::Relaxed), 1);
    assert_eq!(f.s.snapshot().memory_mb, 64);
    b.finish(true).await.unwrap();
    receipt(
        &f,
        serde_json::json!({"runtimeByteChangeRetiredOldOwner":true,"oneResidentChargeAfterReload":true}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_changed_weight_source_refuses_and_retires_cache() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    acquire(&o, &f, true, flag()).await.unwrap().finish(true).await.unwrap();
    std::fs::write(f.root.join("weights.fixture"), b"changed proof").unwrap();
    let error = acquire(&o, &f, true, flag()).await.err().unwrap();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert_eq!(f.loads.load(Ordering::Relaxed), 1);
    assert_eq!(f.s.snapshot().memory_mb, 0);
    receipt(
        &f,
        serde_json::json!({"changedSourceRefused":true,"cacheReaped":true,"noAutomaticReload":true}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_non_cpu_policy_does_not_enable_unaccounted_gpu_retention() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    acquire(&o, &f, false, flag())
        .await
        .unwrap()
        .finish(true)
        .await
        .unwrap();
    assert_eq!(f.s.snapshot().memory_mb, 0);
    assert!(f.tracker.allows_fallback());
    receipt(
        &f,
        serde_json::json!({"nonCpuPolicyNotRetained":true,"verifiedAcceleratorCapacity":false}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_real_scheduler_pressure_reaps_idle_cache() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    acquire(&o, &f, true, flag()).await.unwrap().finish(true).await.unwrap();
    f.available.store(1, Ordering::Relaxed);
    o.sweep().await.unwrap();
    assert_eq!(f.s.snapshot().memory_mb, 0);
    receipt(
        &f,
        serde_json::json!({"schedulerMemoryPressureReaped":true,"gpuCapacityGuessed":false}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_idle_expiry_reaps_without_engine_shutdown() {
    let f = fixture();
    let o = owner(&f, Duration::ZERO);
    acquire(&o, &f, true, flag()).await.unwrap().finish(true).await.unwrap();
    o.sweep().await.unwrap();
    assert_eq!(f.s.snapshot().memory_mb, 0);
    assert!(f.tracker.allows_fallback());
    receipt(&f, serde_json::json!({"idleExpiryReaped":true,"trackerStillOpen":true}));
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_closed_owner_refuses_new_load() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    acquire(&o, &f, true, flag()).await.unwrap().finish(true).await.unwrap();
    o.shutdown().await.unwrap();
    let error = acquire(&o, &f, true, flag()).await.err().unwrap();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert_eq!(f.loads.load(Ordering::Relaxed), 1);
    assert_eq!(f.s.snapshot().memory_mb, 0);
    receipt(
        &f,
        serde_json::json!({"closedOwnerDeniesCheckout":true,"noPostCloseSpawn":true}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_waiting_checkout_cancel_preserves_current_job() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    let a = acquire(&o, &f, true, flag()).await.unwrap();
    let c = flag();
    let (other, owner, token) = (f.clone(), o.clone(), c.clone());
    let waiting = tokio::spawn(async move { acquire(&owner, &other, true, token).await });
    tokio::task::yield_now().await;
    f.available.store(1, Ordering::Relaxed);
    o.sweep().await.unwrap();
    assert_eq!(f.s.snapshot().memory_mb, 64);
    f.available.store(12000, Ordering::Relaxed);
    c.store(true, Ordering::Relaxed);
    let error = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert!(a.reusable_idle());
    assert_eq!(f.loads.load(Ordering::Relaxed), 1);
    a.finish(true).await.unwrap();
    receipt(
        &f,
        serde_json::json!({"waitingCheckoutCancelledOnly":true,"sweepDidNotRetireBusyLease":true}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_actual_missing_registered_weights_refuse_before_runtime() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    let error = o.acquire("qwen3_vl_4b", flag()).await.err().unwrap();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert!(format!("{error:#}").contains("Installed model is missing"));
    assert_eq!(f.loads.load(Ordering::Relaxed), 0);
    receipt(
        &f,
        serde_json::json!({"actualRegisteredMissingWeightsRefused":true,"runtimeSpawned":false}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_close_during_awaited_proof_denies_load() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (other_f, other_o) = (f.clone(), o.clone());
    let checkout = tokio::spawn(async move {
        other_o
            .acquire_with(
                "qwen3_vl_4b",
                other_f.root.clone(),
                flag(),
                || async {
                    let _ = tx.send(());
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    proof(&other_f)
                },
                || async {
                    Ok(Key {
                        cpu: true,
                        runtime: None,
                    })
                },
                |_| async { load(&other_f).await },
            )
            .await
    });
    rx.await.unwrap();
    let _ = o.shutdown().await;
    let err = checkout.await.unwrap().err().unwrap();
    assert!(err.downcast_ref::<HostAdmissionError>().is_some());
    assert_eq!(f.loads.load(Ordering::Relaxed), 0);
    receipt(
        &f,
        serde_json::json!({"closeDuringProofDeniesLoad":true,"loadsAttempted":0}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_cancel_during_awaited_proof_denies_load() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    let c = flag();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (other_f, other_o, other_c) = (f.clone(), o.clone(), c.clone());
    let checkout = tokio::spawn(async move {
        other_o
            .acquire_with(
                "qwen3_vl_4b",
                other_f.root.clone(),
                other_c,
                || async {
                    let _ = tx.send(());
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    proof(&other_f)
                },
                || async {
                    Ok(Key {
                        cpu: true,
                        runtime: None,
                    })
                },
                |_| async { load(&other_f).await },
            )
            .await
    });
    rx.await.unwrap();
    c.store(true, Ordering::Relaxed);
    let err = checkout.await.unwrap().err().unwrap();
    assert!(err.downcast_ref::<HostAdmissionError>().is_some());
    assert_eq!(f.loads.load(Ordering::Relaxed), 0);
    receipt(
        &f,
        serde_json::json!({"cancelDuringProofDeniesLoad":true,"loadsAttempted":0}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_close_during_awaited_load_shuts_down_process_without_commit() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (other_f, other_o) = (f.clone(), o.clone());
    let checkout = tokio::spawn(async move {
        other_o
            .acquire_with(
                "qwen3_vl_4b",
                other_f.root.clone(),
                flag(),
                || async { proof(&other_f) },
                || async {
                    Ok(Key {
                        cpu: true,
                        runtime: None,
                    })
                },
                |_| async {
                    let s = load(&other_f).await?;
                    let _ = tx.send(());
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    Ok(s)
                },
            )
            .await
    });
    rx.await.unwrap();
    let shut = tokio::spawn({
        let o = o.clone();
        async move { o.shutdown().await }
    });
    let err = checkout.await.unwrap().err().unwrap();
    assert!(err.downcast_ref::<HostAdmissionError>().is_some());
    let _ = shut.await.unwrap();
    assert_eq!(f.loads.load(Ordering::Relaxed), 1);
    assert_eq!(f.s.snapshot().memory_mb, 0);
    receipt(
        &f,
        serde_json::json!({"closeDuringLoadReaped":true,"uncommittedServerReaped":true}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_cancel_during_awaited_load_shuts_down_process_without_commit() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    let c = flag();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (other_f, other_o, other_c) = (f.clone(), o.clone(), c.clone());
    let checkout = tokio::spawn(async move {
        other_o
            .acquire_with(
                "qwen3_vl_4b",
                other_f.root.clone(),
                other_c,
                || async { proof(&other_f) },
                || async {
                    Ok(Key {
                        cpu: true,
                        runtime: None,
                    })
                },
                |_| async {
                    let s = load(&other_f).await?;
                    let _ = tx.send(());
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    Ok(s)
                },
            )
            .await
    });
    rx.await.unwrap();
    c.store(true, Ordering::Relaxed);
    let err = checkout.await.unwrap().err().unwrap();
    assert!(err.downcast_ref::<HostAdmissionError>().is_some());
    assert_eq!(f.loads.load(Ordering::Relaxed), 1);
    assert_eq!(f.s.snapshot().memory_mb, 0);
    receipt(
        &f,
        serde_json::json!({"cancelDuringLoadReaped":true,"uncommittedServerReaped":true}),
    );
    finish(&o, &f).await;
}
#[tokio::test]
async fn resident_owner_policy_change_during_startup_reaps_process_and_refuses() {
    let f = fixture();
    let o = owner(&f, Duration::from_secs(120));
    let mut call_count = 0;
    let err = o
        .acquire_with(
            "qwen3_vl_4b",
            f.root.clone(),
            flag(),
            || async { proof(&f) },
            || {
                call_count += 1;
                let cpu = call_count == 1;
                async move { Ok(Key { cpu, runtime: None }) }
            },
            |_| async { load(&f).await },
        )
        .await
        .err()
        .unwrap();
    assert!(err.downcast_ref::<HostAdmissionError>().is_some());
    assert_eq!(f.loads.load(Ordering::Relaxed), 1);
    assert_eq!(f.s.snapshot().memory_mb, 0);
    receipt(
        &f,
        serde_json::json!({"policyChangeDuringStartupReaped":true,"memoryShed":true}),
    );
    finish(&o, &f).await;
}
#[test]
fn resident_owner_actual_deep_and_shutdown_consume_service_not_private_tracker() {
    let deep = include_str!("../commands/deep_analyze.rs");
    assert!(deep.contains("ResidentService::shared().acquire(model_kind, cancel.clone()).await"));
    assert!(deep.contains("server.finish(keep).await"));
    assert!(deep.contains("ProcessTracker::shared().allows_discovery()"));
    let main = include_str!("../server.rs");
    assert!(
        main.find("ResidentService::shared().shutdown()").unwrap()
            < main.find("ProcessTracker::shared().shutdown()").unwrap()
    );
    let owner = include_str!("resident_service.rs")
        .split_whitespace()
        .collect::<String>();
    assert!(owner.contains("entry.key==current&&current.cpu"));
    assert!(owner.contains("self.scheduler.under_pressure()"));
    assert!(!owner.contains("ProcessTracker::default"));
    assert!(!owner.contains("memory_mb:384"));
}
