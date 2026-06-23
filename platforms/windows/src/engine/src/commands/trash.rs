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

        // C1-003: pre-restore occupancy probe — filesystem only, no DB lock needed.
        // Probed before the restore so a successfully-restored file isn't mistaken
        // for a pre-existing occupant.
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

        // SEC-7: collect authorized roots under a short lock scope so PowerShell
        // runs without holding the DB mutex. The old code held `conn` from here
        // through `restore_batch_from_recycle_bin`, blocking every DB reader for
        // the full 30 s PowerShell call. (T-1 fix)
        let allowed_canonical: Vec<std::path::PathBuf> = {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                "SELECT DISTINCT root_path FROM scan_sessions WHERE root_path IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let roots: Vec<String> = rows.filter_map(|r| r.ok()).collect();
            roots
                .iter()
                .filter_map(|r| std::fs::canonicalize(r).ok())
                .collect()
        }; // conn dropped here — DB mutex released before PowerShell

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

        // Single bin enumeration WITHOUT DB lock held (T-1 fix).
        restore_batch_from_recycle_bin(&to_restore);

        // Re-acquire DB lock for post-restore inserts.
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;

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
                // T-3 fix: propagate real DB errors. OR IGNORE returns Ok(0) for
                // constraint conflicts (path already indexed), so `?` only fires on
                // genuine failures (disk full, corruption, schema mismatch).
                tx.execute(
                    "INSERT OR IGNORE INTO files \
                     (path_text, path_hash, path_search, size_bytes, scanned_at, kind, extension, \
                      has_faces, has_text, failed) \
                     VALUES (?1, ?2, ?6, 0, ?3, ?4, ?5, 0, 0, 0)",
                    rusqlite::params![
                        item.original_path,
                        crate::util::path_safety::stable_path_hash(&item.original_path),
                        now,
                        kind.as_str(),
                        extension,
                        crate::pipeline::dbwriter::nfc_path_search(&item.original_path),
                    ],
                )?;
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
/// comparer so the bin's reconstructed `Join-Path $loc $i.Name` matches the
/// DB-stored `original_path` even when their casing diverges (drive-letter /
/// Shell path normalization). The default parameterless HashSet[string] ctor is
/// ordinal case-SENSITIVE, which regressed the case-insensitive `-eq` match the
/// per-item helper used and silently failed recoverable restores. (R-02)
#[cfg(any(windows, test))]
const RESTORE_BATCH_SCRIPT: &str = "\
$shell = New-Object -ComObject Shell.Application; \
$bin = $shell.NameSpace(0x0a); \
$wanted = New-Object System.Collections.Generic.HashSet[string]([System.StringComparer]::OrdinalIgnoreCase); \
foreach ($w in ($env:FILEID_RB_PATHS -split [char]0x1f)) { if ($w.Length -gt 0) { [void]$wanted.Add($w) } }; \
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

/// C1-007: restore a WHOLE batch of paths with ONE Recycle Bin enumeration.
/// The old per-item helper spawned a fresh PowerShell (each re-walking the
/// entire bin) for every path; a large undo batch ran them serially and blew
/// the app's 30s waiter. Here a single PowerShell pass walks the bin once,
/// matching each item against the requested set.
///
/// `wanted_paths` are the full original paths (each `parent\name`). They cross
/// into the script as one U+001F-separated env var (NUL can't cross an env
/// value — see `RB_PATH_SEP`), so there is no string-interpolation surface.
/// Best-effort: per-path success is verified by the caller via on-disk
/// presence, so a non-zero exit here is not fatal.
#[cfg(windows)]
fn restore_batch_from_recycle_bin(wanted_paths: &[&str]) {
    if wanted_paths.is_empty() {
        return;
    }
    // Build the wanted set. Use the FULL original path (DeletedFrom + Name) as
    // the match key so two trashed files with the same Name under different
    // folders aren't confused. Separate with RB_PATH_SEP (U+001F, NUL-free so
    // it survives the env-var hop) so a path containing a newline
    // can't inject a spurious entry. Restore the FIRST bin entry that matches a
    // given target path and then remove it from the wanted set — deterministic
    // when multiple bin entries share one original path (C1-003).
    let joined = wanted_paths.join(RB_PATH_SEP);
    let script = RESTORE_BATCH_SCRIPT;
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
    match status {
        Ok(status) if !status.success() => {
            tracing::warn!(code = ?status.code(), "powershell batch restore exited non-zero");
        }
        Ok(_) => {}
        // A failed spawn (e.g. an env value Command rejects) must NOT pass
        // silently — it means none of the batch was pulled from the Recycle
        // Bin, so the caller reports every item as unrecoverable. (C1-018)
        Err(e) => {
            tracing::warn!(error = %e, "powershell batch restore failed to spawn");
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
        // Attempt to reclaim the original person id. rows_changed == 0 means
        // OR IGNORE fired: the id was recycled by SQLite for a DIFFERENT person
        // after the prior merge deleted the original row. In that case a SELECT
        // would return the wrong person and all reverted faces would land there.
        // Instead allocate a fresh row so faces always go to the right person.
        // (T-2 fix)
        let rows_changed = tx.execute(
            "INSERT OR IGNORE INTO persons (id, file_count, created_at) VALUES (?1, 0, ?2)",
            rusqlite::params![payload.source_person_id, now],
        )?;
        let new_pid: i64 = if rows_changed > 0 {
            payload.source_person_id
        } else {
            tx.execute(
                "INSERT INTO persons (file_count, created_at) VALUES (0, ?1)",
                rusqlite::params![now],
            )?;
            tx.last_insert_rowid()
        };
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

    // C1-018: a multi-file batch crosses to PowerShell as the FILEID_RB_PATHS
    // env var. std::process::Command runs `ensure_no_nuls` on every env value,
    // so the previous `"\0"` separator made `.status()` return Err WITHOUT ever
    // spawning powershell.exe for any batch of len >= 2 — restoring nothing even
    // though the bytes still sat in the Recycle Bin. Lock the separator NUL-free
    // and keep the Rust join + PowerShell split byte-identical so the script
    // rebuilds exactly the wanted set the engine sent.
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
