use super::*;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;

const RESPONSE: &[u8] = br#"{"choices":[{"message":{"content":"generated terminal response, not inference"}}]}"#;

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "fileid-request-lifetime-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("image.png"), [0x89, b'P', b'N', b'G']).unwrap();
    println!("Generated request lifetime state retained: {}", path.display());
    path
}
fn listener() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (listener, url)
}
fn request(stream: &mut std::net::TcpStream) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        assert!(bytes.len() < 65536);
        if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            let size: usize = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + size {
                assert!(header.contains("Bearer generated-auth"));
                return;
            }
        }
    }
}
fn reply(stream: &mut std::net::TcpStream, status: &str, body: &[u8]) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
}
fn snapshot(s: &ResourceScheduler) -> (u64, usize, usize, usize) {
    let state = s.snapshot();
    (state.memory_mb, state.cpu, state.io, state.active)
}
#[cfg(unix)]
async fn wait_snapshot(s: &ResourceScheduler, expected: (u64, usize, usize, usize)) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while snapshot(s) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn fixture(root: &Path, url: String) -> (Arc<VlmServer>, Arc<ProcessTracker>, Arc<ResourceScheduler>) {
    let s = ResourceScheduler::new(|| (16384, 12000), 1, 1);
    let lease = reserve_host(
        s.clone(),
        Demand {
            memory_mb: 64,
            cpu: 1,
            io: 1,
        },
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    let marker = root.join("native-ready");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "models::vlm_server::terminal_lifetime_tests::generated_terminal_lifetime_child",
            "--ignored",
            "--nocapture",
        ])
        .env("FILEID_GENERATED_TERMINAL_READY", &marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let tracker = Arc::new(ProcessTracker::default());
    let process = OwnedProcess::spawn(&mut command, Some(lease.clone()), &tracker).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    lease.retain_memory();
    let server = Arc::new(VlmServer {
        last_timings: parking_lot::Mutex::new(None),
        process,
        base_url: url,
        api_key: "generated-auth".into(),
        client: build_loopback_client().unwrap(),
        scheduler: s.clone(),
        cancel: Arc::new(AtomicBool::new(false)),
        request_gate: tokio::sync::Mutex::new(()),
    });
    (server, tracker, s)
}
async fn finish(server: Arc<VlmServer>, tracker: Arc<ProcessTracker>, s: &ResourceScheduler) {
    Arc::try_unwrap(server).ok().unwrap().shutdown().await.unwrap();
    tracker.shutdown().await.unwrap();
    assert_eq!(snapshot(s), (0, 0, 0, 0));
}
#[tokio::test]
#[ignore = "own executable generated child, not a vendor inference runtime"]
async fn generated_terminal_lifetime_child() {
    std::fs::write(
        std::env::var_os("FILEID_GENERATED_TERMINAL_READY").unwrap(),
        std::process::id().to_string(),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_secs(60)).await;
}

#[tokio::test]
async fn request_lifetime_local_missing_image_preserves_native_and_next_completion() {
    let root = root();
    let (listen, url) = listener();
    let (server, tracker, s) = fixture(&root, url).await;
    assert!(server.complete(&root.join("absent.png"), "generated", 1).await.is_err());
    assert!(server.process.poll_exit().unwrap().is_none());
    assert_eq!(snapshot(&s), (64, 0, 0, 1));
    let observed = s.clone();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listen.accept().unwrap();
        request(&mut stream);
        assert_eq!(snapshot(&observed), (576, 1, 1, 2));
        reply(&mut stream, "200 OK", RESPONSE);
    });
    assert!(server
        .complete(&root.join("image.png"), "generated", 1)
        .await
        .unwrap()
        .starts_with("generated terminal"));
    worker.join().unwrap();
    assert_eq!(snapshot(&s), (64, 0, 0, 1));
    finish(server, tracker, &s).await;
}
async fn terminal_error_keeps_native(status: &'static str, body: &'static [u8], expected_error: &str) {
    let root = root();
    let (listen, url) = listener();
    let (server, tracker, s) = fixture(&root, url).await;
    let observed = s.clone();
    let worker = std::thread::spawn(move || {
        for (status, body) in [(status, body), ("200 OK", RESPONSE)] {
            let (mut stream, _) = listen.accept().unwrap();
            request(&mut stream);
            assert_eq!(snapshot(&observed), (576, 1, 1, 2));
            reply(&mut stream, status, body);
        }
    });
    let error = server
        .complete(&root.join("image.png"), "generated", 1)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains(expected_error));
    assert!(server.process.poll_exit().unwrap().is_none());
    assert_eq!(snapshot(&s), (64, 0, 0, 1));
    assert!(server
        .complete(&root.join("image.png"), "generated", 1)
        .await
        .unwrap()
        .starts_with("generated terminal"));
    worker.join().unwrap();
    assert_eq!(snapshot(&s), (64, 0, 0, 1));
    finish(server, tracker, &s).await;
}
#[tokio::test]
async fn request_lifetime_full_400_preserves_native_and_next_completion() {
    terminal_error_keeps_native("400 Bad Request", b"generated payload refusal", "400").await;
}
#[tokio::test]
async fn request_lifetime_full_malformed_json_preserves_native_and_next_completion() {
    terminal_error_keeps_native("200 OK", b"not JSON", "parse VLM response JSON").await;
}
#[tokio::test]
async fn request_lifetime_full_invalid_utf8_preserves_native_and_next_completion() {
    terminal_error_keeps_native("200 OK", &[0xff], "wasn't UTF-8").await;
}
#[tokio::test]
async fn request_lifetime_full_missing_content_preserves_native_and_next_completion() {
    terminal_error_keeps_native("200 OK", b"{}", "missing choices").await;
}

#[tokio::test]
async fn request_lifetime_truncated_400_requests_stop_not_safe_release() {
    let root = root();
    let (listen, url) = listener();
    let (server, tracker, s) = fixture(&root, url).await;
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listen.accept().unwrap();
        request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 100\r\nConnection: close\r\n\r\nx")
            .unwrap();
    });
    let error = server
        .complete(&root.join("image.png"), "generated", 1)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("read VLM response chunk"));
    server.process.wait_exit().await.unwrap();
    worker.join().unwrap();
    assert_eq!(snapshot(&s), (0, 0, 0, 0));
    finish(server, tracker, &s).await;
}

#[tokio::test]
async fn request_lifetime_native_exit_keeps_live_http_host_charge_until_terminal() {
    let root = root();
    let (listen, url) = listener();
    let (server, tracker, s) = fixture(&root, url).await;
    let (ready, observed) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listen.accept().unwrap();
        request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            RESPONSE.len()
        )
        .unwrap();
        stream.write_all(&RESPONSE[..1]).unwrap();
        ready.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        stream.write_all(&RESPONSE[1..]).unwrap();
    });
    let owner = server.clone();
    let path = root.join("image.png");
    let task = tokio::spawn(async move { owner.complete(&path, "generated", 1).await });
    observed.await.unwrap();
    assert_eq!(snapshot(&s), (576, 1, 1, 2));
    server.process.request_stop();
    server.process.wait_exit().await.unwrap();
    assert_eq!(snapshot(&s), (512, 1, 1, 1));
    assert!(!task.is_finished());
    release.send(()).unwrap();
    assert!(task.await.unwrap().unwrap().starts_with("generated terminal"));
    worker.join().unwrap();
    assert_eq!(snapshot(&s), (0, 0, 0, 0));
    finish(server, tracker, &s).await;
}

#[cfg(unix)]
#[tokio::test]
async fn request_lifetime_abandoned_predispatch_read_keeps_worker_charge_after_native_exit() {
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    let root = root();
    let (_, url) = listener();
    let (server, tracker, s) = fixture(&root, url).await;
    let fifo = root.join("generated-blocked-image.png");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let owner = server.clone();
    let path = fifo.clone();
    let task = tokio::spawn(async move { owner.complete(&path, "generated", 1).await });
    let mut writer = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
            {
                Ok(writer) => break writer,
                Err(error) if error.raw_os_error() == Some(libc::ENXIO) => tokio::task::yield_now().await,
                Err(error) => panic!("generated FIFO writer: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(snapshot(&s), (576, 1, 1, 2));
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(server.process.poll_exit().unwrap().is_none());
    assert_eq!(snapshot(&s), (576, 1, 1, 2));
    server.process.request_stop();
    server.process.wait_exit().await.unwrap();
    assert_eq!(snapshot(&s), (512, 1, 1, 1));
    writer.write_all(&[0x89, b'P', b'N', b'G']).unwrap();
    drop(writer);
    wait_snapshot(&s, (0, 0, 0, 0)).await;
    finish(server, tracker, &s).await;
}
