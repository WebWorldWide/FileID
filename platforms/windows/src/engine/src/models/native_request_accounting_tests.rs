use super::*;
use crate::models::native_admission::RequestWork;
use crate::resource_scheduler::{Demand, Priority, ResourceScheduler};
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt;

struct ControlledExit { gate: tokio::sync::watch::Receiver<bool>, killed: Arc<AtomicUsize> }
impl NativeChild for ControlledExit {
    fn start_kill(&mut self) -> std::io::Result<()> { self.killed.fetch_add(1,Ordering::Relaxed);Ok(()) }
    fn wait(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output=std::io::Result<std::process::ExitStatus>>+Send+'_>> {
        Box::pin(async move { while !*self.gate.borrow() { self.gate.changed().await.map_err(std::io::Error::other)?; } Ok(std::process::ExitStatus::from_raw(0)) })
    }
}
fn lease(s: &Arc<ResourceScheduler>, memory_mb:u64, cpu:usize) -> Arc<Lease> {
    Arc::new(s.reserve(Demand{memory_mb,cpu,io:cpu},Priority::Interactive,||false).unwrap())
}
fn process(s: &Arc<ResourceScheduler>) -> (OwnedProcess,ProcessTracker,tokio::sync::watch::Sender<bool>,Arc<AtomicUsize>) {
    let(gate,receive)=tokio::sync::watch::channel(false);let killed=Arc::new(AtomicUsize::new(0));let tracker=ProcessTracker::default();
    let mut registry=tracker.0.lock();
    let child=OwnedProcess::own(ControlledExit{gate:receive,killed:killed.clone()},Some(lease(s,64,0)),tracker.0.clone(),&mut registry.handles,None,None,Duration::from_secs(5));
    drop(registry);(child,tracker,gate,killed)
}
#[tokio::test]
async fn host_request_charge_abandonment_stays_until_confirmed_mock_reap() {
    let s=ResourceScheduler::new(||(16384,12000),1,1);let(child,tracker,gate,killed)=process(&s);
    let mut work=RequestWork::new(&child,lease(&s,512,1)).unwrap();work.begin_dispatch();drop(work);
    tokio::time::timeout(Duration::from_secs(3),async{while killed.load(Ordering::Relaxed)==0{tokio::task::yield_now().await;}}).await.unwrap();
    let snap=s.snapshot();assert_eq!((snap.memory_mb,snap.cpu,snap.io,snap.active),(576,1,1,2));assert!(tracker.has_unreaped());assert!(child.poll_exit().unwrap().is_none());
    gate.send_replace(true);child.wait_exit().await.unwrap();tracker.shutdown().await.unwrap();assert_eq!(s.snapshot().memory_mb,0);
}
#[tokio::test]
async fn host_request_duplicate_or_stopping_owner_never_replaces_work_charge() {
    let s=ResourceScheduler::new(||(16384,12000),1,1);let(child,tracker,gate,_)=process(&s);
    let mut work=RequestWork::new(&child,lease(&s,512,1)).unwrap();
    assert!(RequestWork::new(&child,lease(&s,16,0)).is_err());assert_eq!(s.snapshot().memory_mb,576);
    work.complete();drop(work);assert_eq!((s.snapshot().memory_mb,s.snapshot().cpu,s.snapshot().io),(64,0,0));
    child.request_stop();assert!(RequestWork::new(&child,lease(&s,512,1)).is_err());assert_eq!(s.snapshot().memory_mb,64);
    gate.send_replace(true);child.wait_exit().await.unwrap();tracker.shutdown().await.unwrap();assert_eq!(s.snapshot().memory_mb,0);
}
