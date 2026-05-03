// Trash — IFileOperation::DeleteItem with FOF_ALLOWUNDO.
//
// Mirror of macOS `FileManager.trashItem`. Each delete goes into the
// user's Recycle Bin so the action is reversible from Explorer's "Restore"
// in case the user changes their mind.
//
// Concurrency: each worker thread owns its own STA COM apartment (per
// the IFileOperation contract). The pool is built by the caller; we
// just expose the per-call API. macOS uses an 8-parallel trash pattern
// and we match it from the Cleanup tab.
//
// Phase 2.3 cut: implements the single-file path. Phase 4 (Cleanup tab)
// builds the parallel pool that consumes this.

use anyhow::{Context, Result};
use std::path::Path;

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName, FOF_ALLOWUNDO,
    FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
};

/// Move a single file to the Recycle Bin. Idempotent: a missing source
/// is treated as success (the file is "already not on disk").
#[cfg(windows)]
pub fn trash_path(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    if !path.exists() {
        return Ok(());
    }

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        // Each call enters and leaves the apartment. The Cleanup pool will
        // amortize this with one apartment per worker; for ad-hoc one-off
        // deletes the per-call cost (~ms) is fine.
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let must_uninit = hr.is_ok();

        let result = (|| -> Result<()> {
            let op: IFileOperation =
                CoCreateInstance(&FileOperation, None, CLSCTX_ALL).context("CoCreateInstance(IFileOperation)")?;
            op.SetOperationFlags(FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI)
                .context("SetOperationFlags")?;

            let item: IShellItem =
                SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)
                    .context("SHCreateItemFromParsingName")?;
            op.DeleteItem(&item, None).context("DeleteItem queue")?;
            op.PerformOperations().context("PerformOperations")?;
            Ok(())
        })();

        if must_uninit {
            CoUninitialize();
        }
        result?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn trash_path(_path: &Path) -> Result<()> {
    anyhow::bail!("trash not available on this platform")
}

/// Batch wrapper. Trashes each path; returns one bool per input, true = success.
/// Sequential; the per-file COM cost is small enough that batching helps minimally
/// at our scale. Phase 4 polish can add an 8-parallel STA worker pool.
pub fn trash(paths: &[std::path::PathBuf]) -> Vec<bool> {
    paths
        .iter()
        .map(|p| trash_path(p).is_ok())
        .collect()
}
