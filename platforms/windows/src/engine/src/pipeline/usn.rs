//! NTFS USN journal primitives (Phase 3 foundation; scan-path integration is
//! a follow-up).
//!
//! When the engine runs elevated on an NTFS volume, the journal lets a
//! repeat scan turn "walk every file and stat it" into "read the change list
//! since the last cursor" — a perf optimization for 1M+-file corpora. This
//! module exposes the minimum primitives needed to wire that integration:
//!
//! - [`is_elevated`](super::super::util::elevation::is_elevated) gate (in
//!   `util::elevation`).
//! - [`query_journal`] — `FSCTL_QUERY_USN_JOURNAL`, the lookup that gives us
//!   the current journal id + the next-USN cursor.
//!
//! The v9 `usn_state` migration provisions the cursor table. The actual
//! incremental record reader (`FSCTL_READ_USN_JOURNAL`), rename-pair
//! correlation, and scan-skip-set integration land in a follow-up so the
//! current `jwalk` + timestamp-skip path remains the verified default.
#![allow(dead_code)]

use std::path::Path;

use anyhow::Result;
#[cfg(windows)]
use anyhow::Context;

/// Summary returned by `FSCTL_QUERY_USN_JOURNAL`. `journal_id` is stable
/// across the journal's lifetime; if it changes (the journal was deleted and
/// recreated) any persisted cursor is invalid and the volume should be
/// re-enumerated.
#[derive(Debug, Clone, Copy)]
pub(crate) struct JournalInfo {
    pub journal_id: u64,
    /// Lowest USN still readable from the journal (older records have rolled
    /// off the front).
    pub first_usn: i64,
    /// Cursor to use as the start for the next incremental read.
    pub next_usn: i64,
    /// Allocation cap in bytes. When the journal grows past this, Windows
    /// rolls older records off the front.
    pub max_size: u64,
}

/// Query the USN journal on `volume_root`'s volume (only the drive letter is
/// used). Returns `Err` if the volume isn't NTFS, the process isn't elevated,
/// or the journal isn't active — callers treat any `Err` as "fall back to
/// the jwalk walk".
#[cfg(windows)]
pub(crate) fn query_journal(volume_root: &Path) -> Result<JournalInfo> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{FSCTL_QUERY_USN_JOURNAL, USN_JOURNAL_DATA_V0};
    use windows::Win32::System::IO::DeviceIoControl;

    // Walk up to the volume root (e.g. `C:\` from a deep path) and trim the
    // trailing separator; FSCTL targets the volume as `\\.\X:`.
    let drive = volume_root
        .ancestors()
        .last()
        .unwrap_or(volume_root)
        .to_string_lossy();
    let trimmed = drive.trim_end_matches(['\\', '/']);
    let unc = format!(r"\\.\{trimmed}");
    let mut wide: Vec<u16> = unc.encode_utf16().collect();
    wide.push(0);

    // Zero desired access + FILE_FLAG_BACKUP_SEMANTICS is the minimum the
    // FSCTL accepts; on system volumes it still requires Administrator (the
    // `is_elevated` gate is the caller's responsibility).
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .context("opening volume for USN journal query")?
    };

    let mut data = USN_JOURNAL_DATA_V0::default();
    let mut returned: u32 = 0;
    let result = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            None,
            0,
            Some((&mut data as *mut USN_JOURNAL_DATA_V0).cast()),
            u32::try_from(std::mem::size_of::<USN_JOURNAL_DATA_V0>()).unwrap_or(0),
            Some(&mut returned),
            None,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    result.context("FSCTL_QUERY_USN_JOURNAL")?;

    Ok(JournalInfo {
        journal_id: data.UsnJournalID,
        first_usn: data.FirstUsn,
        next_usn: data.NextUsn,
        max_size: data.MaximumSize,
    })
}

#[cfg(not(windows))]
pub(crate) fn query_journal(_volume_root: &Path) -> Result<JournalInfo> {
    anyhow::bail!("USN journal is only available on Windows / NTFS")
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// On non-elevated test runs (CI/dev default) this is expected to return
    /// an Err with ACCESS_DENIED, not panic. Either Ok (elevated) or Err
    /// (standard user) is valid here; we're asserting the call is a clean
    /// primitive callers can treat as fallback-or-use.
    #[test]
    #[cfg(windows)]
    fn query_journal_returns_a_result_without_panicking() {
        let _ = query_journal(std::path::Path::new(r"C:\"));
    }
}
