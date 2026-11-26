// Trash via IFileOperation::DeleteItem with FOF_ALLOWUNDO: deletes go to the Recycle Bin so Explorer's "Restore" can undo them.
// Each worker thread owns its own STA COM apartment, as IFileOperation requires.

use anyhow::Result;
#[cfg(windows)]
use anyhow::Context;
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
    trash_path_checked(path, None)
}

#[cfg(windows)]
fn trash_path_checked(
    path: &Path,
    expected: Option<crate::platform::FileIdentity>,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    crate::util::read_only::require_source_mutation(path)?;

    // A bare `path.exists()` misses paths over 260 characters, which would silently skip the delete; probe the verbatim form.
    let probe = crate::util::path_safety::to_extended_length(path);
    if std::fs::symlink_metadata(&probe).is_err() {
        if expected.is_some() {
            anyhow::bail!("claimed Trash source disappeared before the Recycle Bin operation");
        }
        return Ok(());
    }
    if let Some(expected) = expected && crate::platform::file_identity(path) != Some(expected) {
        anyhow::bail!("claimed Trash source identity changed before the Recycle Bin operation");
    }

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
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
            if let Some(expected) = expected && crate::platform::file_identity(path) != Some(expected) {
                anyhow::bail!("claimed Trash source identity changed while binding the Recycle Bin item");
            }
            op.DeleteItem(&item, None).context("DeleteItem queue")?;
            if let Some(expected) = expected && crate::platform::file_identity(path) != Some(expected) {
                anyhow::bail!("claimed Trash source identity changed before Recycle Bin execution");
            }
            op.PerformOperations().context("PerformOperations")?;
            if op
                .GetAnyOperationsAborted()
                .context("GetAnyOperationsAborted")?
                .as_bool()
            {
                anyhow::bail!("Recycle Bin operation was aborted");
            }
            if std::fs::symlink_metadata(&probe).is_ok() {
                anyhow::bail!("Recycle Bin operation completed without removing the claimed file");
            }
            Ok(())
        })();

        if must_uninit {
            CoUninitialize();
        }
        // The shell items do not reliably accept paths over 260 characters even in verbatim form; report that instead of a cryptic HRESULT.
        if let Err(e) = result {
            let path_len = wide.len().saturating_sub(1);
            if path_len > 260 {
                anyhow::bail!(
                    "Path is too long for the Recycle Bin ({path_len} chars > 260); Windows shell \
                     delete can't handle it without long-path support. Shorten the path or remove \
                     the file manually. (underlying error: {e})"
                );
            }
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn trash_path(_path: &Path) -> Result<()> {
    anyhow::bail!("trash not available on this platform")
}

pub(crate) fn trash_path_as(
    source: &Path,
    _original_path: &Path,
    expected: crate::platform::FileIdentity,
) -> Result<()> {
    #[cfg(windows)]
    {
        trash_path_checked(source, Some(expected))
    }
    #[cfg(not(windows))]
    {
        let _ = expected;
        trash_path(source)
    }
}

/// Trashes each path and returns one bool per input (true = success), in order. A pool of STA workers initialises COM once per thread.
pub fn trash(paths: &[std::path::PathBuf]) -> Vec<bool> {
    if paths.is_empty() {
        return Vec::new();
    }
    if paths.len() <= 4 {
        return paths.iter().map(|p| trash_path(p).is_ok()).collect();
    }

    const POOL_SIZE: usize = 8;
    let n = paths.len();
    let workers = POOL_SIZE.min(n);

    let (input_tx, input_rx) = crossbeam_channel::bounded::<(usize, std::path::PathBuf)>(n);
    let (output_tx, output_rx) = crossbeam_channel::bounded::<(usize, bool)>(n);

    for _ in 0..workers {
        let rx = input_rx.clone();
        let tx = output_tx.clone();
        std::thread::spawn(move || {
            #[cfg(windows)]
            let init_ok = unsafe {
                windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                )
                .is_ok()
            };
            while let Ok((idx, path)) = rx.recv() {
                let ok = trash_path(&path).is_ok();
                let _ = tx.send((idx, ok));
            }
            #[cfg(windows)]
            if init_ok {
                unsafe { windows::Win32::System::Com::CoUninitialize() };
            }
        });
    }
    drop(output_tx);

    for (i, p) in paths.iter().enumerate() {
        let _ = input_tx.send((i, p.clone()));
    }
    drop(input_tx);

    let mut result = vec![false; n];
    while let Ok((idx, ok)) = output_rx.recv() {
        if idx < n {
            result[idx] = ok;
        }
    }
    result
}
