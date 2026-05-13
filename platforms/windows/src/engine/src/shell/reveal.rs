// Reveal-in-Explorer — analog of NSWorkspace.activateFileViewerSelecting.
//
// `SHOpenFolderAndSelectItems(parent_pidl, [child_pidl], 0)` opens the
// containing folder in Explorer and pre-selects the file. If the path
// doesn't exist (e.g. the file was moved between Library list and click),
// we fall back to opening the parent without selection.

use anyhow::{Context, Result};
use std::path::Path;

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems,
};

#[cfg(windows)]
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
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

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
