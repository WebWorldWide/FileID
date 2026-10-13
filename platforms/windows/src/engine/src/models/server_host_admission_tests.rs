use super::*;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::io::{Read, Write};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!("fileid-host-admission-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("image.png"), [0x89, b'P', b'N', b'G']).unwrap();
    println!("Generated host admission state retained: {}", root.display());
    root
}
fn scheduler() -> Arc<ResourceScheduler> { ResourceScheduler::new(|| (16384,12000), 1, 1) }
async fn wait_active(s: &ResourceScheduler, active: usize, waiting: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while s.snapshot().active != active || s.snapshot().waiting != waiting { tokio::task::yield_now().await; }
    }).await.unwrap();
}
async fn fixture(root: &Path, url: String, s: Arc<ResourceScheduler>, memory_mb: u64) -> (Arc<VlmServer>, Arc<ProcessTracker>) {
    let lease = reserve_host(s.clone(), Demand { memory_mb, cpu: 1, io: 1 }, Arc::new(AtomicBool::new(false))).await.unwrap();
    assert_eq!((s.snapshot().memory_mb, s.snapshot().cpu, s.snapshot().io), (memory_mb,1,1));
    let marker = root.join("native-ready");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "models::vlm_server::admission_tests::generated_admitted_server_child", "--ignored", "--nocapture"])
        .env("FILEID_GENERATED_HOST_READY", &marker).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let tracker = Arc::new(ProcessTracker::default());
    let process = OwnedProcess::spawn(&mut command, Some(lease.clone()), &tracker).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async { while !marker.exists() { tokio::task::yield_now().await; } }).await.unwrap();
    lease.retain_memory();
    assert_eq!((s.snapshot().memory_mb, s.snapshot().cpu, s.snapshot().io), (memory_mb,0,0));
    (Arc::new(VlmServer { last_timings: parking_lot::Mutex::new(None), process, base_url: url, api_key: "generated-auth".into(), client: build_loopback_client().unwrap(), scheduler:s, cancel:Arc::new(AtomicBool::new(false)), request_gate:tokio::sync::Mutex::new(()) }), tracker)
}
#[tokio::test]
#[ignore = "new generated admitted child; not an installed inference runtime"]
async fn generated_admitted_server_child() {
    std::fs::write(std::env::var_os("FILEID_GENERATED_HOST_READY").unwrap(), std::process::id().to_string()).unwrap();
    tokio::time::sleep(Duration::from_secs(60)).await;
}
fn request(stream: &mut std::net::TcpStream) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut bytes=Vec::new();
    loop {
        let mut chunk=[0u8;4096]; let n=stream.read(&mut chunk).unwrap(); assert!(n>0);bytes.extend_from_slice(&chunk[..n]);assert!(bytes.len()<65536);
        if let Some(end)=bytes.windows(4).position(|p|p==b"\r\n\r\n") {
            let header=String::from_utf8_lossy(&bytes[..end]);
            let size:usize=header.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse().unwrap())).unwrap();
            if bytes.len()>=end+4+size { assert!(header.contains("Bearer generated-auth"));break; }
        }
    }
}
fn reply(stream: &mut std::net::TcpStream) {
    let body=r#"{"choices":[{"message":{"content":"generated response, not inference"}}]}"#;
    let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
}
fn listener() -> (std::net::TcpListener,String) {
    let listener=std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0)).unwrap();
    let url=format!("http://{}",listener.local_addr().unwrap()); (listener,url)
}

#[tokio::test]
async fn host_admission_weight_estimate_is_owned_until_actual_child_reap() {
    let root=root();
    let weights=root.join("generated.gguf");let vision=root.join("generated.mmproj");
    for path in [&weights,&vision] { std::fs::File::create(path).unwrap().set_len(4*1_048_576).unwrap(); }
    let estimate=crate::util::model_memory::require_model_headroom(&weights,&vision).await.unwrap();assert_eq!(estimate,1034);
    let (_,url)=listener();let s=scheduler();let(server,tracker)=fixture(&root,url,s.clone(),estimate).await;
    drop(server);wait_active(&s,0,0).await;tracker.shutdown().await.unwrap();
    assert_eq!(s.snapshot().memory_mb,0);
}

#[tokio::test]
async fn host_admission_real_complete_serializes_requests_and_releases_only_work() {
    let root=root();let(listen,url)=listener();let s=scheduler();let(server,tracker)=fixture(&root,url,s.clone(),64).await;
    let observed=s.clone();let worker=std::thread::spawn(move || {
        for _ in 0..2 {
            let(mut stream,_)=listen.accept().unwrap();request(&mut stream);
            let snap=observed.snapshot();assert_eq!((snap.memory_mb,snap.cpu,snap.io,snap.active),(576,1,1,2));reply(&mut stream);
        }
    });
    let first=server.clone();let image=root.join("image.png");let image2=image.clone();
    let a=tokio::spawn(async move{first.complete(&image,"generated",1).await});
    let second=server.clone();let b=tokio::spawn(async move{second.complete(&image2,"generated",1).await});
    assert_eq!(a.await.unwrap().unwrap(),"generated response, not inference");assert!(b.await.unwrap().is_ok());worker.join().unwrap();
    assert_eq!((s.snapshot().memory_mb,s.snapshot().cpu,s.snapshot().io,s.snapshot().active),(64,0,0,1));
    drop(server);wait_active(&s,0,0).await;tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn host_admission_abandoned_pending_complete_cleans_queue_without_dispatch() {
    let root=root();let(listen,url)=listener();listen.set_nonblocking(true).unwrap();let s=scheduler();let(server,tracker)=fixture(&root,url,s.clone(),64).await;
    let holder=s.reserve(Demand{memory_mb:16,cpu:1,io:1},crate::resource_scheduler::Priority::Interactive,||false).unwrap();
    let target=server.clone();let image=root.join("image.png");let task=tokio::spawn(async move{target.complete(&image,"generated",1).await});
    wait_active(&s,2,1).await;task.abort();assert!(task.await.unwrap_err().is_cancelled());wait_active(&s,2,0).await;
    assert!(matches!(listen.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));assert!(server.process.poll_exit().unwrap().is_none());
    drop(holder);drop(server);wait_active(&s,0,0).await;tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn host_admission_abandoned_dispatched_complete_stops_actual_native_owner() {
    let root=root();let(listen,url)=listener();let s=scheduler();let(server,tracker)=fixture(&root,url,s.clone(),64).await;
    let(entered_tx,entered_rx)=tokio::sync::oneshot::channel();let(release_tx,release_rx)=std::sync::mpsc::channel();let observed=s.clone();
    let worker=std::thread::spawn(move || {let(mut stream,_)=listen.accept().unwrap();request(&mut stream);assert_eq!(observed.snapshot().memory_mb,576);entered_tx.send(()).unwrap();release_rx.recv_timeout(Duration::from_secs(5)).unwrap();reply(&mut stream);});
    let target=server.clone();let image=root.join("image.png");let task=tokio::spawn(async move{target.complete(&image,"generated",1).await});
    tokio::time::timeout(Duration::from_secs(5),entered_rx).await.unwrap().unwrap();task.abort();assert!(task.await.unwrap_err().is_cancelled());
    wait_active(&s,0,0).await;assert!(server.process.poll_exit().unwrap().is_some());release_tx.send(()).unwrap();worker.join().unwrap();
    drop(server);tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn host_admission_invalid_prompt_never_allocates_work_or_dispatches() {
    let root=root();let(listen,url)=listener();listen.set_nonblocking(true).unwrap();let s=scheduler();let(server,tracker)=fixture(&root,url,s.clone(),64).await;
    assert!(server.complete(&root.join("image.png"),&"x".repeat(32769),1).await.is_err());
    assert_eq!((s.snapshot().memory_mb,s.snapshot().active,s.snapshot().waiting),(64,1,0));assert!(matches!(listen.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    drop(server);wait_active(&s,0,0).await;tracker.shutdown().await.unwrap();
}

#[tokio::test]
async fn host_admission_abandoned_reservation_stays_cancellable_before_spawn() {
    let s=scheduler();let holder=s.reserve(Demand{memory_mb:16,cpu:1,io:1},crate::resource_scheduler::Priority::Interactive,||false).unwrap();
    let task=tokio::spawn(reserve_host(s.clone(),Demand{memory_mb:1034,cpu:1,io:1},Arc::new(AtomicBool::new(false))));
    wait_active(&s,1,1).await;task.abort();assert!(matches!(task.await,Err(error) if error.is_cancelled()));wait_active(&s,1,0).await;
    assert_eq!(s.snapshot().memory_mb,16);drop(holder);assert_eq!(s.snapshot().active,0);
}

#[tokio::test]
async fn host_admission_failure_is_typed_and_original_fallback_is_closed() {
    let s=ResourceScheduler::new(||(16384,500),1,1);
    let error=reserve_host(s,Demand{memory_mb:1034,cpu:1,io:1},Arc::new(AtomicBool::new(false))).await.err().expect("host admission must reject insufficient memory");
    assert!(error.downcast_ref::<super::super::native_admission::HostAdmissionError>().is_some());
    let code=include_str!("../commands/deep_analyze.rs");assert!(code.contains("native_admission::HostAdmissionError"));assert!(code.contains("let cleanup = s.shutdown().await;"));assert!(code.contains("if let Some(srv) = server.take()"));
    let server=include_str!("vlm_server.rs");assert!(server.contains("Some(resources.clone())"));assert!(server.contains("resources.retain_memory()"));
}
