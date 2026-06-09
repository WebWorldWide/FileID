// Reveal-in-Explorer.
//
// `SHOpenFolderAndSelectItems(parent_pidl, [child_pidl], 0)` opens the
// containing folder in Explorer and pre-selects the file. If the path
// doesn't exist (e.g. the file was moved between Library list and click),
// we fall back to opening the parent without selection.

use anyhow::Result;
#[cfg(windows)]
use anyhow::Context;
use std::path::Path;

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

/// Balances a successful CoInitializeEx with CoUninitialize on drop (covers
/// the early-return paths). Only uninitializes when we actually performed the
/// init (S_OK / S_FALSE) — never on RPC_E_CHANGED_MODE, where another caller
/// owns the apartment.
#[cfg(windows)]
struct ComGuard(bool);
#[cfg(windows)]
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems,
};

#[cfg(windows)]
#[allow(dead_code)]
pub fn reveal(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        // STA apartment is required; reveal is called from app dispatch
        // threads so this is usually a no-op. RPC_E_CHANGED_MODE is fine.
        // Balance a successful init with CoUninitialize on every exit path.
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _com = ComGuard(hr.is_ok());

        let pidl = ILCreateFromPathW(PCWSTR(wide.as_ptr()));
        if pidl.is_null() {
            anyhow::bail!("ILCreateFromPathW returned null for {}", path.display());
        }

        let result = SHOpenFolderAndSelectItems(pidl, None, 0);
        ILFree(Some(pidl));
        result.context("SHOpenFolderAndSelectItems failed")?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn reveal(_path: &Path) -> Result<()> {
    anyhow::bail!("reveal not available on this platform")
}
