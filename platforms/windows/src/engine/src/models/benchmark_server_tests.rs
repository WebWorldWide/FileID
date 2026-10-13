//! `benchmark::measure` against a loopback stand-in for llama-server. The
//! scripted replies decide correctness; the server reports `timings` as
//! llama-server does. No model or inference runtime is involved.
use super::*;
use super::super::benchmark::{measure, summarize, write_probes};
use std::io::{Read, Write};
use std::sync::atomic::AtomicU64;

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fileid-bench-server-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir(&path).unwrap();
    path
}

#[tokio::test]
#[ignore = "own executable generated child only, not inference runtime"]
async fn generated_benchmark_child() {
    std::fs::write(std::env::var_os("FILEID_BENCH_CHILD_READY").unwrap(), std::process::id().to_string()).unwrap();
    tokio::time::sleep(Duration::from_secs(60)).await;
}

/// Requests arrive in a fixed order: five probe answers, five captions, then
/// two moment sequences (the first answered with valid JSON, the second not).
fn answer_for(body: &str, served: usize) -> &'static str {
    if body.contains("red circles") { "There are 3 circles." }
    else if body.contains("large square") { "Blue" }
    else if body.contains("yellow circles") { "four" }
    else if body.contains("large circle") { "green." }
    else if body.contains("number is written") { "12345" }
    else if body.contains("ordered video frames") && served == 10 { "{\"moments\":[{\"title\":\"Shapes\",\"summary\":\"Coloured shapes.\",\"firstFrame\":0,\"lastFrame\":1}]}" }
    else if body.contains("ordered video frames") { "Not JSON at all" }
    else { "A plain caption of coloured shapes." }
}

fn serve(listener: std::net::TcpListener, requests: usize) -> std::thread::JoinHandle<usize> {
    std::thread::spawn(move || {
        let mut served = 0;
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut bytes = Vec::new();
            let mut length = None;
            loop {
                let mut buffer = [0; 65536];
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if length.is_none() {
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let content = headers.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap();
                        length = Some(end + 4 + content);
                    }
                }
                if length.is_some_and(|total| bytes.len() >= total) { break; }
            }
            let body = String::from_utf8_lossy(&bytes);
            let reply = serde_json::json!({
                "choices": [{"message": {"content": answer_for(&body, served)}}],
                "timings": {"prompt_n": 100, "prompt_per_second": 500.0 + served as f64, "predicted_n": 8, "predicted_per_second": 40.0}
            })
            .to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            served += 1;
        }
        served
    })
}

#[tokio::test]
async fn measure_scores_probes_structured_output_and_timings() {
    let root = root();
    let s = ResourceScheduler::new(|| (16384, 12000), 1, 1);
    let lease = reserve_host(s.clone(), Demand { memory_mb: 64, cpu: 1, io: 1 }, Arc::new(AtomicBool::new(false))).await.unwrap();
    let marker = root.join("ready");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "models::vlm_server::benchmark_server_tests::generated_benchmark_child", "--ignored", "--nocapture"])
        .env("FILEID_BENCH_CHILD_READY", &marker).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let tracker = ProcessTracker::default();
    let process = OwnedProcess::spawn(&mut command, Some(lease.clone()), &tracker).unwrap();
    assert!(process.pid().is_some());
    tokio::time::timeout(Duration::from_secs(5), async { while !marker.exists() { tokio::task::yield_now().await; } }).await.unwrap();
    lease.retain_memory();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = VlmServer { last_timings: parking_lot::Mutex::new(None), process, base_url: url, api_key: "generated-auth".into(), client: build_loopback_client().unwrap(), scheduler: s.clone(), cancel: Arc::new(AtomicBool::new(false)), request_gate: tokio::sync::Mutex::new(()) };

    let probes = write_probes(&root.join("probes")).unwrap();
    let worker = serve(listener, probes.len() * 2 + 2);
    let mut calls = Vec::new();
    let measurement = measure(&server, &probes, &Arc::new(AtomicBool::new(false)), |done, total| calls.push((done, total))).await.unwrap();
    assert_eq!(worker.join().unwrap(), 12);
    assert_eq!(calls.last(), Some(&(12, 12)));
    assert_eq!((measurement.correct, measurement.probes), (4, 5), "four of five scripted answers are right");
    assert_eq!((measurement.json_valid, measurement.json_total), (1, 2));
    assert_eq!(measurement.generated_rates.len(), 12);
    assert!(measurement.peak_resident_bytes > 0, "the child's memory is sampled");
    let summary = summarize(&measurement, "loopback", None);
    assert_eq!(summary.probe_accuracy, Some(0.8));
    assert_eq!(summary.generated_tokens_per_second, Some(40.0));
    assert_eq!(server.last_timings().map(|t| t.predicted_tokens), Some(8));
    server.shutdown().await.unwrap();
    tracker.shutdown().await.unwrap();
    assert_eq!(s.snapshot().memory_mb, 0);
    std::fs::remove_dir_all(root).unwrap();
}
