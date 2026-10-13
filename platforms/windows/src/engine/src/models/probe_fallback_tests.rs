use super::*;
use super::super::native_admission::{HostAdmissionError, probe_requests_fallback};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fileid-probe-fallback-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir(&path).unwrap(); println!("Generated probe fallback state retained: {}", path.display()); path
}
async fn fixture(root: &Path) -> (VlmServer, ProcessTracker, Arc<ResourceScheduler>, Arc<AtomicU64>, std::net::TcpListener) {
    let available = Arc::new(AtomicU64::new(12000)); let budget = available.clone();
    let s = ResourceScheduler::new(move || (16384, budget.load(Ordering::Relaxed)), 1, 1);
    let lease = reserve_host(s.clone(), Demand { memory_mb: 64, cpu: 1, io: 1 }, Arc::new(AtomicBool::new(false))).await.unwrap();
    let marker = root.join("native-ready"); let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "models::vlm_server::probe_fallback_tests::generated_fallback_child", "--ignored", "--nocapture"])
        .env("FILEID_GENERATED_FALLBACK_READY", &marker).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let tracker = ProcessTracker::default(); let process = OwnedProcess::spawn(&mut command, Some(lease.clone()), &tracker).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async { while !marker.exists() { tokio::task::yield_now().await; } }).await.unwrap();
    lease.retain_memory(); let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (VlmServer { process, base_url: url, api_key: "generated-auth".into(), client: build_loopback_client().unwrap(), scheduler: s.clone(), cancel: Arc::new(AtomicBool::new(false)), request_gate: tokio::sync::Mutex::new(()) }, tracker, s, available, listener)
}
async fn finish(server: VlmServer, tracker: &ProcessTracker, s: &ResourceScheduler) {
    server.shutdown().await.unwrap(); assert!(tracker.allows_fallback());
    tracker.shutdown().await.unwrap(); assert_eq!(s.snapshot().memory_mb, 0); assert!(!tracker.allows_fallback());
}
#[tokio::test]
#[ignore = "own executable generated child only, not inference runtime"]
async fn generated_fallback_child() {
    std::fs::write(std::env::var_os("FILEID_GENERATED_FALLBACK_READY").unwrap(), std::process::id().to_string()).unwrap();
    tokio::time::sleep(Duration::from_secs(60)).await;
}
async fn refused_probe(cancelled: bool) {
    let root = root(); let (server, tracker, s, available, listener) = fixture(&root).await;
    if cancelled { server.cancel.store(true, Ordering::Relaxed); } else { available.store(1, Ordering::Relaxed); }
    let error = crate::pipeline::deep_analyze::vlm_server_payload_ok(&server, &server.cancel).await.unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    let refused = probe_requests_fallback(Err(error.context("generated midbatch probe phase"))).unwrap_err();
    assert!(refused.downcast_ref::<HostAdmissionError>().is_some());
    listener.set_nonblocking(true).unwrap(); assert!(matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    assert!(server.process.poll_exit().unwrap().is_none()); assert!(!tracker.allows_fallback());
    let snap = s.snapshot(); assert_eq!((snap.memory_mb, snap.cpu, snap.io, snap.active, snap.waiting), (64, 0, 0, 1, 0));
    std::fs::write(root.join("refusal-receipt.json"), serde_json::to_vec(&serde_json::json!({"actualPayloadProbe": true, "typedRefusalRetainedThroughContext": true, "policyReturnedBatchErrorNotCliSelection": true, "httpDispatched": false, "cancelled": cancelled})).unwrap()).unwrap();
    finish(server, &tracker, &s).await;
}
#[tokio::test]
async fn probe_fallback_actual_admission_refusal_never_selects_cli() { refused_probe(false).await; }
#[tokio::test]
async fn probe_fallback_actual_cancelled_admission_never_selects_cli() { refused_probe(true).await; }

fn worker(listener: std::net::TcpListener, status: &'static str) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0u8; 4096]; let n = stream.read(&mut chunk).unwrap(); assert!(n > 0); bytes.extend_from_slice(&chunk[..n]); assert!(bytes.len() < 65536);
            if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]);
                let size: usize = header.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap();
                if bytes.len() >= end + 4 + size { assert!(header.contains("Bearer generated-auth")); break; }
            }
        }
        let body = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
        write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    })
}
#[tokio::test]
async fn probe_fallback_actual_healthy_payload_keeps_server_instead_of_cli() {
    let root = root(); let (server, tracker, s, _, listener) = fixture(&root).await;
    let worker = worker(listener, "200 OK");
    let result = crate::pipeline::deep_analyze::vlm_server_payload_ok(&server, &server.cancel).await;
    assert!(!probe_requests_fallback(result).unwrap()); worker.join().unwrap();
    assert!(server.process.poll_exit().unwrap().is_none()); assert_eq!(s.snapshot().memory_mb, 64);
    finish(server, &tracker, &s).await;
}
#[tokio::test]
async fn probe_fallback_actual_payload_rejection_requires_confirmed_cleanup_before_eligibility() {
    let root = root(); let (server, tracker, s, _, listener) = fixture(&root).await;
    let worker = worker(listener, "400 Bad Request");
    let result = crate::pipeline::deep_analyze::vlm_server_payload_ok(&server, &server.cancel).await;
    assert!(probe_requests_fallback(result).unwrap()); worker.join().unwrap();
    assert!(server.process.poll_exit().unwrap().is_none()); assert!(!tracker.allows_fallback());
    server.shutdown().await.unwrap(); assert!(tracker.allows_fallback()); assert_eq!(s.snapshot().memory_mb, 0);
    tracker.shutdown().await.unwrap();
}
