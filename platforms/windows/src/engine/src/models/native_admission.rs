use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use anyhow::Result;
use crate::resource_scheduler::{AcceleratorDemand, Demand, Lease, Priority, ResourceScheduler};
use super::native_process::OwnedProcess;

#[derive(Debug)]
pub(crate) struct HostAdmissionError(pub(crate) anyhow::Error);
impl std::fmt::Display for HostAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "Native host admission: {}", self.0) }
}
impl std::error::Error for HostAdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(self.0.as_ref()) }
}

pub(crate) fn startup_allows_cli_fallback(error: &anyhow::Error, tracker: &super::native_process::ProcessTracker) -> bool {
    error.downcast_ref::<HostAdmissionError>().is_none() && tracker.allows_fallback()
}

pub(crate) fn probe_requests_fallback(result: Result<()>) -> Result<bool> {
    match result {
        Ok(()) => Ok(false),
        Err(error) if error.downcast_ref::<HostAdmissionError>().is_some() => Err(error),
        Err(_) => Ok(true),
    }
}

struct PendingAdmission(Arc<AtomicBool>);
impl Drop for PendingAdmission {
    fn drop(&mut self) { self.0.store(true, Ordering::Relaxed); }
}
pub(crate) async fn reserve_host(
    scheduler: Arc<ResourceScheduler>, demand: Demand, cancel: Arc<AtomicBool>,
) -> Result<Arc<Lease>> {
    reserve_host_when(scheduler, demand, move || cancel.load(Ordering::Relaxed)).await
}
pub(crate) async fn reserve_host_when(
    scheduler: Arc<ResourceScheduler>, demand: Demand, cancelled: impl Fn() -> bool + Send + 'static,
) -> Result<Arc<Lease>> {
    let abandoned = Arc::new(AtomicBool::new(false));
    let _guard = PendingAdmission(abandoned.clone());
    tokio::task::spawn_blocking(move || {
        scheduler.reserve_queued(demand, AcceleratorDemand::default(), Priority::Interactive,
            || cancelled() || abandoned.load(Ordering::Relaxed)).map(Arc::new)
    }).await?.map_err(|error| HostAdmissionError(error).into())
}

pub(crate) struct RequestWork<'a> {
    process: &'a OwnedProcess,
    host: Arc<Lease>,
    dispatched: bool,
    completed: bool,
}
impl<'a> RequestWork<'a> {
    pub(crate) fn new(process: &'a OwnedProcess, lease: Arc<Lease>) -> Result<Self> {
        process.retain_work(lease.clone())?;
        Ok(Self { process, host: lease, dispatched: false, completed: false })
    }
    pub(crate) fn host_lease(&self) -> Arc<Lease> { self.host.clone() }
    pub(crate) fn native_incomplete(&self) -> bool { self.dispatched && !self.completed }
    pub(crate) fn begin_dispatch(&mut self) { self.dispatched = true; }
    pub(crate) fn complete(&mut self) { self.completed = true; }
}
impl Drop for RequestWork<'_> {
    fn drop(&mut self) {
        if !self.dispatched || self.completed { self.process.release_work(); }
        else { self.process.request_stop(); }
    }
}
