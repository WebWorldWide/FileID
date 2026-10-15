use std::{path::PathBuf, sync::{Arc, OnceLock, atomic::{AtomicBool, Ordering}}, time::{Instant, Duration}, future::Future};
use anyhow::{anyhow, Result};
use super::{vlm_server::VlmServer, model_provenance::VerifiedModel, runtime_cache::RuntimeInputs, native_admission::HostAdmissionError};
use crate::resource_scheduler::ResourceScheduler;
#[derive(PartialEq,Eq)]
pub(crate) struct Key {pub(crate) cpu:bool,pub(crate) runtime:Option<RuntimeInputs>}
struct Entry {proof:Arc<VerifiedModel>,root:PathBuf,key:Key,server:VlmServer}
struct State {entry:Option<Entry>,last_used:Instant,failure:Option<String>}
pub(crate) struct ResidentService {
    state:Arc<tokio::sync::Mutex<State>>,closed:AtomicBool,stopped:tokio::sync::Notify,
    idle:Duration,scheduler:Arc<ResourceScheduler>,cpu:Box<dyn Fn()->bool+Send+Sync>,
}
pub(crate) struct ResidentLease {service:Arc<ResidentService>,state:tokio::sync::OwnedMutexGuard<State>}
impl std::ops::Deref for ResidentLease {type Target=VlmServer;fn deref(&self)->&Self::Target{&self.state.entry.as_ref().expect("live resident lease").server}}
impl ResidentService {
    pub(crate) fn shared()->Arc<Self> {
        static SHARED:OnceLock<Arc<ResidentService>>=OnceLock::new();
        SHARED.get_or_init(|| {
            let service=Self::new(ResourceScheduler::shared(),Duration::from_secs(120),super::runtime::user_forced_cpu);
            let owner=service.clone();tokio::spawn(async move {
                let mut tick=tokio::time::interval(Duration::from_secs(1));
                loop {tokio::select!{biased;_ = owner.stopped.notified()=>break,_=tick.tick()=>{if owner.closed.load(Ordering::Relaxed){break;}
                    if let Err(error)=owner.sweep().await{tracing::error!(%error,"Resident cleanup failed; native tracker retains fault");}}}}
            });service
        }).clone()
    }
    pub(crate) fn new(scheduler:Arc<ResourceScheduler>,idle:Duration,cpu:impl Fn()->bool+Send+Sync+'static)->Arc<Self> {
        Arc::new(Self{state:Arc::new(tokio::sync::Mutex::new(State{entry:None,last_used:Instant::now(),failure:None})),closed:AtomicBool::new(false),stopped:tokio::sync::Notify::new(),idle,scheduler,cpu:Box::new(cpu)})
    }
    pub(crate) async fn acquire(self:&Arc<Self>,kind:&str,cancel:Arc<AtomicBool>)->Result<ResidentLease> {
        let root=crate::paths::models_dir()?;let proof_cancel=cancel.clone();let key_cancel=cancel.clone();let load_cancel=cancel.clone();
        self.acquire_with(kind,root,cancel,
            ||super::model_provenance::verify_installed(kind,proof_cancel),
            ||{let c=key_cancel.clone();async move {let cpu=super::runtime::user_forced_cpu();let runtime=if cpu{Some(super::runtime_cache::capture(c).await?)}else{None};Ok(Key{cpu,runtime})}},
            |proof|async move {let(a,b)=proof.paths();VlmServer::start(a,b,&load_cancel).await}).await
    }
    pub(crate) async fn acquire_with<P:Future<Output=Result<VerifiedModel>>,K:Future<Output=Result<Key>>,L:Future<Output=Result<VlmServer>>>(
        self:&Arc<Self>,kind:&str,root:PathBuf,cancel:Arc<AtomicBool>,proof:impl FnOnce()->P,mut key:impl FnMut()->K,load:impl FnOnce(Arc<VerifiedModel>)->L,
    )->Result<ResidentLease> {
        if self.closed.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident owner closed")).into());}
        let mut state=tokio::select!{biased;_=wait_cancelled(&cancel)=>return Err(HostAdmissionError(anyhow!("Resident checkout cancelled")).into()),state=self.state.clone().lock_owned()=>state};
        if self.closed.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident owner closed")).into());}
        if let Some(failure)=&state.failure{return Err(HostAdmissionError(anyhow!(failure.clone())).into());}
        let verified=match &state.entry {
            Some(entry) if entry.root==root&&entry.proof.matches_kind(kind)=>{
                let verified=entry.proof.clone();
                if let Err(error)=verified.recheck(cancel.clone()).await {
                    if !cancel.load(Ordering::Relaxed) {
                        if let Err(cleanup)=retire(&mut state).await {return Err(HostAdmissionError(anyhow!("{error:#}; resident cleanup: {cleanup:#}")).into());}
                    }
                    return Err(error);
                }
                verified
            },
            _=>Arc::new(proof().await?),
        };
        if !verified.matches_kind(kind) {return Err(HostAdmissionError(anyhow!("Resident proof kind does not match request")).into());}
        if self.closed.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident owner closed")).into());}
        if cancel.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident checkout cancelled")).into());}
        let current=key().await?;
        if self.closed.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident owner closed")).into());}
        if cancel.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident checkout cancelled")).into());}
        let reusable=state.entry.as_ref().is_some_and(|entry|entry.root==root&&entry.proof.matches_kind(kind)&&entry.key==current&&current.cpu&&entry.server.reusable_idle());
        if reusable {
            if self.closed.load(Ordering::Relaxed){retire(&mut state).await?;return Err(HostAdmissionError(anyhow!("Resident owner closed")).into());}
            if cancel.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident checkout cancelled")).into());}
            tracing::info!(model=kind,"Reusing owned CPU-policy process; native provider acceptance separate");
            return Ok(ResidentLease{service:self.clone(),state});
        }
        retire(&mut state).await?;
        if self.closed.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident owner closed")).into());}
        if cancel.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident checkout cancelled")).into());}
        let server=load(verified.clone()).await?;
        let validity=async {
            if self.closed.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident owner closed")).into());}
            if cancel.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident checkout cancelled")).into());}
            verified.recheck(cancel.clone()).await?;
            if self.closed.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident owner closed")).into());}
            if cancel.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident checkout cancelled")).into());}
            if key().await?!=current{return Err(HostAdmissionError(anyhow!("Resident runtime/policy changed during startup")).into());}
            if self.closed.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident owner closed")).into());}
            if cancel.load(Ordering::Relaxed){return Err(HostAdmissionError(anyhow!("Resident checkout cancelled")).into());}
            Ok::<_,anyhow::Error>(())
        }.await;
        if let Err(error)=validity {
            let reason=format!("{error:#}");
            let cleanup=server.shutdown().await;
            if let Err(failure)=&cleanup {
                let sticky=format!("{reason}; resident startup cleanup failed: {failure:#}");
                state.failure=Some(sticky.clone());
                return Err(HostAdmissionError(anyhow!(sticky)).into());
            }
            let typed=if error.downcast_ref::<HostAdmissionError>().is_some(){error}else{HostAdmissionError(error).into()};
            return Err(cleanup.err().map_or(typed,|failure|HostAdmissionError(anyhow!("{reason}; resident startup cleanup failed: {failure:#}")).into()));
        }
        if self.closed.load(Ordering::Relaxed){
            let cleanup=server.shutdown().await;
            if let Err(failure)=cleanup {
                let reason=format!("Resident cleanup failed: {failure:#}");
                state.failure=Some(reason.clone());
                return Err(HostAdmissionError(anyhow!("Resident owner closed; {reason}")).into());
            }
            return Err(HostAdmissionError(anyhow!("Resident owner closed")).into());
        }
        if cancel.load(Ordering::Relaxed){
            let cleanup=server.shutdown().await;
            if let Err(failure)=cleanup {
                let reason=format!("Resident cleanup failed: {failure:#}");
                state.failure=Some(reason.clone());
                return Err(HostAdmissionError(anyhow!("Resident checkout cancelled; {reason}")).into());
            }
            return Err(HostAdmissionError(anyhow!("Resident checkout cancelled")).into());
        }
        state.entry=Some(Entry{proof:verified,root,key:current,server});state.last_used=Instant::now();
        Ok(ResidentLease{service:self.clone(),state})
    }
    pub(crate) async fn sweep(&self)->Result<()> {
        let Ok(mut state)=self.state.try_lock() else{return Ok(());};
        if state.entry.is_some()&&(self.closed.load(Ordering::Relaxed)||self.scheduler.under_pressure()||state.last_used.elapsed()>=self.idle||!state.entry.as_ref().unwrap().server.reusable_idle()){retire(&mut state).await?;}
        Ok(())
    }
    pub(crate) async fn shutdown(&self)->Result<()> {
        self.closed.store(true,Ordering::Relaxed);self.stopped.notify_one();
        let mut state=tokio::time::timeout(Duration::from_secs(8),self.state.lock()).await.map_err(|_|HostAdmissionError(anyhow!("Resident shutdown waits on active job; native tracker must reap")))?;
        retire(&mut state).await
    }
}
impl ResidentLease {
    pub(crate) async fn shutdown(mut self)->Result<()> {retire(&mut self.state).await}
    pub(crate) async fn finish(mut self,keep:bool)->Result<()> {
        let reusable=keep&&!self.service.closed.load(Ordering::Relaxed)&&(self.service.cpu)()&&!self.service.scheduler.under_pressure()&&self.state.entry.as_ref().is_some_and(|entry|entry.key.cpu&&entry.server.reusable_idle());
        if !reusable{return retire(&mut self.state).await;}
        self.state.last_used=Instant::now();tracing::debug!("CPU-policy process idle in owned cache; host residency remains charged");Ok(())
    }
}
impl Drop for ResidentLease {fn drop(&mut self){self.state.last_used=Instant::now();}}
async fn retire(state:&mut State)->Result<()> {
    if let Some(entry)=state.entry.take(){if let Err(error)=entry.server.shutdown().await{let reason=format!("Resident cleanup failed: {error:#}");state.failure=Some(reason.clone());return Err(HostAdmissionError(anyhow!(reason)).into());}}
    if let Some(failure)=&state.failure{return Err(HostAdmissionError(anyhow!(failure.clone())).into());}Ok(())
}
async fn wait_cancelled(cancel:&AtomicBool){while !cancel.load(Ordering::Relaxed){tokio::time::sleep(Duration::from_millis(25)).await;}}
