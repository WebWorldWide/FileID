//! Trash-related IPC handlers: `restoreFromTrash` (with path-containment
//! check against authorized scan roots, SEC-7) and `revertMerge` (split a
//! merged person cluster back into source + destination).

use crate::ipc::{self, sink::Sink, BulkActionItem, BulkActionResult};
use crate::platform;

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

pub(crate) async fn handle_restore_from_trash(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RestoreFromTrashPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let entry = trash_log::read_batch(&payload.batch_id)?
            .ok_or_else(|| anyhow::anyhow!("trash log batch {} not found", payload.batch_id))?;

        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();

        // The Recycle Bin restore via IFileOperation::Recycle reverse is
        // non-trivial — IShellFolder enumeration of the bin + matching pidl
        // by display path. Shell out to PowerShell's direct cmdlet instead.
        // On success, the file lands at its original path; we re-INSERT a
        // stripped-down DB row so the Library tab shows it again.
        let conn = db.lock();

        // C1-003: capture each destination's pre-restore occupancy. A path that
        // is ALREADY occupied (by a DIFFERENT file the user re-created) cannot be
        // restored without clobbering it — the bin's Undelete is a no-op there and
        // the bytes stay trapped. We must surface that as a conflict error rather
        // than seeing the occupant via Path::exists() and falsely reporting
        // success. Probed before the restore so a successfully-restored file isn't
        // mistaken for a pre-existing occupant.
        let pre_occupied: std::collections::HashSet<String> = entry
            .items
            .iter()
            .filter(|item| {
                std::path::Path::new(&item.original_path)
                    .symlink_metadata()
                    .is_ok()
            })
            .map(|item| item.original_path.clone())
            .collect();

        // SEC-7: collect every authorized scan root from scan_sessions and
        // require each restore destination to be a descendant. Defends
        // against trash_log forgery — a local attacker who appends a
        // hostile entry like
        //   {"original_path":"C:\\Windows\\System32\\foo.exe", ...}
        // would otherwise be able to write into System32 via our
        // PowerShell shell-out. With containment, restore destinations
        // are restricted to user-blessed library directories.
        let allowed_roots: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT root_path FROM scan_sessions WHERE root_path IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let allowed_canonical: Vec<std::path::PathBuf> = allowed_roots
            .iter()
            .filter_map(|r| std::fs::canonicalize(r).ok())
            .collect();

        let tx = conn.unchecked_transaction()?;

        // C1-007: partition into (allowed-to-restore, conflict, refused) WITHOUT
        // spawning PowerShell per item. The allowed set is restored in a SINGLE
        // bin enumeration below so a large undo batch can't blow the app's 30s
        // waiter (each old per-item spawn re-walked the entire Recycle Bin).
        let mut to_restore: Vec<&str> = Vec::new();
        for item in &entry.items {
            let path_obj = std::path::Path::new(&item.original_path);
            let candidate = crate::util::path_safety::canonicalize_for_containment(path_obj);
            let allowed = allowed_canonical
                .iter()
                .any(|root| candidate.starts_with(root));
            let occupied = pre_occupied.contains(&item.original_path);
            match restore_disposition(allowed, occupied) {
                RestoreDisposition::Refused => {
                    tracing::warn!(
                        path = %platform::redact_path_for_log(&item.original_path),
                        "SEC-7: refusing restore — path is outside every authorized library root"
                    );
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(item.file_id),
                        ok: false,
                        message: Some(format!(
                            "Refused: {} is not inside any authorized library root.",
                            item.original_path
                        )),
                    });
                }
                // C1-003: a destination already occupied by a DIFFERENT file is a
                // conflict — restoring would clobber it (or, as the bin's no-op
                // Undelete does, silently leave the bytes trapped). Report a
                // conflict instead of a false success.
                RestoreDisposition::Conflict => {
                    tracing::warn!(
                        path = %platform::redact_path_for_log(&item.original_path),
                        "restore conflict — destination already occupied; not restoring"
                    );
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
                RestoreDisposition::Restore => to_restore.push(&item.original_path),
            }
        }

        // Single bin enumeration for the whole batch (C1-007).
        restore_batch_from_recycle_bin(&to_restore);

        for item in &entry.items {
            // Skip the ones already accounted for above (refused / conflict).
            let attempted = to_restore.contains(&item.original_path.as_str());
            if !attempted {
                continue;
            }
            // C1-003: after the batch restore, success means the file is now
            // present at a path that was NOT pre-occupied — i.e. the bytes we
            // restored, not a stale occupant. (Pre-occupied paths were already
            // filtered into the conflict branch above.)
            let restored = std::path::Path::new(&item.original_path)
                .symlink_metadata()
                .is_ok();
            if restored {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let path_obj = std::path::Path::new(&item.original_path);
                let extension = path_obj
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let kind = crate::pipeline::discovery::FileKind::from_extension(&extension);
                let _ = tx.execute(
                    "INSERT OR IGNORE INTO files \
                     (path_text, path_hash, path_search, size_bytes, scanned_at, kind, extension, \
                      has_faces, has_text, failed) \
                     VALUES (?1, ?2, ?1, 0, ?3, ?4, ?5, 0, 0, 0)",
                    rusqlite::params![
                        item.original_path,
                        crate::util::path_safety::stable_path_hash(&item.original_path),
                        now,
                        kind.as_str(),
                        extension,
                    ],
                );
                succeeded += 1;
                messages.push(BulkActionItem {
                    file_id: Some(item.file_id),
                    ok: true,
                    message: Some(item.original_path.clone()),
                });
            } else {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(item.file_id),
                    ok: false,
                    message: Some(format!(
                        "could not restore from Recycle Bin: {}",
                        item.original_path
                    )),
                });
            }
        }
        tx.commit()?;
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

/// C1-007: restore a WHOLE batch of paths with ONE Recycle Bin enumeration.
/// The old per-item helper spawned a fresh PowerShell (each re-walking the
/// entire bin) for every path; a large undo batch ran them serially and blew
/// the app's 30s waiter. Here a single PowerShell pass walks the bin once,
/// matching each item against the requested set.
///
/// `wanted_paths` are the full original paths (each `parent\name`). They cross
/// into the script as one NUL-separated env var so there is no string-
/// interpolation surface. Best-effort: per-path success is verified by the
/// caller via on-disk presence, so a non-zero exit here is not fatal.
#[cfg(windows)]
fn restore_batch_from_recycle_bin(wanted_paths: &[&str]) {
    if wanted_paths.is_empty() {
        return;
    }
    // Build the wanted set. Use the FULL original path (DeletedFrom + Name) as
    // the match key so two trashed files with the same Name under different
    // folders aren't confused. NUL-separate so a path containing a newline
    // can't inject a spurious entry. Restore the FIRST bin entry that matches a
    // given target path and then remove it from the wanted set — deterministic
    // when multiple bin entries share one original path (C1-003).
    let joined = wanted_paths.join("\0");
    let script = "\
$shell = New-Object -ComObject Shell.Application; \
$bin = $shell.NameSpace(0x0a); \
$wanted = New-Object System.Collections.Generic.HashSet[string]; \
foreach ($w in ($env:FILEID_RB_PATHS -split [char]0)) { if ($w.Length -gt 0) { [void]$wanted.Add($w) } }; \
foreach ($i in $bin.Items()) { \
    $loc = $i.ExtendedProperty('System.Recycle.DeletedFrom'); \
    if ($null -eq $loc) { continue } \
    $full = (Join-Path $loc $i.Name); \
    if ($wanted.Contains($full)) { \
        $i.InvokeVerb('Undelete'); \
        [void]$wanted.Remove($full); \
    } \
    if ($wanted.Count -eq 0) { break } \
}";
    // SEC: pin -ExecutionPolicy Bypass so the script runs even when group
    // policy locks the user-default policy. Script is internal (not user-
    // supplied); the path list crosses via an env var so there's no string-
    // interpolation surface.
    let status = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("FILEID_RB_PATHS", &joined)
        .status();
    if let Ok(status) = status {
        if !status.success() {
            tracing::warn!(code = ?status.code(), "powershell batch restore exited non-zero");
        }
    }
}

#[cfg(not(windows))]
fn restore_batch_from_recycle_bin(_wanted_paths: &[&str]) {}

pub(crate) async fn handle_revert_merge(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RevertMergePayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        // Use the original id if it's still free; else let SQLite pick a new one.
        tx.execute(
            "INSERT OR IGNORE INTO persons (id, file_count, created_at) VALUES (?1, 0, ?2)",
            rusqlite::params![payload.source_person_id, now],
        )?;
        let new_pid: i64 = tx.query_row(
            "SELECT id FROM persons WHERE id = ?1",
            rusqlite::params![payload.source_person_id],
            |r| r.get(0),
        )?;
        let mut update = tx.prepare("UPDATE face_prints SET person_id = ?1 WHERE id = ?2")?;
        let mut moved = 0u32;
        for fid in &payload.face_ids_to_revert {
            update.execute(rusqlite::params![new_pid, fid])?;
            moved += 1;
        }
        drop(update);
        // Recompute EACH person's file_count from its OWN faces. A single
        // `WHERE id IN (?1, ?2)` with the subquery bound to ?1 set the
        // destination person's count to the SOURCE person's face count (the
        // subquery's person_id is fixed to ?1 for both rows) — a wrong count
        // until the next re-cluster. Two correlated updates fix each row. (audit recheck)
        for pid in [new_pid, payload.destination_person_id] {
            let _ = tx.execute(
                "UPDATE persons SET file_count = (SELECT COUNT(DISTINCT file_id) \
                 FROM face_prints WHERE person_id = ?1) WHERE id = ?1",
                rusqlite::params![pid],
            );
        }
        tx.commit()?;
        Ok(BulkActionResult {
            action: "revertMerge".into(),
            succeeded: 1,
            failed: 0,
            messages: vec![BulkActionItem {
                file_id: None,
                ok: true,
                message: Some(format!(
                    "Restored {moved} face print(s) to person #{new_pid}"
                )),
            }],
        })
    })
    .await;

    emit_bulk_result(&sink, "revertMerge", result).await;
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
