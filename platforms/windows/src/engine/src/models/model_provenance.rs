use std::{io::Read, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use crate::resource_scheduler::{Demand, ResourceScheduler};
use super::{registry, native_admission::{reserve_host, HostAdmissionError}};

#[derive(Debug)]
struct VerifiedArtifact { path: PathBuf, source: PathBuf, snapshot: std::fs::Metadata, sha256: String }
#[derive(Debug)]
pub(crate) struct VerifiedModel { model_kind: String, registry_id: String, artifacts: [VerifiedArtifact; 2] }
impl VerifiedModel {
    pub(crate) fn paths(&self) -> (&Path, &Path) { (&self.artifacts[0].path, &self.artifacts[1].path) }
    pub(crate) fn matches_kind(&self, kind: &str) -> bool {
        let stored = registry::vlm_dir_name(&self.model_kind);
        stored.is_some() && stored == registry::vlm_dir_name(kind)
    }
    pub(crate) async fn recheck(self: &Arc<Self>, cancel: Arc<AtomicBool>) -> Result<()> {
        let resources = reserve_host(ResourceScheduler::shared(), Demand { memory_mb: 16, cpu: 1, io: 1 }, cancel.clone()).await?;
        let model = self.clone();
        let abandoned = Arc::new(AtomicBool::new(false)); let _guard = AbandonedVerification(abandoned.clone());
        tokio::task::spawn_blocking(move || {
            let _resources = resources;
            for artifact in &model.artifacts {
                if cancel.load(Ordering::Relaxed) || abandoned.load(Ordering::Relaxed) { bail!("Model identity check cancelled"); }
                let current = std::fs::symlink_metadata(&artifact.source).context("stat verified model source")?;
                if !same_snapshot(&artifact.snapshot, &current)? || std::fs::canonicalize(&artifact.source)? != artifact.path { bail!("Verified model identity changed; start a new job to reverify"); }
            }
            Ok::<_, anyhow::Error>(())
        }).await.map_err(|error| HostAdmissionError(anyhow!(error)))?.map_err(|error| HostAdmissionError(error).into())
    }
    pub(crate) fn trace_verified_bytes(&self) {
        tracing::info!(model = %self.model_kind, registry = %self.registry_id,
            weights_sha256 = %self.artifacts[0].sha256, projector_sha256 = %self.artifacts[1].sha256,
            "Installed model bytes verified; no loader/provider acceptance implied");
    }
}
struct AbandonedVerification(Arc<AtomicBool>);
impl Drop for AbandonedVerification { fn drop(&mut self) { self.0.store(true, Ordering::Relaxed); } }

pub(crate) async fn verify_installed(model_kind: &str, cancel: Arc<AtomicBool>) -> Result<VerifiedModel> {
    if registry::vlm_dir_name(model_kind).is_none() { return Err(HostAdmissionError(anyhow!("Unknown caption/text model")).into()); }
    let registry::LookupResult::Found(model) = registry::lookup_full(model_kind) else {
        return Err(HostAdmissionError(anyhow!("Unknown installed caption/text model")).into());
    };
    let kind = model_kind.to_owned();
    run_verification(ResourceScheduler::shared(), cancel, move |cancelled| verify_files(&kind, model.id, model.files, cancelled)).await
}
async fn run_verification(
    scheduler: Arc<ResourceScheduler>, cancel: Arc<AtomicBool>,
    job: impl FnOnce(&dyn Fn() -> bool) -> Result<VerifiedModel> + Send + 'static,
) -> Result<VerifiedModel> {
    let resources = reserve_host(scheduler, Demand { memory_mb: 16, cpu: 1, io: 1 }, cancel.clone()).await?;
    let abandoned = Arc::new(AtomicBool::new(false)); let _guard = AbandonedVerification(abandoned.clone());
    tokio::task::spawn_blocking(move || {
        let _resources = resources;
        let cancelled = || cancel.load(Ordering::Relaxed) || abandoned.load(Ordering::Relaxed);
        if cancelled() { bail!("Model verification cancelled"); }
        let result = job(&cancelled);
        if cancelled() { bail!("Model verification cancelled"); }
        result
    }).await.map_err(|error| HostAdmissionError(anyhow!(error)))?.map_err(|error| HostAdmissionError(error).into())
}
fn same_snapshot(a: &std::fs::Metadata, b: &std::fs::Metadata) -> Result<bool> {
    let same = a.is_file() && b.is_file() && a.len() == b.len() && a.modified()? == b.modified()?;
    #[cfg(unix)]
    let same = { use std::os::unix::fs::MetadataExt; same && a.dev() == b.dev() && a.ino() == b.ino() };
    Ok(same)
}
fn verify_file(file: registry::FileEntry, cancelled: &dyn Fn() -> bool) -> Result<VerifiedArtifact> {
    let expected = file.sha256.context("Installed model lacks a SHA256 pin")?;
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { bail!("Installed model SHA256 pin is malformed"); }
    if !file.dest.is_absolute() { bail!("Installed model path must be absolute"); }
    if cancelled() { bail!("Model verification cancelled"); }
    let metadata = std::fs::symlink_metadata(&file.dest).context("Installed model is missing; no download was started")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 * 1024 * 1024 { bail!("Model weights must be bounded nonempty regular files, not symlinks"); }
    let path = std::fs::canonicalize(&file.dest).context("resolve installed model")?;
    let mut source = std::fs::File::open(&path).context("open installed model")?;
    let opened = source.metadata()?;
    if !same_snapshot(&metadata, &opened)? { bail!("Model weights changed before verification"); }
    let mut hasher = Sha256::new(); let mut buffer = vec![0; 1024 * 1024]; let mut read_bytes = 0_u64;
    loop {
        if cancelled() { bail!("Model verification cancelled"); }
        let count = source.read(&mut buffer).context("read installed model")?;
        if count == 0 { break; }
        read_bytes = read_bytes.checked_add(count as u64).context("Model byte count overflow")?;
        if read_bytes > metadata.len() { bail!("Model weights grew during verification"); }
        hasher.update(&buffer[..count]);
    }
    if cancelled() { bail!("Model verification cancelled"); }
    if read_bytes != metadata.len() || hex::encode(hasher.finalize()) != expected { bail!("Installed model failed pinned SHA256 verification"); }
    if !same_snapshot(&opened, &source.metadata()?)? || !same_snapshot(&opened, &std::fs::metadata(&file.dest)?)? { bail!("Model weights changed during verification"); }
    Ok(VerifiedArtifact { path, source: file.dest, snapshot: opened, sha256: expected })
}
pub(crate) fn verify_files(model_kind: &str, registry_id: &str, files: Vec<registry::FileEntry>, cancelled: &dyn Fn() -> bool) -> Result<VerifiedModel> {
    if files.len() != 2 { bail!("Caption model requires pinned weights and projector"); }
    let mut files = files.into_iter();
    let artifacts = [verify_file(files.next().unwrap(), cancelled)?, verify_file(files.next().unwrap(), cancelled)?];
    if artifacts[0].path == artifacts[1].path { bail!("Model weights and projector must be distinct files"); }
    Ok(VerifiedModel { model_kind: model_kind.into(), registry_id: registry_id.into(), artifacts })
}

#[cfg(test)]
#[path = "model_provenance_tests.rs"]
mod tests;
