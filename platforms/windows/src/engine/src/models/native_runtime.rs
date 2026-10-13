use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub fn candidates(name: &str, configured: &str) -> Result<Vec<PathBuf>> {
    if let Some(path) = std::env::var_os(configured) {
        let path = PathBuf::from(path);
        if !path.is_absolute() { bail!("{configured} must be an absolute installed runtime path"); }
        validate_binary(&path)?;
        return Ok(vec![path]);
    }
    let root = crate::paths::models_dir()?;
    let mut dirs = vec![root.join("llama.cpp-cuda"), root.join("llama.cpp")];
    if let Some(parent) = std::env::current_exe()?.parent() {
        dirs.push(parent.join("RuntimeBundles/llama.cpp"));
    }
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    Ok(dirs.into_iter().flat_map(|dir| [dir.join(format!("{name}{suffix}")), dir.join("bin").join(format!("{name}{suffix}"))])
        .filter(|path| validate_binary(path).is_ok()).collect())
}

pub fn validate_binary(path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path).context("stat installed native runtime")?;
    if !meta.is_file() || !(20_000..=500_000_000).contains(&meta.len()) {
        bail!("native runtime has invalid file size");
    }
    let mut header = [0; 64];
    let mut file = std::fs::File::open(path)?;
    file.read_exact(&mut header)?;
    validate_header(&header)?;
    #[cfg(windows)]
    {
        use std::io::{Seek, SeekFrom};
        let offset = u32::from_le_bytes(header[60..64].try_into().unwrap()) as u64;
        if offset < 64 || offset.saturating_add(6) > meta.len() { bail!("native runtime has invalid PE offset"); }
        file.seek(SeekFrom::Start(offset))?;
        let mut pe = [0; 6]; file.read_exact(&mut pe)?;
        let machine = if cfg!(target_arch = "aarch64") { 0xaa64 } else { 0x8664 };
        if &pe[..4] != b"PE\0\0" || u16::from_le_bytes([pe[4], pe[5]]) != machine {
            bail!("native runtime is not a compatible Windows PE executable");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 { bail!("native runtime is not executable"); }
    }
    Ok(())
}
fn validate_header(header: &[u8; 64]) -> Result<()> {
    #[cfg(windows)]
    if &header[..2] != b"MZ" { bail!("native runtime is not a Windows PE executable"); }
    #[cfg(target_os = "linux")]
    {
        let machine = if cfg!(target_arch = "aarch64") { 183 } else { 62 };
        if &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 || u16::from_le_bytes([header[18], header[19]]) != machine {
            bail!("native runtime is not a compatible 64-bit Linux ELF executable");
        }
    }
    Ok(())
}

pub fn probe_version(path: &Path) -> Result<()> {
    validate_binary(path)?;
    let mut command = std::process::Command::new(path);
    command.arg("--version").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    super::native_process::configure_sync(&mut command);
    let mut child = command.spawn().context("probe native runtime version")?;
    #[cfg(windows)]
    let _ownership = match super::native_process::ParentOwnership::new_sync(&child) {
        Ok(ownership) => ownership,
        Err(error) => { let _ = child.kill(); let _ = child.wait(); return Err(error); }
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() { return Ok(()); }
            bail!("installed runtime version probe failed: {status}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("installed runtime version probe timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_foreign_binary_header() {
        assert!(validate_header(&[0; 64]).is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn accepts_only_host_elf_machine() {
        let mut header = [0; 64];
        header[..4].copy_from_slice(b"\x7fELF"); header[4] = 2; header[5] = 1;
        let machine: u16 = if cfg!(target_arch = "aarch64") { 183 } else { 62 };
        header[18..20].copy_from_slice(&machine.to_le_bytes());
        assert!(validate_header(&header).is_ok());
        header[18..20].copy_from_slice(&3u16.to_le_bytes());
        assert!(validate_header(&header).is_err());
    }
}
