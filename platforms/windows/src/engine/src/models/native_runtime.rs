use std::{io::Read, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use anyhow::{anyhow, bail, Context, Result};
use crate::resource_scheduler::{Demand, ResourceScheduler};
use super::native_admission::{reserve_host, HostAdmissionError};

struct AbandonedValidation(Arc<AtomicBool>);
impl Drop for AbandonedValidation { fn drop(&mut self) { self.0.store(true, Ordering::Relaxed); } }

pub(crate) fn validate_binary(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() { bail!("Installed runtime path must be absolute"); }
    let path = std::fs::canonicalize(path).context("resolve installed native runtime")?;
    let meta = std::fs::metadata(&path).context("stat installed native runtime")?;
    if !meta.is_file() || !(20_000..=200_000_000).contains(&meta.len()) { bail!("Native runtime has invalid file size/type"); }
    let mut file = std::fs::File::open(&path)?; let mut header = [0; 64]; file.read_exact(&mut header)?;
    #[cfg(windows)] {
        use std::io::{Seek, SeekFrom};
        if &header[..2] != b"MZ" { bail!("Native runtime is not Windows PE"); }
        let offset = u32::from_le_bytes(header[60..64].try_into().unwrap()) as u64;
        if offset < 64 || offset.saturating_add(6) > meta.len() { bail!("Native runtime PE offset is invalid"); }
        file.seek(SeekFrom::Start(offset))?; let mut pe = [0; 6]; file.read_exact(&mut pe)?;
        let machine = if cfg!(target_arch = "aarch64") { 0xaa64 } else { 0x8664 };
        if &pe[..4] != b"PE\0\0" || u16::from_le_bytes([pe[4], pe[5]]) != machine { bail!("Native runtime PE architecture is incompatible"); }
    }
    #[cfg(target_os = "linux")] {
        let machine = if cfg!(target_arch = "aarch64") { 183 } else { 62 };
        if &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 || u16::from_le_bytes([header[18], header[19]]) != machine { bail!("Native runtime is not host-compatible 64-bit ELF"); }
    }
    #[cfg(target_os = "macos")] {
        let cpu = if cfg!(target_arch = "aarch64") { 0x0100000c } else { 0x01000007 };
        if header[..4] != [0xcf, 0xfa, 0xed, 0xfe] || u32::from_le_bytes(header[4..8].try_into().unwrap()) != cpu { bail!("Native runtime is not host-compatible thin64 Mach-O"); }
    }
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 { bail!("Native runtime is not executable"); }
    }
    Ok(path)
}
pub(crate) fn discover_at(root: &Path, application_dir: Option<&Path>, name: &str, configured: Option<PathBuf>) -> Result<Vec<PathBuf>> {
    if let Some(configured) = configured { return validate_binary(&configured).map(|p| vec![p]); }
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let mut dirs = vec![root.join("llama.cpp-cuda"), root.join("llama.cpp")];
    if let Some(application_dir) = application_dir { dirs.push(application_dir.join("RuntimeBundles/llama.cpp")); }
    let mut result = Vec::new();
    for dir in dirs {
        for path in [dir.join(format!("{name}{suffix}")), dir.join("bin").join(format!("{name}{suffix}"))] {
            if let Ok(path) = validate_binary(&path) { if !result.contains(&path) { result.push(path); } }
        }
    }
    Ok(result)
}
pub(crate) async fn candidates(name: &'static str, setting: &'static str, cancel: Arc<AtomicBool>) -> Result<Vec<PathBuf>> {
    let root = crate::paths::models_dir().map_err(HostAdmissionError)?;
    let application = std::env::current_exe().map_err(|error| HostAdmissionError(error.into()))?.parent().map(Path::to_owned);
    let configured = std::env::var_os(setting).map(PathBuf::from);
    let resources = reserve_host(ResourceScheduler::shared(), Demand { memory_mb: 16, cpu: 1, io: 1 }, cancel.clone()).await?;
    let abandoned = Arc::new(AtomicBool::new(false));
    let _guard = AbandonedValidation(abandoned.clone());
    tokio::task::spawn_blocking(move || {
        let _resources = resources;
        let cancelled = || cancel.load(Ordering::Relaxed) || abandoned.load(Ordering::Relaxed);
        if cancelled() { bail!("Native runtime validation cancelled"); }
        let result = discover_at(&root, application.as_deref(), name, configured);
        if cancelled() { bail!("Native runtime validation cancelled"); }
        result
    }).await.map_err(|error| HostAdmissionError(anyhow!(error)))?.map_err(|error| HostAdmissionError(error.context(setting)).into())
}
