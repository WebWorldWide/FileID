use super::{registry, vlm_server::VlmServer, native_process::ProcessTracker};
use crate::resource_scheduler::{AcceleratorDemand, Demand, Lease, Priority, ResourceScheduler};
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::{io::Read, path::{Path, PathBuf}, sync::{atomic::{AtomicBool, Ordering}, Arc, OnceLock}, time::{Duration, Instant}};

pub const DEFAULT_MODEL: &str = "qwen2_5_vl_7b";
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ModelArtifact { pub path: PathBuf, pub sha256: String, pub source_url: String }
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ModelProvenance { pub model_kind: String, pub registry_id: String, pub artifacts: Vec<ModelArtifact> }
struct Resident { server: VlmServer, provenance: ModelProvenance, forced_cpu: bool, last_used: Instant }
#[derive(Default)]
struct State { resident: Option<Resident>, active: Option<ModelProvenance>, verifying: usize }
pub struct ResidentModelService {
    state: Arc<Mutex<State>>,
    scheduler: Arc<ResourceScheduler>,
    queue: Arc<ResourceScheduler>,
    stopped: Arc<AtomicBool>,
    processes: Arc<ProcessTracker>,
}
struct PendingCancellation(Arc<AtomicBool>);
impl Drop for PendingCancellation { fn drop(&mut self) { self.0.store(true, Ordering::Relaxed); } }
struct VerificationWork(Arc<Mutex<State>>);
impl Drop for VerificationWork { fn drop(&mut self) { self.0.lock().verifying -= 1; } }

impl ResidentModelService {
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<ResidentModelService>> = OnceLock::new();
        SHARED.get_or_init(|| Self::new(ResourceScheduler::shared())).clone()
    }
    pub fn new(scheduler: Arc<ResourceScheduler>) -> Arc<Self> {
        let service = Arc::new(Self {
            state: Arc::new(Mutex::new(State::default())), scheduler,
            queue: ResourceScheduler::new(|| (16384, 12000), 1, 1),
            stopped: Arc::new(AtomicBool::new(false)), processes: Arc::new(ProcessTracker::default()),
        });
        let weak = Arc::downgrade(&service);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Some(service) = weak.upgrade() else { break };
                if let Err(error) = service.evict_idle().await { tracing::error!(%error, "Resident eviction cleanup failed"); }
                if service.stopped.load(Ordering::Relaxed) { break; }
            }
        });
        service
    }
    pub fn active_model(&self) -> Option<ModelProvenance> {
        let state = self.state.lock();
        if !self.stopped.load(Ordering::Relaxed) && (state.resident.is_some() || self.queue.snapshot().active > 0) { state.active.clone() } else { None }
    }
    pub async fn resolve_model(&self, model: Option<&str>, explicit_load: bool) -> Result<ModelProvenance> {
        let active = self.active_model();
        let kind = model.map(str::to_owned).or_else(|| active.map(|p| p.model_kind))
            .or_else(|| explicit_load.then(|| DEFAULT_MODEL.to_owned()))
            .context("No active local model; explicit load intent required")?;
        let cancel = Arc::new(AtomicBool::new(false));
        let _turn = self.enter(Priority::Interactive, cancel.clone()).await?;
        self.verify(kind, Priority::Interactive, cancel).await
    }
    async fn verify(&self, kind: String, priority: Priority, cancel: Arc<AtomicBool>) -> Result<ModelProvenance> {
        self.verify_with(priority, cancel, move |cancelled| verify_installed(&kind, cancelled)).await
    }
    async fn verify_with(&self, priority: Priority, cancel: Arc<AtomicBool>, job: impl FnOnce(&dyn Fn() -> bool) -> Result<ModelProvenance> + Send + 'static) -> Result<ModelProvenance> {
        let work = {
            let mut state = self.state.lock();
            if self.stopped.load(Ordering::Relaxed) { bail!("Model verification cancelled"); }
            state.verifying += 1;
            VerificationWork(self.state.clone())
        };
        let abandoned = Arc::new(AtomicBool::new(false));
        let _guard = PendingCancellation(abandoned.clone());
        let stopped = self.stopped.clone();
        let scheduler = self.scheduler.clone();
        tokio::task::spawn_blocking(move || {
            let _work = work;
            let cancelled = || cancel.load(Ordering::Relaxed) || stopped.load(Ordering::Relaxed) || abandoned.load(Ordering::Relaxed);
            let _lease = scheduler.reserve_queued(Demand { memory_mb: 0, cpu: 1, io: 1 }, AcceleratorDemand::default(), priority, cancelled)?;
            job(&cancelled)
        }).await?
    }
    async fn reserve(&self, scheduler: Arc<ResourceScheduler>, demand: Demand, accelerator: AcceleratorDemand, priority: Priority, cancel: Arc<AtomicBool>) -> Result<Lease> {
        let abandoned = Arc::new(AtomicBool::new(false));
        let _guard = PendingCancellation(abandoned.clone());
        let stopped = self.stopped.clone();
        tokio::task::spawn_blocking(move || scheduler.reserve_queued(demand, accelerator, priority,
            || cancel.load(Ordering::Relaxed) || abandoned.load(Ordering::Relaxed) || stopped.load(Ordering::Relaxed))).await?
    }
    async fn enter(&self, priority: Priority, cancel: Arc<AtomicBool>) -> Result<Lease> {
        super::runtime::ensure_gpu_inference_alive()?;
        let lease = self.reserve(self.queue.clone(), Demand { cpu: 1, ..Demand::default() }, AcceleratorDemand::default(), priority, cancel).await?;
        super::runtime::ensure_gpu_inference_alive()?;
        Ok(lease)
    }
    async fn take_or_load(&self, model: Option<&str>, cancel: Arc<AtomicBool>, priority: Priority) -> Result<Resident> {
        let previous = {
            let mut state = self.state.lock();
            if self.stopped.load(Ordering::Relaxed) { bail!("Model load cancelled"); }
            state.resident.take()
        };
        if let Some(resident) = previous {
            let matches = model.is_none_or(|kind| registry::vlm_dir_name(kind) == registry::vlm_dir_name(&resident.provenance.model_kind));
            if matches && resident.forced_cpu == super::runtime::user_forced_cpu() && !self.scheduler.under_pressure() { return Ok(resident); }
            self.state.lock().active = None;
            resident.server.shutdown().await?;
        }
        self.state.lock().active = None;
        let provenance = self.verify(model.unwrap_or(DEFAULT_MODEL).to_owned(), priority, cancel.clone()).await?;
        let gguf = &provenance.artifacts[0].path;
        let mmproj = &provenance.artifacts[1].path;
        let memory_mb = tokio::fs::metadata(gguf).await?.len().saturating_add(tokio::fs::metadata(mmproj).await?.len()).div_ceil(1_048_576);
        let memory_mb = memory_mb.saturating_add(memory_mb.div_ceil(4)).saturating_add(1024);
        let gpu_mb = if super::runtime::user_forced_cpu() { 0 } else {
            super::runtime::verified_gpu_capacity_mb().filter(|capacity| memory_mb.saturating_add(self.scheduler.snapshot().gpu_mb) <= *capacity).map(|_| memory_mb).unwrap_or(0)
        };
        let lease = Arc::new(self.reserve(self.scheduler.clone(), Demand { memory_mb, cpu: 1, io: 1 }, AcceleratorDemand { gpu_mb, npu_mb: 0 }, priority, cancel.clone()).await?);
        let server = tokio::select! {
            biased;
            _ = wait_cancelled(&cancel, &self.stopped) => bail!("Model load cancelled"),
            result = VlmServer::start(gguf, mmproj, &cancel, gpu_mb > 0, lease.clone(), &self.processes) => result?,
        };
        lease.retain_memory();
        Ok(Resident { server, provenance, forced_cpu: super::runtime::user_forced_cpu(), last_used: Instant::now() })
    }
    async fn retain(&self, mut resident: Resident) -> Result<()> {
        resident.last_used = Instant::now();
        let rejected = {
            let mut state = self.state.lock();
            if self.stopped.load(Ordering::Relaxed) { Some(resident) }
            else { state.active = Some(resident.provenance.clone()); state.resident = Some(resident); None }
        };
        if let Some(resident) = rejected { resident.server.shutdown().await?; bail!("Model retention cancelled by shutdown"); }
        Ok(())
    }
    pub async fn load_installed(&self, model: Option<&str>, priority: Priority, cancel: Arc<AtomicBool>) -> Result<ModelProvenance> {
        let _turn = self.enter(priority, cancel.clone()).await?;
        let resident = self.take_or_load(model, cancel, priority).await?;
        self.complete_load(resident).await
    }
    async fn complete_load(&self, resident: Resident) -> Result<ModelProvenance> {
        let provenance = resident.provenance.clone();
        self.retain(resident).await?;
        Ok(provenance)
    }
    pub async fn caption(&self, model: &str, image: &Path, prompt: &str, max_tokens: u32, priority: Priority, cancel: Arc<AtomicBool>) -> Result<String> {
        self.caption_stream(model, image, prompt, max_tokens, priority, cancel, |_| {}).await
    }
    pub async fn caption_stream(&self, model: &str, image: &Path, prompt: &str, max_tokens: u32, priority: Priority, cancel: Arc<AtomicBool>, on_token: impl FnMut(&str)) -> Result<String> {
        super::vlm_server::validate_generation("", prompt, max_tokens)?;
        let _turn = self.enter(priority, cancel.clone()).await?;
        let resident = self.take_or_load(Some(model), cancel.clone(), priority).await?;
        let compute = self.reserve(self.scheduler.clone(), Demand { memory_mb: 384, cpu: 1, io: 1 }, AcceleratorDemand::default(), priority, cancel.clone()).await;
        let result = match compute {
            Ok(compute) => {
                resident.server.retain_work(Arc::new(compute))?;
                tokio::select! {
                    biased;
                    _ = wait_cancelled(&cancel, &self.stopped) => Err(anyhow::anyhow!("Inference cancelled")),
                    result = resident.server.complete_stream(image, prompt, max_tokens, on_token) => result,
                }
            }
            Err(err) => Err(err),
        };
        if result.is_ok() { resident.server.release_work(); }
        self.finish(resident, &result).await?;
        result
    }
    #[allow(dead_code)]
    pub async fn generate_text(&self, model: Option<&str>, system: &str, prompt: &str, max_tokens: u32, priority: Priority, cancel: Arc<AtomicBool>, on_token: impl FnMut(&str)) -> Result<String> {
        super::vlm_server::validate_generation(system, prompt, max_tokens)?;
        let _turn = self.enter(priority, cancel.clone()).await?;
        let resident = self.take_or_load(model, cancel.clone(), priority).await?;
        let compute = self.reserve(self.scheduler.clone(), Demand { memory_mb: 128, cpu: 1, io: 0 }, AcceleratorDemand::default(), priority, cancel.clone()).await;
        let result = match compute {
            Ok(compute) => {
                resident.server.retain_work(Arc::new(compute))?;
                tokio::select! {
                    biased;
                    _ = wait_cancelled(&cancel, &self.stopped) => Err(anyhow::anyhow!("Inference cancelled")),
                    result = resident.server.generate_text(system, prompt, max_tokens, on_token) => result,
                }
            }
            Err(err) => Err(err),
        };
        if result.is_ok() { resident.server.release_work(); }
        self.finish(resident, &result).await?;
        result
    }
    async fn finish(&self, resident: Resident, result: &Result<String>) -> Result<()> {
        if result.is_err() || crate::coordinator::process_gpu_device_removed() {
            self.state.lock().active = None;
            resident.server.shutdown().await
        } else { self.retain(resident).await }
    }
    pub async fn evict_idle(&self) -> Result<bool> {
        let resident = {
            let mut state = self.state.lock();
            if state.resident.as_ref().is_some_and(|r| self.stopped.load(Ordering::Relaxed) || self.scheduler.under_pressure() || r.last_used.elapsed() >= Duration::from_secs(60)) {
                state.active = None; state.resident.take()
            } else { None }
        };
        if let Some(resident) = resident { resident.server.shutdown().await?; Ok(true) } else { Ok(false) }
    }
    #[allow(dead_code)]
    pub async fn release(&self) -> Result<bool> {
        let resident = {
            let mut state = self.state.lock();
            if state.resident.is_some() { state.active = None; state.resident.take() } else { None }
        };
        if let Some(resident) = resident { resident.server.shutdown().await?; Ok(true) } else { Ok(false) }
    }
    #[allow(dead_code)]
    pub async fn shutdown(&self) -> Result<()> {
        { let mut state = self.state.lock(); self.stopped.store(true, Ordering::Relaxed); state.active = None; }
        let first = self.release().await.err();
        while {
            let snapshot = self.queue.snapshot();
            snapshot.active > 0 || snapshot.waiting > 0 || self.state.lock().verifying > 0
        } { tokio::time::sleep(Duration::from_millis(25)).await; }
        let last = self.release().await.err();
        let cleanup = self.processes.shutdown().await.err();
        if let Some(error) = first.or(last).or(cleanup) { return Err(error); }
        Ok(())
    }
}
async fn wait_cancelled(cancel: &AtomicBool, stopped: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) && !stopped.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if crate::coordinator::process_gpu_device_removed() { break; }
    }
}
fn verify_installed(kind: &str, cancelled: &dyn Fn() -> bool) -> Result<ModelProvenance> {
    if registry::vlm_dir_name(kind).is_none() { bail!("Unknown caption/text model"); }
    let registry::LookupResult::Found(model) = registry::lookup_full(kind) else { bail!("Unknown local model"); };
    verify_files(kind, model.id, model.files, cancelled)
}
fn verify_files(kind: &str, registry_id: &str, files: Vec<registry::FileEntry>, cancelled: &dyn Fn() -> bool) -> Result<ModelProvenance> {
    let mut artifacts = Vec::new();
    for file in files {
        let expected = file.sha256.context("Local model lacks a SHA256 pin")?;
        if cancelled() { bail!("Model verification cancelled"); }
        let before = std::fs::metadata(&file.dest).context("Installed model is missing; no download was started")?;
        if !before.is_file() || before.len() == 0 { bail!("Model weights must be nonempty regular files"); }
        let mut source = std::fs::File::open(&file.dest).context("Installed local model is missing; no download was started")?;
        let mut hasher = Sha256::new();
        let mut buffer = [0; 1024 * 1024];
        loop { if cancelled() { bail!("Model verification cancelled"); } let count = source.read(&mut buffer)?; if count == 0 { break; } hasher.update(&buffer[..count]); }
        if cancelled() { bail!("Model verification cancelled"); }
        if hex::encode(hasher.finalize()) != expected { bail!("Installed local model failed pinned SHA256 verification"); }
        let after = std::fs::metadata(&file.dest)?;
        if before.len() != after.len() || before.modified()? != after.modified()? { bail!("Model weights changed during verification"); }
        artifacts.push(ModelArtifact { path: file.dest, sha256: expected, source_url: file.url });
    }
    if artifacts.len() != 2 { bail!("Caption model must include weights and projector"); }
    Ok(ModelProvenance { model_kind: kind.to_owned(), registry_id: registry_id.to_owned(), artifacts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    async fn fixture(service: &ResidentModelService) {
        let lease = service.scheduler.reserve(Demand { memory_mb: 1024, cpu: 0, io: 0 }, Priority::Background, || false).unwrap();
        let lease = Arc::new(lease);
        let server = VlmServer::fixture("http://127.0.0.1:1".into(), lease, &service.processes);
        let provenance = ModelProvenance { model_kind: DEFAULT_MODEL.into(), registry_id: "fixture".into(), artifacts: Vec::new() };
        service.state.lock().active = Some(provenance.clone());
        service.retain(Resident { server, provenance, forced_cpu: false, last_used: Instant::now() }).await.unwrap();
    }
    fn staged_resident(service: &ResidentModelService) -> Resident {
        let lease = Arc::new(service.scheduler.reserve(Demand { memory_mb: 1024, cpu: 1, io: 1 }, Priority::Interactive, || false).unwrap());
        let server = VlmServer::fixture("http://127.0.0.1:1".into(), lease, &service.processes);
        Resident { server, provenance: ModelProvenance { model_kind: DEFAULT_MODEL.into(), registry_id: "generated-fixture".into(), artifacts: Vec::new() }, forced_cpu: true, last_used: Instant::now() }
    }
    async fn retention_race(load: bool) {
        let service = ResidentModelService::new(ResourceScheduler::new(|| (16384, 12000), 1, 1));
        let entered = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let (pid_send, pid_receive) = tokio::sync::oneshot::channel();
        let operation = { let service = service.clone(); let entered = entered.clone(); let resume = resume.clone(); tokio::spawn(async move {
            let _turn = service.enter(Priority::Interactive, Arc::new(AtomicBool::new(false))).await.unwrap();
            let resident = staged_resident(&service);
            pid_send.send(resident.server.process_id()).unwrap();
            entered.wait().await;
            resume.wait().await;
            if load { service.complete_load(resident).await.map(|_| ()) }
            else { service.finish(resident, &Ok("completed fixture request".into())).await }
        }) };
        entered.wait().await;
        let pid = pid_receive.await.unwrap();
        let shutdown = { let service = service.clone(); tokio::spawn(async move { service.shutdown().await }) };
        while !service.stopped.load(Ordering::Relaxed) { tokio::task::yield_now().await; }
        assert!(!shutdown.is_finished());
        assert_eq!(service.scheduler.snapshot().memory_mb, 1024);
        resume.wait().await;
        assert!(operation.await.unwrap().is_err());
        tokio::time::timeout(Duration::from_secs(3), shutdown).await.unwrap().unwrap().unwrap();
        assert!(!super::super::native_process::test_child_alive(pid));
        assert!(service.state.lock().resident.is_none());
        assert!(service.active_model().is_none());
        assert_eq!(service.scheduler.snapshot().active, 0);
    }
    #[tokio::test]
    async fn admitted_load_commit_vs_shutdown_is_a_reaping_barrier() { retention_race(true).await; }
    #[tokio::test]
    async fn completed_inference_retention_vs_shutdown_is_a_reaping_barrier() { retention_race(false).await; }

    struct HashFixture { root: PathBuf, files: Vec<registry::FileEntry> }
    impl HashFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("fileid-hash-fixture-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let bytes = vec![37; 2 * 1024 * 1024];
            let sha = hex::encode(Sha256::digest(&bytes));
            let files = ["model.gguf", "mmproj.gguf"].into_iter().map(|name| {
                let dest = root.join(name); std::fs::write(&dest, &bytes).unwrap();
                registry::FileEntry { dest, sha256: Some(sha.clone()), url: "generated-fixture".into(), approx_bytes: bytes.len() as u64 }
            }).collect();
            Self { root, files }
        }
    }
    impl Drop for HashFixture { fn drop(&mut self) { std::fs::remove_dir_all(&self.root).unwrap(); } }
    #[tokio::test]
    async fn hashing_waits_for_the_sole_global_cpu_and_io_without_nesting() {
        let scheduler = ResourceScheduler::new(|| (16384, 12000), 1, 1);
        let service = ResidentModelService::new(scheduler.clone());
        let held = scheduler.reserve(Demand { cpu: 1, io: 1, ..Demand::default() }, Priority::Interactive, || false).unwrap();
        let fixture = HashFixture::new();
        let files = fixture.files.clone();
        let entered = Arc::new(AtomicBool::new(false));
        let task = { let service = service.clone(); let entered = entered.clone(); tokio::spawn(async move {
            service.verify_with(Priority::Interactive, Arc::new(AtomicBool::new(false)), move |cancelled| {
                entered.store(true, Ordering::Relaxed);
                verify_files(DEFAULT_MODEL, "generated-fixture", files, cancelled)
            }).await
        }) };
        tokio::time::timeout(Duration::from_secs(2), async { while scheduler.snapshot().waiting == 0 { tokio::time::sleep(Duration::from_millis(5)).await; } }).await.unwrap();
        assert!(!entered.load(Ordering::Relaxed));
        assert_eq!(scheduler.snapshot().active, 1);
        drop(held);
        assert_eq!(tokio::time::timeout(Duration::from_secs(3), task).await.unwrap().unwrap().unwrap().artifacts.len(), 2);
        service.shutdown().await.unwrap();
        assert_eq!(scheduler.snapshot().active, 0);
    }
    async fn hashing_interrupt(mode: u8) {
        let service = ResidentModelService::new(ResourceScheduler::new(|| (16384, 12000), 1, 1));
        let fixture = HashFixture::new();
        let files = fixture.files.clone();
        let entered = Arc::new(AtomicBool::new(false));
        let resume = Arc::new(std::sync::Barrier::new(2));
        let cancel = Arc::new(AtomicBool::new(false));
        let task = { let service = service.clone(); let entered = entered.clone(); let resume = resume.clone(); let cancel = cancel.clone(); tokio::spawn(async move {
            service.verify_with(Priority::Interactive, cancel, move |cancelled| {
                let checks = std::sync::atomic::AtomicUsize::new(0);
                let check = || {
                    if checks.fetch_add(1, Ordering::Relaxed) == 2 {
                        entered.store(true, Ordering::Relaxed); resume.wait();
                    }
                    cancelled()
                };
                verify_files(DEFAULT_MODEL, "generated-fixture", files, &check)
            }).await
        }) };
        tokio::time::timeout(Duration::from_secs(2), async { while !entered.load(Ordering::Relaxed) { tokio::time::sleep(Duration::from_millis(5)).await; } }).await.unwrap();
        assert_eq!(service.scheduler.snapshot().active, 1);
        if mode == 2 {
            task.abort(); let _ = task.await;
            assert_eq!(service.scheduler.snapshot().active, 1);
            assert_eq!(service.state.lock().verifying, 1);
            resume.wait();
            tokio::time::timeout(Duration::from_secs(2), async { while service.state.lock().verifying > 0 { tokio::time::sleep(Duration::from_millis(5)).await; } }).await.unwrap();
            assert!(!service.stopped.load(Ordering::Relaxed));
            service.shutdown().await.unwrap();
        } else {
            if mode == 0 { cancel.store(true, Ordering::Relaxed); }
            let shutdown = if mode == 1 { let service = service.clone(); Some(tokio::spawn(async move { service.shutdown().await })) } else { None };
            if mode == 1 {
                while !service.stopped.load(Ordering::Relaxed) { tokio::task::yield_now().await; }
                assert!(!shutdown.as_ref().unwrap().is_finished());
            }
            resume.wait();
            assert!(task.await.unwrap().unwrap_err().to_string().contains("verification cancelled"));
            if let Some(shutdown) = shutdown { shutdown.await.unwrap().unwrap(); } else { service.shutdown().await.unwrap(); }
        }
        assert_eq!(service.scheduler.snapshot().active, 0);
    }
    #[tokio::test]
    async fn request_cancel_is_checked_between_actual_hash_chunks() { hashing_interrupt(0).await; }
    #[tokio::test]
    async fn shutdown_waits_for_blocking_hash_and_cancels_between_chunks() { hashing_interrupt(1).await; }
    #[tokio::test]
    async fn abandoned_hash_join_keeps_thread_lease_until_hash_stops() { hashing_interrupt(2).await; }

    #[tokio::test]
    async fn idle_residency_pressure_eviction_reaps_and_releases_budget() {
        let available = Arc::new(AtomicU64::new(12000));
        let probe = available.clone();
        let scheduler = ResourceScheduler::new(move || (16384, probe.load(Ordering::Relaxed)), 2, 2);
        let service = ResidentModelService::new(scheduler.clone());
        fixture(&service).await;
        assert_eq!(scheduler.snapshot().memory_mb, 1024);
        assert!(!service.evict_idle().await.unwrap());
        assert!(service.active_model().is_some());
        available.store(512, Ordering::Relaxed);
        assert!(service.evict_idle().await.unwrap());
        assert_eq!(scheduler.snapshot().memory_mb, 0);
        assert!(service.active_model().is_none());
        service.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn cancelled_and_shutdown_requests_never_load_a_model() {
        let service = ResidentModelService::new(ResourceScheduler::new(|| (16384, 12000), 1, 1));
        let cancel = Arc::new(AtomicBool::new(true));
        assert!(service.generate_text(None, "", "hello", 8, Priority::Interactive, cancel, |_| {}).await.is_err());
        assert!(service.active_model().is_none());
        assert!(service.resolve_model(None, false).await.is_err());
        assert!(service.resolve_model(Some("unknown"), true).await.is_err());
        service.shutdown().await.unwrap();
        assert!(service.load_installed(None, Priority::Interactive, Arc::new(AtomicBool::new(false))).await.is_err());
        assert_eq!(service.scheduler.snapshot().active, 0);
    }
    #[tokio::test]
    async fn abandoned_pending_request_does_not_leak_admission() {
        let service = ResidentModelService::new(ResourceScheduler::new(|| (16384, 12000), 1, 1));
        let turn = service.enter(Priority::Interactive, Arc::new(AtomicBool::new(false))).await.unwrap();
        let worker = { let service = service.clone(); tokio::spawn(async move { service.enter(Priority::Interactive, Arc::new(AtomicBool::new(false))).await }) };
        while service.queue.snapshot().waiting == 0 { tokio::task::yield_now().await; }
        worker.abort(); let _ = worker.await;
        let deadline = Instant::now() + Duration::from_secs(2);
        while service.queue.snapshot().waiting > 0 { assert!(Instant::now() < deadline); tokio::time::sleep(Duration::from_millis(25)).await; }
        drop(turn);
        service.shutdown().await.unwrap();
        assert_eq!(service.queue.snapshot().active, 0);
    }
    #[tokio::test]
    async fn release_without_idle_session_preserves_admitted_active_provenance() {
        let service = ResidentModelService::new(ResourceScheduler::new(|| (16384, 12000), 1, 1));
        fixture(&service).await;
        let turn = service.enter(Priority::Interactive, Arc::new(AtomicBool::new(false))).await.unwrap();
        let resident = service.state.lock().resident.take().unwrap();
        assert!(!service.release().await.unwrap());
        assert!(service.active_model().is_some());
        service.finish(resident, &Err(anyhow::anyhow!("cancelled"))).await.unwrap();
        drop(turn);
        service.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn request_cancellation_kills_active_backend_before_budget_release() {
        let service = ResidentModelService::new(ResourceScheduler::new(|| (16384, 12000), 1, 1));
        fixture(&service).await;
        let resident = service.state.lock().resident.take().unwrap();
        service.finish(resident, &Err(anyhow::anyhow!("cancelled"))).await.unwrap();
        assert_eq!(service.scheduler.snapshot().active, 0);
        assert!(service.active_model().is_none());
        service.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod installed_runtime_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires explicit isolated fixture state and verified installed validation models/runtime"]
    async fn installed_verified_runtime_reuses_session_streams_and_cancels() {
        assert!(std::env::var_os("FILEID_LLAMA_SERVER").is_some());
        assert!(std::env::var_os("FILEID_MODELS_DIR").is_some());
        let service = ResidentModelService::shared();
        let cancel = Arc::new(AtomicBool::new(false));
        let provenance = service.load_installed(None, Priority::Interactive, cancel.clone()).await.unwrap();
        assert_eq!(provenance.artifacts.len(), 2);
        assert!(provenance.artifacts.iter().all(|a| a.sha256.len() == 64 && a.source_url.starts_with("https://huggingface.co/")));
        let pid = service.state.lock().resident.as_ref().unwrap().server.process_id();
        let mut tokens = String::new();
        let answer = service.generate_text(None, "Answer briefly.", "Reply with exactly: ok", 8, Priority::Interactive, cancel.clone(), |chunk| tokens.push_str(chunk)).await.unwrap();
        assert!(!answer.trim().is_empty()); assert_eq!(answer, tokens);
        assert_eq!(pid, service.state.lock().resident.as_ref().unwrap().server.process_id());
        let image = std::env::temp_dir().join(format!("fileid-resident-fixture-{}.png", uuid::Uuid::new_v4()));
        image::RgbImage::from_pixel(32, 32, image::Rgb([128, 128, 128])).save(&image).unwrap();
        let mut image_tokens = String::new();
        let caption = service.caption_stream(DEFAULT_MODEL, &image, "Describe the color briefly.", 8, Priority::Interactive, cancel.clone(), |chunk| image_tokens.push_str(chunk)).await;
        assert_eq!(caption.as_ref().unwrap(), &image_tokens);
        let _ = std::fs::remove_file(image);
        assert!(!caption.unwrap().trim().is_empty());
        assert_eq!(pid, service.state.lock().resident.as_ref().unwrap().server.process_id());
        let cancelled = service.generate_text(None, "Follow the request.", "List every integer from 1 through 1000. Do not summarize, stop early, or use ellipses.", 512, Priority::Interactive, cancel.clone(), |_| { cancel.store(true, Ordering::Relaxed); }).await;
        let mut outcome = match &cancelled {
            Ok(_) => "unexpected successful cancellation result".to_owned(),
            Err(error) => format!("{error:#}"),
        };
        for artifact in &provenance.artifacts {
            outcome = outcome.replace(artifact.path.to_string_lossy().as_ref(), "<approved-validation-artifact>");
        }
        eprintln!("resident cancellation diagnostic: fixture_pid={pid}, request_cancel={}, service_stopped={}, owned_cleanup_unreaped={}, resources={:?}, outcome={outcome}",
            cancel.load(Ordering::Relaxed), service.stopped.load(Ordering::Relaxed), service.processes.has_unreaped(), service.scheduler.snapshot());
        assert!(cancelled.is_err());
        assert!(service.active_model().is_none());
        assert_eq!(service.scheduler.snapshot().memory_mb, 0);
        service.shutdown().await.unwrap();
    }
}
