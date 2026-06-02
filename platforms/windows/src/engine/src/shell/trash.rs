// Trash — IFileOperation::DeleteItem with FOF_ALLOWUNDO.
//
// Each delete goes into the user's Recycle Bin so the action is reversible
// from Explorer's "Restore" in case the user changes their mind.
//
// Concurrency: each worker thread owns its own STA COM apartment (per
// the IFileOperation contract). The pool is built by the caller; we
// just expose the per-call API. The Cleanup tab uses an 8-parallel pool.

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
    use std::os::windows::ffi::OsStrExt;

    // Verbatim (\\?\) probe: a bare `path.exists()` misses >260-char paths the
    // verbatim discovery walk indexed, silently no-opping the delete (#28).
    let probe = crate::util::path_safety::to_extended_length(path);
    if std::fs::symlink_metadata(&probe).is_err() {
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
/// 8-parallel STA worker pool: each OS thread initializes COM apartment-
/// threaded once + stays in the pool for the batch's lifetime, amortizing
/// the ~1-2 ms CoInitialize cost across N files. Matches macOS's 8-way
/// async trash pattern. Order of `paths` is preserved in the result.
pub fn trash(paths: &[std::path::PathBuf]) -> Vec<bool> {
    if paths.is_empty() {
        return Vec::new();
    }
    if paths.len() <= 4 {
        // Tiny batches: sequential is faster than spinning up workers.
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
            // One CoInitializeEx per worker; held for the whole batch.
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
    drop(output_tx); // workers hold the only senders now

    for (i, p) in paths.iter().enumerate() {
        // unbounded relative to capacity since channel is sized to N.
        let _ = input_tx.send((i, p.clone()));
    }
    drop(input_tx); // workers will see channel close + exit

    let mut result = vec![false; n];
    while let Ok((idx, ok)) = output_rx.recv() {
        if idx < n {
            result[idx] = ok;
        }
    }
    result
}
