//! Trash-related IPC handlers: `restoreFromTrash` (with path-containment
//! check against authorized scan roots, SEC-7) and `revertMerge` (split a
//! merged person cluster back into source + destination).

use crate::ipc::{self, sink::Sink, BulkActionItem, BulkActionResult};
use crate::platform;

use super::bulk::emit_bulk_result;
use super::trash_log;

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
        for item in &entry.items {
            let path_obj = std::path::Path::new(&item.original_path);
            let candidate = crate::util::path_safety::canonicalize_for_containment(path_obj);
            let allowed = allowed_canonical
                .iter()
                .any(|root| candidate.starts_with(root));
            if !allowed {
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
                continue;
            }
            let restored = restore_one_from_recycle_bin(&item.original_path).is_ok();
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
                     (path_text, path_hash, size_bytes, scanned_at, kind, extension, \
                      has_faces, has_text, failed) \
                     VALUES (?1, ?2, 0, ?3, ?4, ?5, 0, 0, 0)",
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

#[cfg(windows)]
fn restore_one_from_recycle_bin(original_path: &str) -> anyhow::Result<()> {
    // PowerShell walks the Recycle Bin, finds an item whose
    // `OriginalLocation` + `Name` matches, and invokes its Verb "Undelete"
    // (Restore). Path data flows through environment variables
    // (FILEID_RB_PARENT / FILEID_RB_NAME) instead of being interpolated
    // into the script — eliminates every escape concern.
    let parent = std::path::Path::new(original_path)
        .parent()
        .ok_or_else(|| anyhow::anyhow!("bad path"))?
        .to_string_lossy()
        .to_string();
    let name = std::path::Path::new(original_path)
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("bad path"))?
        .to_string_lossy()
        .to_string();
    let script = "\
$shell = New-Object -ComObject Shell.Application; \
$bin = $shell.NameSpace(0x0a); \
$wantParent = $env:FILEID_RB_PARENT; \
$wantName = $env:FILEID_RB_NAME; \
foreach ($i in $bin.Items()) { \
    $loc = $i.ExtendedProperty('System.Recycle.DeletedFrom'); \
    $nm = $i.Name; \
    if ($loc -eq $wantParent -and $nm -eq $wantName) { \
        $i.InvokeVerb('Undelete'); break; \
    } \
}";
    // SEC: pin -ExecutionPolicy Bypass so the script runs even when group
    // policy locks the user-default policy. Script is internal (not user-
    // supplied), arguments cross via env vars so there's no string-
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
        .env("FILEID_RB_PARENT", &parent)
        .env("FILEID_RB_NAME", &name)
        .status()?;
    if !status.success() {
        anyhow::bail!("powershell restore exit {:?}", status.code());
    }
    if !std::path::Path::new(original_path).exists() {
        anyhow::bail!("restore reported success but file is still missing");
    }
    Ok(())
}

#[cfg(not(windows))]
fn restore_one_from_recycle_bin(_original_path: &str) -> anyhow::Result<()> {
    anyhow::bail!("Recycle Bin restore not supported on this platform")
}

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
