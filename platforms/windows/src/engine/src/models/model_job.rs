use std::{path::Path, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use anyhow::{anyhow, Result};
use super::{model_provenance::{self, VerifiedModel}, native_admission::HostAdmissionError};

#[derive(Debug, Default)]
enum State { #[default] Empty, Ready(Arc<VerifiedModel>), Refused }
#[derive(Debug, Default)]
pub(crate) struct ModelJob { state: tokio::sync::Mutex<State> }
impl ModelJob {
    pub(crate) async fn resolve(&self, kind: &str, cancel: Arc<AtomicBool>) -> Result<Arc<VerifiedModel>> {
        self.resolve_with(kind, cancel.clone(), || model_provenance::verify_installed(kind, cancel)).await
    }
    async fn lock(&self, cancel: &AtomicBool) -> Result<tokio::sync::MutexGuard<'_, State>> {
        tokio::select! { biased;
            _ = wait_cancelled(cancel) => Err(HostAdmissionError(anyhow!("Model job cancelled")).into()),
            guard = self.state.lock() => Ok(guard),
        }
    }
    async fn resolve_with<F: std::future::Future<Output = Result<VerifiedModel>>>(
        &self, kind: &str, cancel: Arc<AtomicBool>, verify: impl FnOnce() -> F,
    ) -> Result<Arc<VerifiedModel>> {
        let mut state = self.lock(&cancel).await?;
        let model = match &*state {
            State::Refused => return Err(HostAdmissionError(anyhow!("Model job identity refused; create a new job")).into()),
            State::Ready(model) => {
                if !model.matches_kind(kind) { return Err(HostAdmissionError(anyhow!("Model job cannot switch model")).into()); }
                model.clone()
            }
            State::Empty => {
                let model = match verify().await {
                    Ok(model) => Arc::new(model),
                    Err(error) => { *state = State::Refused; return Err(error); }
                };
                if !model.matches_kind(kind) { *state = State::Refused; return Err(HostAdmissionError(anyhow!("Verified model does not match job")).into()); }
                model.trace_verified_bytes(); *state = State::Ready(model.clone()); model
            }
        };
        if let Err(error) = model.recheck(cancel).await { *state = State::Refused; return Err(error); }
        Ok(model)
    }
    pub(crate) async fn require_pair(&self, gguf: &Path, mmproj: &Path, cancel: Arc<AtomicBool>) -> Result<Arc<VerifiedModel>> {
        let mut state = self.lock(&cancel).await?;
        let State::Ready(model) = &*state else { return Err(HostAdmissionError(anyhow!("Caption requires a verified model job")).into()); };
        let model = model.clone(); let paths = model.paths();
        if gguf != paths.0 || mmproj != paths.1 { return Err(HostAdmissionError(anyhow!("Caption paths differ from verified model job")).into()); }
        if let Err(error) = model.recheck(cancel).await { *state = State::Refused; return Err(error); }
        Ok(model)
    }
}
async fn wait_cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) { tokio::time::sleep(std::time::Duration::from_millis(25)).await; }
}

#[cfg(test)]
#[path = "model_job_tests.rs"]
mod tests;
