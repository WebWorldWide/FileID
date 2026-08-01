//! Trash-related IPC handlers: `restoreFromTrash` (with path-containment
//! check against authorized scan roots, SEC-7) and `revertMerge` (split a
//! merged person cluster back into source + destination).

use anyhow::Context;

use crate::ipc::{self, sink::Sink, BulkActionItem, BulkActionResult};

use super::bulk::emit_bulk_result;
use super::trash_log;

/// Per-target restore decision, made BEFORE touching the Recycle Bin.
/// Keeps the C1-003 conflict rule and the SEC-7 containment rule in one
/// pure, unit-testable place.
#[derive(Debug, PartialEq, Eq)]
enum RestoreDisposition {
    /// Inside an authorized root and the destination is free — attempt restore.
    Restore,
    /// Outside every authorized library root (SEC-7).
    Refused,
    /// Destination already occupied by another file (C1-003) — restoring would
    /// clobber it / the bin's Undelete is a no-op, so report a conflict rather
    /// than a false success.
    Conflict,
}

fn restore_disposition(allowed: bool, occupied: bool) -> RestoreDisposition {
    if !allowed {
        RestoreDisposition::Refused
    } else if occupied {
        RestoreDisposition::Conflict
    } else {
        RestoreDisposition::Restore
    }
}

#[derive(Debug)]
enum RestoreOutcome {
    Restored(crate::platform::FileIdentity, Option<std::path::PathBuf>),
    Conflict,
    Refused(String),
    Failed(String),
}

#[cfg(windows)]
fn windows_io_error(error: windows::core::Error) -> std::io::Error {
    let code = error.code().0 as u32;
    if code & 0xffff_0000 == 0x8007_0000 {
        std::io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        std::io::Error::other(error)
    }
}

#[cfg(windows)]
struct WindowsRestoreSentinel {
    file: Option<std::fs::File>,
    path: std::path::PathBuf,
}

#[cfg(windows)]
impl Drop for WindowsRestoreSentinel {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
struct PlatformRestoreTarget {
    leaf: Vec<u16>,
    parent: std::fs::File,
    _sentinel: WindowsRestoreSentinel,
}

#[cfg(windows)]
impl PlatformRestoreTarget {
    fn prepare(
        original: &std::path::Path,
        authorized_roots: &[std::path::PathBuf],
    ) -> anyhow::Result<Self> {
        let parent = original.parent().context("restore destination has no parent")?;
        if original.symlink_metadata().is_ok() {
            anyhow::bail!("restore destination is already occupied");
        }
        let sentinel_path = parent.join(format!(
            ".fileid-restore-lock-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&sentinel_path)
            .context("create restore parent lock")?;
        let sentinel = WindowsRestoreSentinel {
            file: Some(open_windows_directory_lock(&sentinel_path)?),
            path: sentinel_path,
        };
        let parent_handle = open_windows_directory_lock(parent)?;
        let candidate = windows_handle_path(&parent_handle)?;
        let sentinel_parent = windows_handle_path(
            sentinel
                .file
                .as_ref()
                .context("restore parent lock was not secured")?,
        )?
        .parent()
        .context("restore parent lock has no parent")?
        .to_path_buf();
        let candidate = crate::util::path_safety::normalize_for_exclusion(&candidate);
        if candidate != crate::util::path_safety::normalize_for_exclusion(&sentinel_parent) {
            anyhow::bail!("restore parent changed while it was being secured");
        }
        if !authorized_roots.iter().any(|root| {
            let root = crate::util::path_safety::normalize_for_exclusion(root);
            candidate == root
                || candidate
                    .strip_prefix(&root)
                    .is_some_and(|tail| tail.starts_with('\\'))
        }) {
            anyhow::bail!("restore parent handle is outside every authorized library root");
        }
        if original.symlink_metadata().is_ok() {
            anyhow::bail!("restore destination became occupied");
        }
        use std::os::windows::ffi::OsStrExt;
        let leaf = original
            .file_name()
            .context("restore destination has no filename")?
            .encode_wide()
            .collect();
        Ok(Self {
            leaf,
            parent: parent_handle,
            _sentinel: sentinel,
        })
    }

    fn restore_claim(
        &self,
        claim: &std::path::Path,
        expected: Option<crate::platform::FileIdentity>,
    ) -> anyhow::Result<crate::platform::FileIdentity> {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{BOOLEAN, HANDLE};
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, DELETE,
            FILE_ATTRIBUTE_NORMAL, FILE_RENAME_INFO, FILE_RENAME_INFO_0, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING,
        };

        let expected = expected.context("Trash journal has no source identity")?;
        let claim = crate::util::path_safety::to_extended_length(claim);
        let mut claim_wide: Vec<u16> = claim.as_os_str().encode_wide().collect();
        claim_wide.push(0);
        let handle = unsafe {
            CreateFileW(
                PCWSTR(claim_wide.as_ptr()),
                DELETE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(windows_io_error)?;
        if handle.is_invalid() {
            return Err(std::io::Error::last_os_error()).context("open restored Trash claim");
        }
        let claim_file = unsafe { std::fs::File::from_raw_handle(handle.0 as _) };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(handle, &mut info) }
            .map_err(windows_io_error)?;
        let actual = crate::platform::FileIdentity {
            volume: u64::from(info.dwVolumeSerialNumber),
            file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        };
        if actual != expected {
            anyhow::bail!("restored claim identity does not match the Trash journal");
        }

        let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        let byte_len = header + self.leaf.len() * std::mem::size_of::<u16>();
        let mut storage = vec![0u64; byte_len.div_ceil(std::mem::size_of::<u64>())];
        let rename = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        let parent_handle = self.parent.as_raw_handle();
        unsafe {
            (*rename).Anonymous = FILE_RENAME_INFO_0 {
                ReplaceIfExists: BOOLEAN(0),
            };
            (*rename).RootDirectory = HANDLE(parent_handle);
            (*rename).FileNameLength = u32::try_from(self.leaf.len() * 2)?;
            std::ptr::copy_nonoverlapping(
                self.leaf.as_ptr(),
                std::ptr::addr_of_mut!((*rename).FileName).cast::<u16>(),
                self.leaf.len(),
            );
            nt_rename_relative(
                HANDLE(claim_file.as_raw_handle()),
                rename.cast(),
                u32::try_from(byte_len)?,
            )
        }
        .context("move restored claim into its authorized parent")?;
        Ok(actual)
    }
}

#[cfg(windows)]
#[repr(C)]
struct IoStatusBlock {
    status_or_pointer: usize,
    information: usize,
}

#[cfg(windows)]
pub(crate) unsafe fn nt_rename_relative(
    handle: windows::Win32::Foundation::HANDLE,
    information: *const std::ffi::c_void,
    length: u32,
) -> std::io::Result<()> {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSetInformationFile(
            file_handle: *mut std::ffi::c_void,
            io_status_block: *mut IoStatusBlock,
            file_information: *const std::ffi::c_void,
            length: u32,
            file_information_class: u32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
    }

    let mut io_status = IoStatusBlock {
        status_or_pointer: 0,
        information: 0,
    };
    let status = unsafe {
        NtSetInformationFile(
            handle.0,
            &mut io_status,
            information,
            length,
            10,
        )
    };
    if status >= 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ))
    }
}

#[cfg(windows)]
pub(crate) fn windows_handle_path(file: &std::fs::File) -> anyhow::Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, GETFINALPATHNAMEBYHANDLE_FLAGS,
    };

    let handle = HANDLE(file.as_raw_handle());
    let mut buffer = vec![0u16; 512];
    loop {
        let length = unsafe {
            GetFinalPathNameByHandleW(handle, &mut buffer, GETFINALPATHNAMEBYHANDLE_FLAGS(0))
        } as usize;
        if length == 0 {
            return Err(std::io::Error::last_os_error()).context("resolve restore parent handle");
        }
        if length < buffer.len() {
            return Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(
                &buffer[..length],
            )));
        }
        buffer.resize(length + 1, 0);
    }
}

#[cfg(windows)]
pub(crate) fn open_windows_directory_lock(path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let path = crate::util::path_safety::to_extended_length(path);
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(windows_io_error)?;
    if handle.is_invalid() {
        return Err(std::io::Error::last_os_error()).context("open restore directory");
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle, &mut info) }
        .map_err(windows_io_error)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        anyhow::bail!("restore path contains a junction or reparse point");
    }
    Ok(unsafe { std::fs::File::from_raw_handle(handle.0 as _) })
}

#[cfg(target_os = "linux")]
type PlatformRestoreTarget = crate::shell::trash::RestoreTarget;

#[cfg(all(not(windows), not(target_os = "linux")))]
struct PlatformRestoreTarget;

#[cfg(target_os = "linux")]
fn prepare_restore_target(
    original: &std::path::Path,
    authorized_roots: &[std::path::PathBuf],
) -> anyhow::Result<PlatformRestoreTarget> {
    PlatformRestoreTarget::prepare(original, authorized_roots)
}

#[cfg(windows)]
fn prepare_restore_target(
    original: &std::path::Path,
    authorized_roots: &[std::path::PathBuf],
) -> anyhow::Result<PlatformRestoreTarget> {
    PlatformRestoreTarget::prepare(original, authorized_roots)
}

#[cfg(all(not(windows), not(target_os = "linux")))]
fn prepare_restore_target(
    _original: &std::path::Path,
    _authorized_roots: &[std::path::PathBuf],
) -> anyhow::Result<PlatformRestoreTarget> {
    anyhow::bail!("automatic restore is unsupported on this platform")
}

struct PreparedRestore {
    index: usize,
    original: std::path::PathBuf,
    #[allow(dead_code)]
    target: PlatformRestoreTarget,
    #[allow(dead_code)]
    claim: Option<std::path::PathBuf>,
    #[cfg(windows)]
    recycle_physical: Option<std::path::PathBuf>,
    source_identity: Option<crate::platform::FileIdentity>,
}

fn already_restored_outcome(
    item: &trash_log::TrashLogItem,
    original: &std::path::Path,
) -> Option<RestoreOutcome> {
    let identity = item.source_identity?;
    (crate::platform::file_identity(original) == Some(identity)).then(|| {
        RestoreOutcome::Restored(
            identity,
            item.recycle_physical_path
                .as_deref()
                .map(std::path::PathBuf::from),
        )
    })
}

fn reconcile_restored_catalog(
    tx: &rusqlite::Transaction<'_>,
    item: &trash_log::TrashLogItem,
    identity: crate::platform::FileIdentity,
) -> anyhow::Result<()> {
    let path = std::path::Path::new(&item.original_path);
    let metadata = std::fs::symlink_metadata(path)?;
    if crate::platform::file_identity(path) != Some(identity) {
        anyhow::bail!("restored path identity changed before catalog reconciliation");
    }
    let size = i64::try_from(metadata.len()).context("restored file is too large for the catalog")?;
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let kind = crate::pipeline::discovery::FileKind::from_extension(&extension);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
    let file_ref = identity.file as i64;
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO files \
         (id, path_text, path_hash, path_search, size_bytes, scanned_at, kind, extension, \
          file_ref, has_faces, has_text, failed) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0, 0)",
        rusqlite::params![
            item.file_id,
            item.original_path,
            crate::util::path_safety::stable_path_hash(&item.original_path),
            crate::pipeline::dbwriter::nfc_path_search(&item.original_path),
            size,
            now,
            kind.as_str(),
            extension,
            file_ref,
        ],
    )?;
    if inserted == 1 {
        return Ok(());
    }
    let exact: i64 = tx.query_row(
        "SELECT COUNT(*) FROM files \
         WHERE id=?1 AND path_text=?2 AND size_bytes=?3 AND file_ref=?4",
        rusqlite::params![item.file_id, item.original_path, size, file_ref],
        |row| row.get(0),
    )?;
    if exact == 1 {
        Ok(())
    } else {
        anyhow::bail!(
            "catalog row conflicts with recovered file id {} or path {}",
            item.file_id,
            item.original_path
        )
    }
}

#[cfg(windows)]
fn persist_windows_restore_receipts(
    entry: &mut trash_log::TrashLogEntry,
    prepared: &mut [PreparedRestore],
) -> anyhow::Result<()> {
    let mut changed = false;
    for item in prepared.iter_mut() {
        if item.recycle_physical.as_deref().is_some_and(|path| {
            crate::platform::file_identity(path) != item.source_identity
        }) {
            item.recycle_physical = None;
            entry.items[item.index].recycle_physical_path = None;
            changed = true;
        }
    }
    let claims: Vec<&str> = prepared
        .iter()
        .filter(|item| item.recycle_physical.is_none())
        .filter_map(|item| item.claim.as_deref()?.to_str())
        .collect();
    let physical = invoke_windows_restore_batch(&claims);
    for item in prepared {
        if item.recycle_physical.is_some() {
            continue;
        }
        let Some(claim) = item.claim.as_deref() else {
            continue;
        };
        let key = crate::util::path_safety::normalize_for_exclusion(claim);
        let Some(path) = physical.get(&key) else {
            continue;
        };
        if crate::platform::file_identity(path) != item.source_identity {
            continue;
        }
        item.recycle_physical = Some(path.clone());
        entry.items[item.index].recycle_physical_path =
            Some(path.to_string_lossy().into_owned());
        changed = true;
    }
    if changed {
        trash_log::append(entry).context("persist identity-bound Recycle Bin receipt")?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn persist_windows_restore_receipts(
    _entry: &mut trash_log::TrashLogEntry,
    _prepared: &mut [PreparedRestore],
) -> anyhow::Result<()> {
    Ok(())
}

pub(crate) async fn handle_restore_from_trash(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RestoreFromTrashPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let mut entry = trash_log::read_batch(&payload.batch_id)?
            .ok_or_else(|| anyhow::anyhow!("trash log batch {} not found", payload.batch_id))?;
        let allowed_canonical: Vec<std::path::PathBuf> = {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                "SELECT DISTINCT root_path FROM scan_sessions WHERE root_path IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.filter_map(|root| root.ok())
                .filter_map(|root| std::fs::canonicalize(root).ok())
                .collect()
        };

        let mut outcomes: Vec<Option<RestoreOutcome>> =
            std::iter::repeat_with(|| None).take(entry.items.len()).collect();
        let mut prepared = Vec::new();
        for (index, item) in entry.items.iter().enumerate() {
            let original = std::path::Path::new(&item.original_path);
            let candidate = crate::util::path_safety::canonicalize_for_containment(original);
            let allowed = allowed_canonical
                .iter()
                .any(|root| candidate.starts_with(root));
            let occupied = original.symlink_metadata().is_ok();
            let already_restored = already_restored_outcome(item, original);
            match restore_disposition(allowed, occupied) {
                RestoreDisposition::Refused => {
                    outcomes[index] = Some(RestoreOutcome::Refused(
                        "path is outside every authorized library root".into(),
                    ));
                }
                RestoreDisposition::Conflict => {
                    outcomes[index] = Some(already_restored.unwrap_or(RestoreOutcome::Conflict));
                }
                RestoreDisposition::Restore => {
                    #[cfg(windows)]
                    if item.recycle_bin_id.is_none() || item.source_identity.is_none() {
                        outcomes[index] = Some(RestoreOutcome::Failed(
                            "this legacy Trash record has no identity-bound claim; restore it manually in Explorer"
                                .into(),
                        ));
                        continue;
                    }
                    match prepare_restore_target(original, &allowed_canonical) {
                        Ok(target) => prepared.push(PreparedRestore {
                            index,
                            original: original.to_path_buf(),
                            target,
                            claim: item.recycle_bin_id.as_deref().map(std::path::PathBuf::from),
                            #[cfg(windows)]
                            recycle_physical: item
                                .recycle_physical_path
                                .as_deref()
                                .map(std::path::PathBuf::from),
                            source_identity: item.source_identity,
                        }),
                        Err(error) => {
                            outcomes[index] = Some(if original.symlink_metadata().is_ok() {
                                RestoreOutcome::Conflict
                            } else {
                                RestoreOutcome::Failed(format!(
                                    "restore destination could not be securely pinned: {error}"
                                ))
                            });
                        }
                    }
                }
            }
        }

        persist_windows_restore_receipts(&mut entry, &mut prepared)?;
        for (prepared_item, outcome) in prepared
            .iter()
            .zip(restore_batch_from_recycle_bin(&prepared))
        {
            let outcome = match prepared_item.source_identity {
                Some(expected)
                    if !matches!(outcome, RestoreOutcome::Restored(_, _))
                        && crate::platform::file_identity(&prepared_item.original)
                            == Some(expected) =>
                {
                    RestoreOutcome::Restored(expected, None)
                }
                _ => outcome,
            };
            outcomes[prepared_item.index] = Some(outcome);
        }

        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::with_capacity(entry.items.len());
        let mut cleanup_actions = Vec::new();
        for (item, outcome) in entry.items.iter().zip(outcomes) {
            let outcome = outcome.unwrap_or_else(|| {
                RestoreOutcome::Failed("restore backend returned no result".into())
            });
            match outcome {
                RestoreOutcome::Restored(identity, cleanup)
                    if crate::platform::file_identity(std::path::Path::new(&item.original_path))
                        == Some(identity) =>
                {
                    match reconcile_restored_catalog(&tx, item, identity) {
                        Ok(()) => {
                            succeeded += 1;
                            cleanup_actions.push((item.original_path.clone(), cleanup));
                            messages.push(BulkActionItem {
                                file_id: Some(item.file_id),
                                ok: true,
                                message: Some(item.original_path.clone()),
                            });
                        }
                        Err(error) => {
                            failed += 1;
                            messages.push(BulkActionItem {
                                file_id: Some(item.file_id),
                                ok: false,
                                message: Some(format!(
                                    "restored {}, but catalog reconciliation failed: {error}",
                                    item.original_path
                                )),
                            });
                        }
                    }
                }
                RestoreOutcome::Restored(_, _) => {
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(item.file_id),
                        ok: false,
                        message: Some(format!(
                            "the restored object is no longer present at {}",
                            item.original_path
                        )),
                    });
                }
                RestoreOutcome::Conflict => {
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(item.file_id),
                        ok: false,
                        message: Some(format!(
                            "Cannot restore: {} is already occupied by another file.",
                            item.original_path
                        )),
                    });
                }
                RestoreOutcome::Refused(reason) | RestoreOutcome::Failed(reason) => {
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(item.file_id),
                        ok: false,
                        message: Some(format!("Could not restore {}: {reason}", item.original_path)),
                    });
                }
            }
        }
        tx.commit()?;
        for (original, cleanup) in cleanup_actions {
            let _ = (&original, &cleanup);
            #[cfg(windows)]
            if let Some(physical) = cleanup.as_deref() {
                remove_windows_recycle_metadata(physical);
            }
            #[cfg(target_os = "linux")]
            crate::shell::trash::forget_restore_record(std::path::Path::new(&original));
        }
        Ok(BulkActionResult {
            action: "restoreFromTrash".into(),
            succeeded,
            failed,
            messages,
        })
    })
    .await;

    emit_bulk_result(&sink, "restoreFromTrash", result).await;
}

/// Separator for the `FILEID_RB_PATHS` env transport (engine -> PowerShell).
/// MUST be NUL-free: `std::process::Command` runs `ensure_no_nuls` on every env
/// value, so an interior NUL makes `.status()` return `Err` WITHOUT ever
/// spawning powershell.exe — which silently restored NOTHING for every
/// multi-file batch (`wanted_paths.len() >= 2`). U+001F (Unit Separator) is
/// NUL-free yet still forbidden in Windows file names (0x01-0x1F), so it can't
/// appear in any `original_path` or inject a spurious entry; the script splits
/// on the same byte (`-split [char]0x1f`). (C1-018)
#[cfg(any(windows, test))]
const RB_PATH_SEP: &str = "\u{1f}";

/// PowerShell batch-restore script. The wanted set uses an ordinal-IGNORE-CASE
/// comparer so the bin's reconstructed path matches the DB-stored
/// `original_path` even when their casing diverges (drive-letter / Shell path
/// normalization). The default parameterless HashSet[string] ctor is ordinal
/// case-SENSITIVE, which regressed the case-insensitive `-eq` match the
/// per-item helper used and silently failed recoverable restores. (R-02)
///
/// The match key is reconstructed from `DeletedFrom + name`, but `$i.Name`
/// follows Explorer's "Hide extensions for known file types" setting and can
/// return the display name WITHOUT the extension ("document" for
/// "document.txt") — so `DeletedFrom\document` never matched the DB's
/// `...\document.txt` and the whole batch silently restored nothing on any box
/// with the default shell setting. The physical recycled file (`$i.Path` →
/// `...\$R######.txt`) always keeps the original extension, so we test BOTH the
/// display-name path AND the display-name-plus-physical-extension path against
/// the wanted set. Checking two candidates (rather than grafting only when the
/// name looks extensionless) also covers multi-dot names like `archive.tar.gz`,
/// where hiding the final ".gz" still leaves an apparent ".tar" extension.
/// NOTE: this is concatenated into a SINGLE line — no `#` comments (they would
/// swallow the rest of the script) and statements are `;`-separated. (audit 2026-07-08)
#[cfg(any(windows, test))]
const RESTORE_BATCH_SCRIPT: &str = "\
$shell = New-Object -ComObject Shell.Application; \
$bin = $shell.NameSpace(0x0a); \
$enc = [System.Text.Encoding]::Unicode; \
$wanted = New-Object System.Collections.Generic.HashSet[string]([System.StringComparer]::OrdinalIgnoreCase); \
foreach ($w in ($env:FILEID_RB_PATHS -split [char]0x1f)) { if ($w.Length -gt 0) { [void]$wanted.Add($w) } }; \
foreach ($i in $bin.Items()) { \
    $loc = $i.ExtendedProperty('System.Recycle.DeletedFrom'); \
    if ($null -eq $loc) { continue } \
    $cands = @(Join-Path $loc $i.Name); \
    $pext = [System.IO.Path]::GetExtension($i.Path); \
    if ($pext) { $cands += (Join-Path $loc ($i.Name + $pext)) } \
    foreach ($full in $cands) { \
        if ($wanted.Contains($full)) { \
            $left = [Convert]::ToBase64String($enc.GetBytes($full)); \
            $right = [Convert]::ToBase64String($enc.GetBytes($i.Path)); \
            [Console]::Out.WriteLine($left + [char]9 + $right); \
            [void]$wanted.Remove($full); \
            break; \
        } \
    } \
    if ($wanted.Count -eq 0) { break } \
}";

/// Enumerate the Recycle Bin once and return the physical `$R...` file for
/// each requested claim. PowerShell is lookup-only: Rust verifies the logged
/// volume/file identity and performs the no-replace move relative to the pinned
/// authorized parent handle. This avoids Shell Undelete resolving a mutable
/// destination path. U+001F transports the requested paths without an
/// interpolation surface, and base64-encoded UTF-16 stdout preserves every
/// valid Windows path inside a bounded response.
#[cfg(any(windows, test))]
fn partition_windows_restore_paths<'a>(
    wanted_paths: &[&'a str],
    max_units: usize,
) -> Vec<Vec<&'a str>> {
    let mut chunks = Vec::new();
    let mut chunk = Vec::new();
    let mut units = 0usize;
    for path in wanted_paths {
        let path_units = path.encode_utf16().count().saturating_add(1);
        if path_units > max_units {
            continue;
        }
        if !chunk.is_empty() && units.saturating_add(path_units) > max_units {
            chunks.push(std::mem::take(&mut chunk));
            units = 0;
        }
        chunk.push(*path);
        units += path_units;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

#[cfg(windows)]
pub(crate) fn invoke_windows_restore_batch(
    wanted_paths: &[&str],
) -> std::collections::HashMap<String, std::path::PathBuf> {
    const ENV_PATH_UNITS: usize = 12_000;
    let chunks = partition_windows_restore_paths(wanted_paths, ENV_PATH_UNITS);
    if chunks.iter().map(Vec::len).sum::<usize>() != wanted_paths.len() {
        tracing::warn!("Recycle Bin lookup path exceeded the bounded environment transport");
    }
    let mut found = std::collections::HashMap::new();
    for chunk in chunks {
        found.extend(invoke_windows_restore_chunk(&chunk));
    }
    found
}

#[cfg(windows)]
fn invoke_windows_restore_chunk(
    wanted_paths: &[&str],
) -> std::collections::HashMap<String, std::path::PathBuf> {
    use base64::Engine;
    use std::io::Read;
    use std::os::windows::ffi::OsStringExt;

    const OUTPUT_CAP: u64 = 32 * 1024 * 1024;
    if wanted_paths.is_empty() {
        return std::collections::HashMap::new();
    }
    // Match the full deleted-from path and emit only the first physical item
    // for each claim, preserving deterministic behavior for duplicate bin rows.
    let joined = wanted_paths.join(RB_PATH_SEP);
    let script = RESTORE_BATCH_SCRIPT;
    let Some(powershell) = system_powershell_path() else {
        tracing::warn!("could not resolve the trusted system PowerShell path");
        return std::collections::HashMap::new();
    };
    let child = std::process::Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("FILEID_RB_PATHS", &joined)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%error, "powershell Recycle Bin lookup failed to spawn");
            return std::collections::HashMap::new();
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return std::collections::HashMap::new();
    };
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(OUTPUT_CAP + 1).read_to_end(&mut bytes);
        (result, bytes)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => {
                tracing::warn!(code = ?status.code(), "powershell Recycle Bin lookup exited non-zero");
                break;
            }
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!("powershell Recycle Bin lookup timed out and was terminated");
                break;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!(%error, "waiting for powershell Recycle Bin lookup failed");
                break;
            }
        }
    }

    let Ok((Ok(_), bytes)) = reader.join() else {
        return std::collections::HashMap::new();
    };
    if bytes.len() as u64 > OUTPUT_CAP {
        tracing::warn!("powershell Recycle Bin lookup output exceeded its bound");
        return std::collections::HashMap::new();
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return std::collections::HashMap::new();
    };
    let decode_path = |encoded: &str| -> Option<std::path::PathBuf> {
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
        if bytes.len() % 2 != 0 {
            return None;
        }
        let wide: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        Some(std::path::PathBuf::from(std::ffi::OsString::from_wide(&wide)))
    };
    text.lines()
        .filter_map(|line| {
            let (wanted, physical) = line.split_once('\t')?;
            let wanted = decode_path(wanted)?;
            let physical = decode_path(physical)?;
            Some((
                crate::util::path_safety::normalize_for_exclusion(&wanted),
                physical,
            ))
        })
        .collect()
}

#[cfg(windows)]
fn system_powershell_path() -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    let mut buffer = vec![0u16; 32_768];
    // The OS supplies the System32 path; unlike PATH/SystemRoot lookup this
    // cannot be redirected to a same-user executable.
    let length = unsafe {
        windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut buffer))
    } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    let system_dir = std::ffi::OsString::from_wide(&buffer[..length]);
    Some(
        std::path::PathBuf::from(system_dir)
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe"),
    )
}

#[allow(dead_code)]
fn restore_error_kind(error: &anyhow::Error) -> Option<std::io::ErrorKind> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .map(std::io::Error::kind)
}

#[cfg(windows)]
fn restore_batch_from_recycle_bin(prepared: &[PreparedRestore]) -> Vec<RestoreOutcome> {
    let claims: Vec<&str> = prepared
        .iter()
        .filter(|item| item.recycle_physical.is_none())
        .filter_map(|item| item.claim.as_deref()?.to_str())
        .collect();
    let physical_sources = invoke_windows_restore_batch(&claims);

    prepared
        .iter()
        .map(|item| {
            let Some(claim) = item.claim.as_deref() else {
                return RestoreOutcome::Failed("Trash record has no claim path".into());
            };
            let key = crate::util::path_safety::normalize_for_exclusion(claim);
            let physical = item
                .recycle_physical
                .as_ref()
                .or_else(|| physical_sources.get(&key));
            let source = physical.map_or(claim, std::path::PathBuf::as_path);
            match item.target.restore_claim(source, item.source_identity) {
                Ok(identity) => RestoreOutcome::Restored(identity, physical.cloned()),
                Err(error)
                    if restore_error_kind(&error) == Some(std::io::ErrorKind::AlreadyExists) =>
                {
                    RestoreOutcome::Conflict
                }
                Err(error) => RestoreOutcome::Failed(error.to_string()),
            }
        })
        .collect()
}

#[cfg(windows)]
fn windows_recycle_metadata_path(physical: &std::path::Path) -> Option<std::path::PathBuf> {
    let name = physical.file_name().and_then(|name| name.to_str())?;
    let suffix = name.strip_prefix("$R")?;
    Some(physical.with_file_name(format!("$I{suffix}")))
}

#[cfg(windows)]
fn remove_windows_recycle_metadata(physical: &std::path::Path) {
    let Some(info) = windows_recycle_metadata_path(physical) else {
        return;
    };
    if let Err(error) = std::fs::remove_file(info) {
        tracing::warn!(?error, "could not remove restored Recycle Bin metadata");
    }
}

#[cfg(target_os = "linux")]
fn restore_batch_from_recycle_bin(prepared: &[PreparedRestore]) -> Vec<RestoreOutcome> {
    let targets: Vec<(
        &crate::shell::trash::RestoreTarget,
        Option<crate::platform::FileIdentity>,
    )> = prepared
        .iter()
        .map(|item| (&item.target, item.source_identity))
        .collect();
    crate::shell::trash::restore(&targets)
        .into_iter()
        .zip(prepared)
        .map(|(outcome, item)| match outcome {
            crate::shell::trash::RestoreOutcome::Restored(identity) => {
                RestoreOutcome::Restored(identity, None)
            }
            backend_outcome => {
                let Some(claim) = item.claim.as_deref() else {
                    return match backend_outcome {
                        crate::shell::trash::RestoreOutcome::Conflict => RestoreOutcome::Conflict,
                        crate::shell::trash::RestoreOutcome::Failed(error) => {
                            RestoreOutcome::Failed(error)
                        }
                        crate::shell::trash::RestoreOutcome::Restored(_) => unreachable!(),
                    };
                };
                match item.target.restore_claim(claim, item.source_identity) {
                    Ok(identity) => RestoreOutcome::Restored(identity, None),
                    Err(error)
                        if restore_error_kind(&error) == Some(std::io::ErrorKind::AlreadyExists) =>
                    {
                        RestoreOutcome::Conflict
                    }
                    Err(claim_error) => match backend_outcome {
                        crate::shell::trash::RestoreOutcome::Conflict => RestoreOutcome::Conflict,
                        crate::shell::trash::RestoreOutcome::Failed(backend_error) => {
                            RestoreOutcome::Failed(format!(
                                "{backend_error}; claim recovery failed: {claim_error}"
                            ))
                        }
                        crate::shell::trash::RestoreOutcome::Restored(_) => unreachable!(),
                    },
                }
            }
        })
        .collect()
}

#[cfg(all(not(windows), not(target_os = "linux")))]
fn restore_batch_from_recycle_bin(prepared: &[PreparedRestore]) -> Vec<RestoreOutcome> {
    prepared
        .iter()
        .map(|_| RestoreOutcome::Failed("automatic restore is unsupported".into()))
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
struct RevertMergeOutcome {
    person_id: Option<i64>,
    moved: u32,
    already_restored: u32,
    stale: u32,
}

fn apply_revert_merge(
    conn: &rusqlite::Connection,
    payload: &ipc::RevertMergePayload,
    now: f64,
) -> anyhow::Result<RevertMergeOutcome> {
    let tx = conn.unchecked_transaction()?;
    let requested: std::collections::BTreeSet<i64> =
        payload.face_ids_to_revert.iter().copied().collect();
    anyhow::ensure!(!requested.is_empty(), "merge undo contains no face IDs");
    let source_exists = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM persons WHERE id = ?1)",
        [payload.source_person_id],
        |row| row.get::<_, bool>(0),
    )?;
    let mut owner_of =
        tx.prepare("SELECT (SELECT person_id FROM face_prints WHERE id = ?1)")?;
    let mut owners = Vec::with_capacity(requested.len());
    for &face_id in &requested {
        owners.push((
            face_id,
            owner_of.query_row([face_id], |row| row.get::<_, Option<i64>>(0))?,
        ));
    }
    drop(owner_of);
    let already_source = owners
        .iter()
        .filter(|(_, owner)| *owner == Some(payload.source_person_id))
        .count();
    let destination_owned = owners
        .iter()
        .filter(|(_, owner)| *owner == Some(payload.destination_person_id))
        .count();
    let new_pid = if source_exists && already_source > 0 {
        Some(payload.source_person_id)
    } else if destination_owned == 0 {
        None
    } else if !source_exists {
        tx.execute(
            "INSERT INTO persons (id, file_count, created_at) VALUES (?1, 0, ?2)",
            rusqlite::params![payload.source_person_id, now],
        )?;
        Some(payload.source_person_id)
    } else {
        tx.execute(
            "INSERT INTO persons (file_count, created_at) VALUES (0, ?1)",
            [now],
        )?;
        Some(tx.last_insert_rowid())
    };
    let mut update = tx.prepare(
        "UPDATE face_prints SET person_id = ?1 \
         WHERE id = ?2 AND person_id = ?3",
    )?;
    let mut moved = 0u32;
    let mut already_restored = 0u32;
    let mut stale = 0u32;
    for (face_id, owner) in owners {
        if owner == new_pid && new_pid.is_some() {
            already_restored += 1;
        } else if owner == Some(payload.destination_person_id) {
            let Some(new_pid) = new_pid else {
                stale += 1;
                continue;
            };
            let changed = update.execute(rusqlite::params![
                new_pid,
                face_id,
                payload.destination_person_id
            ])?;
            if changed == 1 {
                moved += 1;
            } else {
                stale += 1;
            }
        } else {
            stale += 1;
        }
    }
    drop(update);
    if moved > 0 || already_restored > 0 {
        let person_a = payload
            .source_person_id
            .min(payload.destination_person_id);
        let person_b = payload
            .source_person_id
            .max(payload.destination_person_id);
        tx.execute(
            "DELETE FROM face_verifications \
             WHERE person_a = ?1 AND person_b = ?2 \
               AND same_person = 1 AND vlm_model = 'user-merged'",
            rusqlite::params![person_a, person_b],
        )?;
    }
    let mut affected_people = vec![payload.destination_person_id];
    if let Some(new_pid) = new_pid {
        affected_people.push(new_pid);
    }
    affected_people.sort_unstable();
    affected_people.dedup();
    for pid in affected_people {
        tx.execute(
            "UPDATE persons SET file_count = (SELECT COUNT(DISTINCT file_id) \
             FROM face_prints WHERE person_id = ?1) WHERE id = ?1",
            [pid],
        )?;
    }
    tx.commit()?;
    Ok(RevertMergeOutcome {
        person_id: new_pid,
        moved,
        already_restored,
        stale,
    })
}

pub(crate) async fn handle_revert_merge(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RevertMergePayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let conn = db.lock();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let outcome = apply_revert_merge(&conn, &payload, now)?;
        let complete = outcome.stale == 0;
        let restored = outcome.moved + outcome.already_restored;
        let target = outcome
            .person_id
            .map(|person_id| format!("person #{person_id}"))
            .unwrap_or_else(|| "a restored person".into());
        let message = if complete {
            format!("Restored {restored} face print(s) to {target}")
        } else {
            format!(
                "Restored {restored} face print(s) to {target}; skipped {} stale or missing face(s)",
                outcome.stale
            )
        };
        Ok(BulkActionResult {
            action: "revertMerge".into(),
            succeeded: u32::from(complete),
            failed: u32::from(!complete),
            messages: vec![BulkActionItem {
                file_id: None,
                ok: complete,
                message: Some(message),
            }],
        })
    })
    .await;

    emit_bulk_result(&sink, "revertMerge", result).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revert_merge_test_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE persons (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_count INTEGER NOT NULL DEFAULT 0,
                created_at DOUBLE NOT NULL
             );
             CREATE TABLE face_prints (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id INTEGER NOT NULL,
                person_id INTEGER
             );
             CREATE TABLE face_verifications (
                person_a INTEGER NOT NULL,
                person_b INTEGER NOT NULL,
                same_person INTEGER NOT NULL,
                confidence DOUBLE NOT NULL,
                vlm_model TEXT NOT NULL,
                verified_at DOUBLE NOT NULL,
                PRIMARY KEY (person_a, person_b)
             );",
        )
        .unwrap();
        conn
    }

    fn add_person(conn: &rusqlite::Connection, person_id: i64) {
        conn.execute(
            "INSERT INTO persons (id, file_count, created_at) VALUES (?1, 0, 1.0)",
            [person_id],
        )
        .unwrap();
    }

    fn add_face(
        conn: &rusqlite::Connection,
        face_id: i64,
        file_id: i64,
        person_id: i64,
    ) {
        conn.execute(
            "INSERT INTO face_prints (id, file_id, person_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![face_id, file_id, person_id],
        )
        .unwrap();
    }

    fn face_owner(conn: &rusqlite::Connection, face_id: i64) -> Option<i64> {
        conn.query_row(
            "SELECT person_id FROM face_prints WHERE id = ?1",
            [face_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn add_merge_verification(
        conn: &rusqlite::Connection,
        person_a: i64,
        person_b: i64,
    ) {
        conn.execute(
            "INSERT INTO face_verifications (
                person_a, person_b, same_person, confidence, vlm_model, verified_at
             ) VALUES (?1, ?2, 1, 1.0, 'user-merged', 1.0)",
            rusqlite::params![person_a.min(person_b), person_a.max(person_b)],
        )
        .unwrap();
    }

    fn merge_verification_count(conn: &rusqlite::Connection, person_a: i64, person_b: i64) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM face_verifications WHERE person_a = ?1 AND person_b = ?2",
            rusqlite::params![person_a.min(person_b), person_a.max(person_b)],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn revert_payload(face_ids: Vec<i64>) -> ipc::RevertMergePayload {
        ipc::RevertMergePayload {
            source_person_id: 10,
            destination_person_id: 20,
            face_ids_to_revert: face_ids,
        }
    }

    #[test]
    fn revert_merge_exact_replay_is_idempotent() {
        let conn = revert_merge_test_db();
        add_person(&conn, 20);
        add_face(&conn, 101, 1, 20);
        add_face(&conn, 102, 2, 20);
        add_merge_verification(&conn, 10, 20);
        add_merge_verification(&conn, 10, 30);
        let payload = revert_payload(vec![101, 102]);

        assert_eq!(
            apply_revert_merge(&conn, &payload, 2.0).unwrap(),
            RevertMergeOutcome {
                person_id: Some(10),
                moved: 2,
                already_restored: 0,
                stale: 0,
            }
        );
        assert_eq!(
            apply_revert_merge(&conn, &payload, 3.0).unwrap(),
            RevertMergeOutcome {
                person_id: Some(10),
                moved: 0,
                already_restored: 2,
                stale: 0,
            }
        );
        assert_eq!(face_owner(&conn, 101), Some(10));
        assert_eq!(face_owner(&conn, 102), Some(10));
        assert_eq!(merge_verification_count(&conn, 10, 20), 0);
        assert_eq!(merge_verification_count(&conn, 10, 30), 1);
    }

    #[test]
    fn revert_merge_partial_replay_finishes_on_original_source() {
        let conn = revert_merge_test_db();
        add_person(&conn, 10);
        add_person(&conn, 20);
        add_face(&conn, 101, 1, 10);
        add_face(&conn, 102, 2, 20);

        assert_eq!(
            apply_revert_merge(&conn, &revert_payload(vec![101, 102]), 2.0).unwrap(),
            RevertMergeOutcome {
                person_id: Some(10),
                moved: 1,
                already_restored: 1,
                stale: 0,
            }
        );
        assert_eq!(face_owner(&conn, 101), Some(10));
        assert_eq!(face_owner(&conn, 102), Some(10));
    }

    #[test]
    fn revert_merge_recycled_source_id_allocates_a_fresh_person() {
        let conn = revert_merge_test_db();
        add_person(&conn, 10);
        add_person(&conn, 20);
        add_face(&conn, 900, 9, 10);
        add_face(&conn, 101, 1, 20);
        add_face(&conn, 102, 2, 20);

        let outcome =
            apply_revert_merge(&conn, &revert_payload(vec![101, 102]), 2.0).unwrap();
        let restored_person = outcome.person_id.unwrap();
        assert_ne!(restored_person, 10);
        assert_ne!(restored_person, 20);
        assert_eq!(outcome.moved, 2);
        assert_eq!(outcome.stale, 0);
        assert_eq!(face_owner(&conn, 900), Some(10));
        assert_eq!(face_owner(&conn, 101), Some(restored_person));
        assert_eq!(face_owner(&conn, 102), Some(restored_person));
    }

    #[test]
    fn revert_merge_never_steals_reassigned_or_missing_faces() {
        let conn = revert_merge_test_db();
        add_person(&conn, 20);
        add_person(&conn, 30);
        add_face(&conn, 101, 1, 20);
        add_face(&conn, 102, 2, 30);

        let outcome =
            apply_revert_merge(&conn, &revert_payload(vec![101, 102, 999]), 2.0).unwrap();
        assert_eq!(
            outcome,
            RevertMergeOutcome {
                person_id: Some(10),
                moved: 1,
                already_restored: 0,
                stale: 2,
            }
        );
        assert_eq!(face_owner(&conn, 101), Some(10));
        assert_eq!(face_owner(&conn, 102), Some(30));
    }

    #[test]
    fn revert_merge_all_stale_failure_preserves_manual_merge_constraint() {
        let conn = revert_merge_test_db();
        add_person(&conn, 20);
        add_person(&conn, 30);
        add_face(&conn, 102, 2, 30);
        add_merge_verification(&conn, 10, 20);

        let outcome =
            apply_revert_merge(&conn, &revert_payload(vec![102, 999]), 2.0).unwrap();

        assert_eq!(
            outcome,
            RevertMergeOutcome {
                person_id: None,
                moved: 0,
                already_restored: 0,
                stale: 2,
            }
        );
        assert_eq!(face_owner(&conn, 102), Some(30));
        assert_eq!(merge_verification_count(&conn, 10, 20), 1);
    }

    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn intent_journal_claim_is_restored_without_clobbering() {
        let base = std::env::temp_dir().join(format!(
            "fileid-restore-claim-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let root = std::fs::canonicalize(&base).unwrap();
        let original = base.join("original.bin");
        let claim = base.join(".fileid-trash-claim");
        std::fs::write(&claim, b"payload").unwrap();
        let identity = crate::platform::file_identity(&claim);
        let target = prepare_restore_target(&original, std::slice::from_ref(&root)).unwrap();
        target.restore_claim(&claim, identity).unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), b"payload");

        std::fs::remove_file(&original).unwrap();
        std::fs::write(&claim, b"claim").unwrap();
        let identity = crate::platform::file_identity(&claim);
        let target = prepare_restore_target(&original, std::slice::from_ref(&root)).unwrap();
        std::fs::write(&original, b"occupant").unwrap();
        let error = target.restore_claim(&claim, identity).unwrap_err();
        assert_eq!(restore_error_kind(&error), Some(std::io::ErrorKind::AlreadyExists));
        assert_eq!(std::fs::read(&original).unwrap(), b"occupant");
        assert_eq!(std::fs::read(&claim).unwrap(), b"claim");
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    #[cfg(windows)]
    fn secured_windows_restore_parent_cannot_be_replaced() {
        let base = std::env::temp_dir().join(format!(
            "fileid-restore-parent-lock-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let parent = base.join("parent");
        let moved = base.join("moved-parent");
        std::fs::create_dir_all(&parent).unwrap();
        let root = std::fs::canonicalize(&base).unwrap();
        let original = parent.join("original.bin");
        let target = prepare_restore_target(&original, std::slice::from_ref(&root)).unwrap();

        assert!(std::fs::rename(&parent, &moved).is_err());
        drop(target);
        std::fs::rename(&parent, &moved).unwrap();
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "mutates only a temporary file through the real Windows Recycle Bin"]
    fn recycle_lookup_and_handle_relative_restore_round_trip() {
        let base = std::env::temp_dir().join(format!(
            "fileid-recycle-restore-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let claim = base.join(format!(".fileid-trash-{}.txt", uuid::Uuid::new_v4()));
        let original = base.join("restored.txt");
        std::fs::write(&claim, b"payload").unwrap();
        let expected = crate::platform::file_identity(&claim);
        crate::shell::trash::trash_path(&claim).unwrap();
        assert!(!claim.exists());

        let claim_text = claim.to_str().unwrap();
        let physical = invoke_windows_restore_batch(&[claim_text]);
        let key = crate::util::path_safety::normalize_for_exclusion(&claim);
        let physical = physical.get(&key).expect("Recycle Bin physical path");
        assert_eq!(crate::platform::file_identity(physical), expected);
        let info = windows_recycle_metadata_path(physical).unwrap();
        assert!(info.exists());
        let root = std::fs::canonicalize(&base).unwrap();
        let target = prepare_restore_target(&original, &[root]).unwrap();
        target.restore_claim(physical, expected).unwrap();
        let retry = trash_log::TrashLogItem {
            file_id: 7,
            original_path: original.to_string_lossy().into_owned(),
            recycle_bin_id: Some(claim.to_string_lossy().into_owned()),
            recycle_physical_path: Some(physical.to_string_lossy().into_owned()),
            source_identity: expected,
        };
        let cleanup = match already_restored_outcome(&retry, &original) {
            Some(RestoreOutcome::Restored(identity, Some(cleanup))) => {
                assert_eq!(identity, expected.unwrap());
                cleanup
            }
            outcome => panic!("expected retry cleanup receipt, got {outcome:?}"),
        };
        remove_windows_recycle_metadata(&cleanup);

        assert!(!info.exists());
        assert_eq!(std::fs::read(&original).unwrap(), b"payload");
        drop(target);
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn restored_catalog_reconciliation_is_exact_and_idempotent() {
        let base = std::env::temp_dir().join(format!(
            "fileid-restore-catalog-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("restored.bin");
        std::fs::write(&path, b"payload").unwrap();
        let identity = crate::platform::file_identity(&path).unwrap();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let item = trash_log::TrashLogItem {
            file_id: 42,
            original_path: path.to_string_lossy().into_owned(),
            recycle_bin_id: None,
            recycle_physical_path: Some(r"C:\$Recycle.Bin\$Rreceipt.bin".into()),
            source_identity: Some(identity),
        };
        match already_restored_outcome(&item, &path) {
            Some(RestoreOutcome::Restored(recovered, Some(cleanup))) => {
                assert_eq!(recovered, identity);
                assert_eq!(cleanup, std::path::Path::new(r"C:\$Recycle.Bin\$Rreceipt.bin"));
            }
            outcome => panic!("expected retryable restored receipt, got {outcome:?}"),
        }

        let tx = conn.transaction().unwrap();
        reconcile_restored_catalog(&tx, &item, identity).unwrap();
        reconcile_restored_catalog(&tx, &item, identity).unwrap();
        let row: (i64, i64, i64) = tx
            .query_row(
                "SELECT id,size_bytes,file_ref FROM files WHERE path_text=?1",
                [&item.original_path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (42, 7, identity.file as i64));

        let conflicting = trash_log::TrashLogItem {
            file_id: 43,
            ..item
        };
        assert!(reconcile_restored_catalog(&tx, &conflicting, identity).is_err());
        tx.commit().unwrap();
        std::fs::remove_dir_all(base).ok();
    }

    // C1-003: an occupied destination must be a Conflict (not a Restore that
    // later reads the occupant via Path::exists() and falsely reports success).
    #[test]
    fn occupied_destination_is_a_conflict_not_success() {
        // Inside an authorized root but the path is already occupied.
        assert_eq!(
            restore_disposition(true, true),
            RestoreDisposition::Conflict
        );
        // The happy path: allowed + free.
        assert_eq!(
            restore_disposition(true, false),
            RestoreDisposition::Restore
        );
    }

    // SEC-7 still wins: an out-of-root target is Refused regardless of occupancy.
    #[test]
    fn out_of_root_is_refused_before_conflict() {
        assert_eq!(
            restore_disposition(false, false),
            RestoreDisposition::Refused
        );
        assert_eq!(
            restore_disposition(false, true),
            RestoreDisposition::Refused
        );
    }

    // C1-003 deterministic multi-entry: when two log items share one original
    // path, both classify identically (the batch enumeration restores the
    // first matching bin entry per path and removes it from the wanted set, so
    // the pick is deterministic rather than arbitrary). Here we assert the
    // pre-classification is stable and does not depend on item order.
    #[test]
    fn same_path_items_classify_identically() {
        let occupied = true;
        let allowed = true;
        let a = restore_disposition(allowed, occupied);
        let b = restore_disposition(allowed, occupied);
        assert_eq!(a, b);
        assert_eq!(a, RestoreDisposition::Conflict);
    }

    // R-02: the batch-restore wanted set must match paths case-INSENSITIVELY,
    // restoring the case-insensitive `-eq` semantics the per-item helper had.
    // A parameterless HashSet[string] is ordinal case-SENSITIVE, which fails to
    // match (and so fails to restore) a recoverable file whenever the bin's
    // reconstructed path casing diverges from the stored original_path.
    #[test]
    fn restore_batch_script_matches_paths_case_insensitively() {
        assert!(
            RESTORE_BATCH_SCRIPT.contains("[System.StringComparer]::OrdinalIgnoreCase"),
            "batch-restore HashSet must use an ordinal-ignore-case comparer"
        );
        // Guard against a silent revert to the parameterless (case-sensitive) ctor.
        assert!(
            !RESTORE_BATCH_SCRIPT.contains("System.Collections.Generic.HashSet[string];"),
            "must not use the parameterless (ordinal case-sensitive) HashSet ctor"
        );
    }

    // audit 2026-07-08: $i.Name follows the "hide extensions for known file
    // types" shell setting and can drop the extension, so the reconstructed
    // match key must also try the physical extension from $i.Path — otherwise
    // the whole batch silently restores nothing on any box with the default
    // shell setting. Also guard the single-line invariant: a `#` comment or an
    // unescaped newline would break the concatenated script.
    #[test]
    fn restore_batch_script_grafts_physical_extension() {
        assert!(
            RESTORE_BATCH_SCRIPT.contains("[System.IO.Path]::GetExtension($i.Path)"),
            "script must read the physical recycled extension from $i.Path"
        );
        assert!(
            RESTORE_BATCH_SCRIPT.contains("($i.Name + $pext)"),
            "script must test the display-name-plus-physical-extension candidate"
        );
        assert!(RESTORE_BATCH_SCRIPT.contains("$i.Path"));
        assert!(!RESTORE_BATCH_SCRIPT.contains("InvokeVerb"));
        // Single-line invariant: no PowerShell comment tokens (would swallow the
        // rest of the script) and no embedded newlines.
        assert!(
            !RESTORE_BATCH_SCRIPT.contains('#'),
            "script is one line — a '#' would comment out everything after it"
        );
        assert!(
            !RESTORE_BATCH_SCRIPT.contains('\n'),
            "script must remain a single concatenated line"
        );
    }

    // C1-018: a multi-file batch crosses to PowerShell as the FILEID_RB_PATHS
    // env var. std::process::Command runs `ensure_no_nuls` on every env value,
    // so the previous `"\0"` separator made `.status()` return Err WITHOUT ever
    // spawning powershell.exe for any batch of len >= 2 — restoring nothing even
    // though the bytes still sat in the Recycle Bin. Lock the separator NUL-free
    // and keep the Rust join + PowerShell split byte-identical so the script
    // rebuilds exactly the wanted set the engine sent.
    #[test]
    fn recycle_lookup_chunks_fit_the_environment_bound() {
        let paths = ["aa", "bbb", "123456"];
        let chunks = partition_windows_restore_paths(&paths, 6);

        assert_eq!(chunks, vec![vec!["aa"], vec!["bbb"]]);
        assert!(chunks.iter().all(|chunk| {
            chunk
                .iter()
                .map(|path| path.encode_utf16().count() + 1)
                .sum::<usize>()
                <= 6
        }));
    }

    #[test]
    fn batch_restore_env_separator_is_nul_free_and_round_trips() {
        // The exact guard Command::env enforces: an interior NUL aborts the spawn.
        assert!(
            !RB_PATH_SEP.contains('\0'),
            "FILEID_RB_PATHS separator must be NUL-free or Command::env aborts the spawn"
        );
        // The separator is a control char (< 0x20), forbidden in Windows file
        // names, so it can never appear in an original_path and can't inject.
        assert!(
            RB_PATH_SEP.chars().all(|c| u32::from(c) < 0x20),
            "separator must be a control char forbidden in Windows file names"
        );
        // The regressed case: 2+ paths must join to a value Command::env accepts.
        let paths = ["C:\\Users\\a\\one.txt", "C:\\Users\\a\\two.txt", "D:\\x\\3"];
        let joined = paths.join(RB_PATH_SEP);
        assert!(!joined.contains('\0'), "multi-path env value must be NUL-free");
        // Rust join and PowerShell split MUST agree on the separator.
        let round_trip: Vec<&str> = joined.split(RB_PATH_SEP).collect();
        assert_eq!(round_trip, paths.to_vec());
        assert!(
            RESTORE_BATCH_SCRIPT.contains("-split [char]0x1f"),
            "script must split FILEID_RB_PATHS on the same U+001F separator"
        );
        // Guard against a silent revert to the NUL separator that aborts the spawn.
        assert!(
            !RESTORE_BATCH_SCRIPT.contains("-split [char]0)"),
            "must not split on NUL: Command::env rejects the value and never spawns"
        );
    }
}
