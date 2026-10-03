//! Bulk action handlers — every `BulkActionResult`-shaped IPC. Apply tags,
//! rename files, trash files, merge person clusters, rename persons, mark
//! persons as unknown, find merge suggestions. They share the
//! `emit_bulk_result` tail so the wire shape stays uniform.

use std::path::PathBuf;

use crate::ipc::{
    self, sink::Sink, BulkActionItem, BulkActionResult, EngineError, EventPayload, IpcEvent,
    MergeSuggestion, MergeSuggestions, TagMode, Wrap,
};
use crate::pipeline::face_clustering::{MERGE_SUGGEST_COS_HIGH, MERGE_SUGGEST_COS_LOW};

use super::trash_log::{self, TrashLogEntry, TrashLogItem};

#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_COPY_ALLOWED};

/// No-clobber filename rename (same directory, filesystem move). On Windows this
/// is `MoveFileExW(MOVEFILE_COPY_ALLOWED)` with NO `MOVEFILE_REPLACE_EXISTING`,
/// so an occupied destination fails the move atomically inside the kernel rather
/// than being silently overwritten — closing the existence-check→rename TOCTOU.
/// Both operands carry the `\\?\` extended-length prefix (the engine .exe has no
/// longPathAware manifest); mirrors restructure_apply.rs::move_file (B3).
#[cfg(windows)]
fn no_clobber_rename(src: &Path, dst: &Path) -> std::io::Result<()> {
    crate::util::read_only::require_source_mutation(src)?;
    crate::util::read_only::require_source_mutation(dst)?;
    use std::os::windows::ffi::OsStrExt;
    let src_ext = crate::util::path_safety::to_extended_length(src);
    let dst_ext = crate::util::path_safety::to_extended_length(dst);
    let src_w: Vec<u16> = src_ext
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst_w: Vec<u16> = dst_ext
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(src_w.as_ptr()),
            PCWSTR(dst_w.as_ptr()),
            MOVEFILE_COPY_ALLOWED,
        )
        .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

#[cfg(not(windows))]
fn no_clobber_rename(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    crate::util::read_only::require_source_mutation(src)?;
    crate::util::read_only::require_source_mutation(dst)?;
    std::fs::rename(
        crate::util::path_safety::to_extended_length(src),
        crate::util::path_safety::to_extended_length(dst),
    )
}

/// C1-012: best-effort durable record of an on-disk rename whose DB row is
/// now stale (either the per-move UPDATE failed, or the end-of-batch commit
/// rolled back every move's UPDATE). Mirrors restructure_apply.rs's B5
/// `record_path_update_failure`: NDJSON, append-only, a recovery HINT (the
/// next scan self-heals via rename-heal on the NTFS file_ref) — not a restore
/// authority like trash_log, so no HMAC. Written beside the trash log.
fn record_rename_recovery(file_id: i64, src: &str, dst: &str) {
    let Ok(trash) = crate::paths::trash_log_path() else {
        return;
    };
    let Some(dir) = trash.parent() else {
        return;
    };
    write_rename_recovery_line(dir, &rename_recovery_line(file_id, src, dst));
}

/// Pure NDJSON line builder for the rename recovery sidecar (kept separate so
/// the wire shape is unit-testable without touching the filesystem).
fn rename_recovery_line(file_id: i64, src: &str, dst: &str) -> String {
    serde_json::json!({ "file_id": file_id, "src": src, "dst": dst }).to_string()
}

/// Append one recovery line to `dir/rename_recover.ndjson`, creating it if
/// absent. Best-effort: a write failure is swallowed (the next scan still
/// self-heals via rename-heal on the NTFS file_ref).
fn write_rename_recovery_line(dir: &std::path::Path, line: &str) {
    let path = dir.join("rename_recover.ndjson");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
        let _ = f.sync_all();
    }
}

/// Bulk-apply tags to a set of files. Updates DB `tags` table + writes the
/// sidecar JSON so Explorer + future scans see the same set.
pub(crate) async fn handle_apply_tags(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::ApplyTagsPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        // Bound the request so a pathological payload can't make the handler do
        // quadratic work (tags × files) under the DB lock or balloon the messages
        // Vec. The IPC peer is the trusted sibling app, but a bug there must not
        // be able to wedge the engine.
        const MAX_TAGS: usize = 2000;
        const MAX_FILES: usize = 100_000;
        if payload.tags.len() > MAX_TAGS || payload.file_ids.len() > MAX_FILES {
            return Ok(BulkActionResult {
                action: "applyTags".into(),
                succeeded: 0,
                failed: payload.file_ids.len().min(u32::MAX as usize) as u32,
                messages: vec![BulkActionItem {
                    file_id: None,
                    ok: false,
                    message: Some(format!(
                        "Request too large: {} tags / {} files (max {MAX_TAGS} / {MAX_FILES})",
                        payload.tags.len(),
                        payload.file_ids.len()
                    )),
                }],
            });
        }
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();
        // (path, tags) to persist to disk (sidecar JSON + IPropertyStore COM)
        // AFTER the tx commits and the writer lock drops — never inside it. (audit P0)
        let mut sidecar_writes: Vec<(String, Vec<String>)> = Vec::new();
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        // Cache prepared statements outside the per-file loop. Raw
        // `tx.execute(sql, ...)` re-parses SQL on every call;
        // `prepare_cached` keeps the parsed statement on the connection
        // so per-tag inserts reuse it.
        for fid in &payload.file_ids {
            let path: Result<String, _> = tx
                .prepare_cached("SELECT path_text FROM files WHERE id = ?1")?
                .query_row(rusqlite::params![fid], |r| r.get::<_, String>(0));
            let path = match path {
                Ok(p) => p,
                Err(err) => {
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(*fid),
                        ok: false,
                        message: Some(format!("not found: {err}")),
                    });
                    continue;
                }
            };
            if matches!(payload.mode, TagMode::Replace) {
                let _ = tx
                    .prepare_cached("DELETE FROM tags WHERE file_id = ?1 AND source = 'user'")?
                    .execute(rusqlite::params![fid]);
            }
            let mut row_ok = true;
            for tag in &payload.tags {
                let trimmed = tag.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let exec_res = match payload.mode {
                    TagMode::Remove => tx
                        .prepare_cached(
                            "DELETE FROM tags WHERE file_id = ?1 AND tag = ?2 AND source = 'user'",
                        )?
                        .execute(rusqlite::params![fid, trimmed]),
                    _ => tx
                        .prepare_cached(
                            "INSERT OR REPLACE INTO tags (file_id, tag, source, score) VALUES (?1, ?2, 'user', NULL)",
                        )?
                        .execute(rusqlite::params![fid, trimmed]),
                };
                if let Err(err) = exec_res {
                    failed += 1;
                    row_ok = false;
                    messages.push(BulkActionItem {
                        file_id: Some(*fid),
                        ok: false,
                        message: Some(format!("tag write failed: {err}")),
                    });
                    break;
                }
            }
            if row_ok {
                let mut stmt = tx.prepare_cached(
                    "SELECT tag FROM tags WHERE file_id = ?1 AND source = 'user' ORDER BY tag",
                )?;
                let rows = stmt.query_map(rusqlite::params![fid], |r| r.get::<_, String>(0))?;
                let tags: Vec<String> = rows.filter_map(|r| r.ok()).collect();
                // Defer the sidecar JSON + IPropertyStore COM write to AFTER the tx
                // commits (see loop past tx.commit). Doing per-file fs+COM (1-10 ms
                // each) inside the open tx held the engine's only writer lock for the
                // whole bulk op and grew the WAL; the sidecar has no transactional
                // coupling to the DB rows (failures only log), so deferring is
                // behavior-preserving. (audit P0)
                sidecar_writes.push((path, tags));
                succeeded += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*fid),
                    ok: true,
                    message: None,
                });
            }
        }
        tx.commit()?;
        // Release the single writer lock BEFORE the per-file fs + COM sidecar
        // writes so a large bulk-tag can't wedge the engine's only writer (and
        // any concurrent scan flush) for the whole operation. (audit P0)
        drop(conn);
        let mut iprops_count = 0u32;
        let mut sidecar_only_count = 0u32;
        for (path, tags) in &sidecar_writes {
            match crate::shell::tags::write_tags_full(std::path::Path::new(path), tags) {
                Ok(true) => iprops_count += 1,
                Ok(false) => sidecar_only_count += 1,
                Err(err) => {
                    tracing::warn!(?err, path = %crate::platform::redact_path_for_log(path), "sidecar tag write failed");
                }
            }
        }
        // Surface a human-readable summary of where the tags landed so the UI
        // can tell the user why Explorer's Details column is blank for some files
        // (those use sidecar-only because their extension has no property handler).
        if iprops_count > 0 || sidecar_only_count > 0 {
            let summary = match (iprops_count, sidecar_only_count) {
                (f, 0) => format!("{f} file(s) tagged in Explorer Keywords + sidecar"),
                (0, s) => format!("{s} file(s) tagged in sidecar only (Explorer Keywords not supported for these file types)"),
                (f, s) => format!("{f} file(s) tagged in Explorer Keywords + sidecar; {s} file(s) sidecar only (no property handler for those extensions)"),
            };
            messages.push(BulkActionItem {
                file_id: None,
                ok: true,
                message: Some(summary),
            });
        }
        Ok(BulkActionResult {
            action: "applyTags".into(),
            succeeded,
            failed,
            messages,
        })
    })
    .await;

    emit_bulk_result(&sink, "applyTags", result).await;
}

/// Bulk-rename a set of files (filename only, same directory). Each move is a
/// no-clobber `MoveFileExW` (no `MOVEFILE_REPLACE_EXISTING`) + DB row update.
pub(crate) async fn handle_rename_files(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RenameFilesPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();
        // C1-012: every move that landed on disk, tracked so a failed
        // end-of-batch commit (which rolls back ALL the per-move DB UPDATEs)
        // can be reconciled via the recovery sidecar — the disk is renamed but
        // the DB is stale across the whole batch otherwise.
        let mut on_disk_moves: Vec<(i64, String, String)> = Vec::new();
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        for entry in &payload.renames {
            // Reject anything that isn't a single Normal path component.
            if !crate::util::path_safety::is_safe_filename(&entry.new_name) {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(entry.file_id),
                    ok: false,
                    message: Some(
                        "new name must be a single filename (no slashes, no '..', no '.', no drive)"
                            .into(),
                    ),
                });
                continue;
            }
            let path: Result<String, _> = tx.query_row(
                "SELECT path_text FROM files WHERE id = ?1",
                rusqlite::params![entry.file_id],
                |r| r.get::<_, String>(0),
            );
            let path = match path {
                Ok(p) => PathBuf::from(p),
                Err(err) => {
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(entry.file_id),
                        ok: false,
                        message: Some(format!("not found: {err}")),
                    });
                    continue;
                }
            };
            let dir = match path.parent() {
                Some(d) => d.to_path_buf(),
                None => {
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(entry.file_id),
                        ok: false,
                        message: Some("source has no parent".into()),
                    });
                    continue;
                }
            };
            let dest = dir.join(&entry.new_name);
            // No-clobber rename. The destination existence is re-checked by the
            // kernel inside the move itself (no MOVEFILE_REPLACE_EXISTING), so a
            // separate symlink_metadata probe + std::fs::rename — which clobbers
            // via MoveFileExW(REPLACE_EXISTING) — is a TOCTOU: an external file
            // materializing in the probe→rename window would be silently
            // overwritten. Here an occupied destination fails the move (failed++)
            // rather than destroying data. The un-prefixed `dest` is still used
            // for DB path_text + user messages so stored paths stay normal-form
            // (#29). Mirrors restructure_apply.rs::move_file (B3).
            if let Err(err) = no_clobber_rename(&path, &dest) {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(entry.file_id),
                    ok: false,
                    message: Some(format!("rename failed: {err}")),
                });
                continue;
            }
            // Move the on-disk tags sidecar to follow the renamed file (#27).
            // Best-effort: a missing sidecar (the common case) or any error is
            // ignored so it never turns a successful rename into a failure.
            crate::shell::tags::move_sidecar(&path, &dest);
            let dest_text = dest.to_string_lossy().to_string();
            let src_text = path.to_string_lossy().to_string();
            // C1-012: the on-disk move is now committed but the DB row is only
            // updated below (and the whole tx commits at end-of-batch). Track it
            // so a failed UPDATE or a failed end-of-batch commit is reconcilable.
            on_disk_moves.push((entry.file_id, src_text, dest_text.clone()));
            // ENG-91: keep path_hash in sync with path_text (lookups/dedup key
            // on it). ENG-92: do NOT swallow the UPDATE error and still claim
            // success — a file renamed on disk but with a failed DB write must
            // be reported as failed (the next scan's rename-heal rebinds it via
            // content_hash/file_ref).
            let dest_hash = crate::util::path_safety::stable_path_hash(&dest_text);
            match tx.execute(
                // path_search NFC-normalized (not verbatim ?1) so an NFD-accented
                // renamed/moved file stays findable by its accented name. (audit parity)
                "UPDATE files SET path_text = ?1, path_hash = ?2, path_search = ?4, vlm_proposed_name = NULL WHERE id = ?3",
                rusqlite::params![
                    dest_text,
                    dest_hash,
                    entry.file_id,
                    crate::pipeline::dbwriter::nfc_path_search(&dest_text)
                ],
            ) {
                Ok(_) => {
                    succeeded += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(entry.file_id),
                        ok: true,
                        message: Some(dest_text),
                    });
                }
                Err(err) => {
                    // C1-012: file is renamed on disk but its row update failed.
                    // Record it to the recovery sidecar so the disk/DB desync is
                    // reconcilable even if the next scan never runs.
                    record_rename_recovery(entry.file_id, &path.to_string_lossy(), &dest_text);
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(entry.file_id),
                        ok: false,
                        message: Some(format!("renamed on disk but DB update failed: {err}")),
                    });
                }
            }
        }
        // C1-012: a failed end-of-batch commit rolls back EVERY per-move UPDATE,
        // leaving every on-disk rename desynced from a now-stale DB across the
        // whole batch. Record all of them to the recovery sidecar before
        // surfacing the error so the batch is reconcilable (mirror restructure
        // B5, which records per-move on a path-update failure).
        if let Err(err) = tx.commit() {
            for (fid, src, dst) in &on_disk_moves {
                record_rename_recovery(*fid, src, dst);
            }
            tracing::error!(?err, moves = on_disk_moves.len(), "bulk rename commit failed — recorded on-disk moves for recovery");
            return Err(anyhow::Error::from(err)
                .context("bulk rename committed on disk but the DB commit failed; recovery sidecar written"));
        }
        Ok(BulkActionResult {
            action: "renameFiles".into(),
            succeeded,
            failed,
            messages,
        })
    })
    .await;

    emit_bulk_result(&sink, "renameFiles", result).await;
}

fn exact_trash_identity_valid(
    identity: &ipc::ExactTrashIdentity,
    file_id: i64,
    indexed_path: &std::path::Path,
    indexed_size: i64,
    mut digest: impl FnMut(&std::path::Path, u64) -> Option<[u8; 32]>,
) -> bool {
    if identity.file_id != file_id
        || indexed_path != std::path::Path::new(&identity.path)
        || indexed_size < 0
        || indexed_size != identity.size_bytes
        || identity.keeper_path == identity.path
    {
        return false;
    }
    let (Ok(expected), Ok(keeper_expected), Ok(size), Ok(keeper_size)) = (
        hex::decode(&identity.sha256_hex),
        hex::decode(&identity.keeper_sha256_hex),
        u64::try_from(identity.size_bytes),
        u64::try_from(identity.keeper_size_bytes),
    ) else {
        return false;
    };
    expected.len() == 32
        && expected == keeper_expected
        && digest(indexed_path, size).is_some_and(|actual| actual.as_slice() == expected)
        && digest(std::path::Path::new(&identity.keeper_path), keeper_size)
            .is_some_and(|actual| actual.as_slice() == expected)
}

/// Trash a set of files. Looks up paths from the DB, hands a Vec<PathBuf>
/// to shell::trash::trash, removes the rows on success.
pub(crate) async fn handle_trash_files(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::TrashFilesPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();
        // ENG-93: capture each path's pre-op existence. shell::trash::trash_path
        // is idempotent — a source that is already gone returns Ok (reported as
        // `true`). That is correct for the shell layer but must not be recorded
        // here as a successful trash: it would pollute the undo/trash log with an
        // entry restoreFromTrash can never honor. A file missing before the op is
        // skipped (failed), not trashed.
        let mut path_for_id: Vec<(i64, PathBuf, bool, i64)> = Vec::with_capacity(payload.file_ids.len());

        {
            let conn = db.lock();
            for fid in &payload.file_ids {
                match conn.query_row(
                    "SELECT path_text, size_bytes FROM files WHERE id = ?1",
                    rusqlite::params![fid],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                ) {
                    Ok((p, size)) => {
                        let path = PathBuf::from(p);
                        let existed = std::fs::symlink_metadata(
                            crate::util::path_safety::to_extended_length(&path),
                        ).is_ok();
                        path_for_id.push((*fid, path, existed, size));
                    }
                    Err(error) => {
                        failed += 1;
                        messages.push(BulkActionItem {
                            file_id: Some(*fid),
                            ok: false,
                            message: Some(format!("File is no longer indexed or could not be read: {error}")),
                        });
                    }
                }
            }
        }

        let identity_by_id = payload.exact_identities.as_ref().map(|identities| {
            identities.iter().map(|identity| (identity.file_id, identity))
                .collect::<std::collections::HashMap<_, _>>()
        });
        let mut digest_cache: std::collections::HashMap<PathBuf, (u64, Option<[u8; 32]>)> =
            std::collections::HashMap::new();
        let valid: Vec<bool> = path_for_id.iter().map(|(fid, path, existed, size)| {
            *existed && identity_by_id.as_ref().is_none_or(|identities| {
                identities.get(fid).is_some_and(|identity| exact_trash_identity_valid(
                    identity, *fid, path, *size, |candidate, expected_size| {
                        if let Some((cached_size, hash)) = digest_cache.get(candidate) {
                            if *cached_size == expected_size { return *hash; }
                        }
                        let hash = crate::util::content_hash::exact_file_sha256(candidate, expected_size).ok();
                        digest_cache.insert(candidate.to_path_buf(), (expected_size, hash));
                        hash
                    }
                ))
            })
        }).collect();
        #[cfg(windows)]
        let outcomes: Vec<(bool, Option<String>)> = {
            let paths: Vec<PathBuf> = path_for_id.iter().zip(&valid)
                .filter(|(_, valid)| **valid).map(|((_, path, _, _), _)| path.clone()).collect();
            let mut results = crate::shell::trash::trash(&paths).into_iter();
            valid.iter().map(|valid| (if *valid { results.next().unwrap_or(false) } else { false }, None)).collect()
        };
        #[cfg(not(windows))]
        let outcomes: Vec<(bool, Option<String>)> = path_for_id.iter().zip(&valid)
            .map(|((_, path, _, _), valid)| {
                if !valid { return (false, None); }
                match crate::shell::trash::trash_path_with_receipt(path) {
                    Ok(receipt) => (true, Some(receipt.to_string_lossy().into_owned())),
                    Err(error) => {
                        tracing::warn!(path = %crate::platform::redact_path_for_log(path), %error, "trash failed");
                        (false, None)
                    }
                }
            }).collect();

        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let mut log_items: Vec<TrashLogItem> = Vec::new();
        for (((fid, path, existed, _), accepted), (trashed_ok, receipt)) in path_for_id.iter().zip(&valid).zip(outcomes) {
            if !existed {
                tracing::warn!(
                    path = %crate::platform::redact_path_for_log(path),
                    "ENG-93: skipping trash record — file was already missing before the op"
                );
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*fid),
                    ok: false,
                    message: Some(format!("already missing: {}", path.display())),
                });
                continue;
            }
            if trashed_ok {
                let _ = tx.execute("DELETE FROM files WHERE id = ?1", rusqlite::params![fid]);
                succeeded += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*fid),
                    ok: true,
                    message: Some(path.to_string_lossy().to_string()),
                });
                log_items.push(TrashLogItem {
                    file_id: *fid,
                    original_path: path.to_string_lossy().to_string(),
                    recycle_bin_id: receipt,
                });
            } else {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*fid),
                    ok: false,
                    message: Some(if *accepted {
                        format!("trash failed: {}", path.display())
                    } else {
                        "Exact duplicate changed, keeper missing, or selected file no longer matches its indexed identity.".into()
                    }),
                });
            }
        }
        // C1-018: write the undo journal BEFORE committing the row-DELETE. The
        // bytes are already in the Recycle Bin (irreversible from here), and the
        // journal is the ONLY map back to them — `restoreFromTrash` reads it to
        // find which paths to bring back. If the append fails AFTER we delete the
        // Library rows, the app's UndoStack restore is a silent no-op (no journal
        // entry). So: append first; on failure, roll back the DELETE (drop the tx
        // without commit) so the file rows survive as a recovery handle and
        // surface a hard error rather than a silently-unrecoverable trash.
        let batch_id = uuid::Uuid::new_v4().to_string();
        if !log_items.is_empty() {
            let entry = TrashLogEntry {
                batch_id: batch_id.clone(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0),
                items: log_items,
            };
            if let Err(err) = trash_log::append(&entry) {
                tracing::error!(?err, "trash_log append failed — rolling back DELETE so the trashed files stay recoverable");
                drop(tx); // rollback: keep the Library rows as a recovery handle
                anyhow::bail!(
                    "files were moved to the Recycle Bin but the undo journal could not be written ({err}); restore is unavailable for this batch"
                );
            }
        }
        tx.commit()?;

        // Tag the BulkActionResult.action with the batch id so the app can
        // store it on the UndoStack entry without an extra IPC.
        Ok(BulkActionResult {
            action: format!("trashFiles:{}", batch_id),
            succeeded,
            failed,
            messages,
        })
    })
    .await;

    emit_bulk_result(&sink, "trashFiles", result).await;
}

/// Merge two person clusters: every face_print with person_id = source is
/// reassigned to destination, then the source person row is deleted.
pub(crate) async fn handle_merge_clusters(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::MergeClustersPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let src = payload.source_person_id;
        let dst = payload.destination_person_id;
        // Self-merge guard: moving a person's faces onto itself then deleting
        // its row would orphan every face (person_id points at a deleted row).
        // Return a no-op success so any caller passing src == dst is safe.
        if src == dst {
            return Ok(BulkActionResult {
                action: "mergeClusters".into(),
                succeeded: 1,
                failed: 0,
                messages: vec![BulkActionItem {
                    file_id: None,
                    ok: true,
                    message: Some(format!("#{src} is already one cluster; nothing to merge")),
                }],
            });
        }
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        anyhow::ensure!(src > 0 && dst > 0, "Select existing people before merging.");
        let selected: i64 = tx.query_row("SELECT COUNT(*) FROM persons WHERE id IN (?1,?2)",rusqlite::params![src,dst],|r|r.get(0))?;
        anyhow::ensure!(selected == 2, "A selected person no longer exists. Refresh People before merging.");
        let moved = tx.execute(
            "UPDATE face_prints SET person_id = ?1 WHERE person_id = ?2",
            rusqlite::params![dst, src],
        )? as u32;
        // Transfer one complete identity only into an unnamed destination.
        tx.execute(
            "UPDATE persons SET name=(SELECT name FROM persons WHERE id=?2), title=(SELECT title FROM persons WHERE id=?2), first_name=(SELECT first_name FROM persons WHERE id=?2), middle_name=(SELECT middle_name FROM persons WHERE id=?2), last_name=(SELECT last_name FROM persons WHERE id=?2), suffix=(SELECT suffix FROM persons WHERE id=?2), is_unknown=0 WHERE id=?1 AND length(trim(COALESCE(name,'') || COALESCE(title,'') || COALESCE(first_name,'') || COALESCE(middle_name,'') || COALESCE(last_name,'') || COALESCE(suffix,'')))=0 AND EXISTS(SELECT 1 FROM persons WHERE id=?2 AND length(trim(COALESCE(name,'') || COALESCE(title,'') || COALESCE(first_name,'') || COALESCE(middle_name,'') || COALESCE(last_name,'') || COALESCE(suffix,'')))>0)",
            rusqlite::params![dst,src],
        )?;
        tx.execute("DELETE FROM persons WHERE id = ?1", rusqlite::params![src])?;
        // Clean up face-verification verdicts referencing the merged-away source
        // person — otherwise findMergeSuggestions JOINs on a now-deleted persons
        // row and surfaces stale suggestions (orphan rows that never GC). The
        // "src != X" verdict is moot once src is folded into dst.
        // R4-06: only GC legacy person-keyed rows that can't re-project (NULL
        // anchors). A v13 face-anchored verdict (face_a/face_b set) must SURVIVE
        // the merge so its (fa,fb) pair keeps re-projecting onto current cluster
        // membership (fa→dst, fb→other) — deleting it would let two
        // user-confirmed-different people re-merge. A row whose faces now land in
        // one cluster is auto-inert (find_merge_suggestions `pa != pb`, consolidate
        // `ca != cb`).
        tx.execute(
            "DELETE FROM face_verifications WHERE (person_a = ?1 OR person_b = ?1) \
             AND (face_a IS NULL OR face_b IS NULL)",
            rusqlite::params![src],
        )?;
        // Recompute the destination's file_count AND representative_face_id
        // (highest-quality embedded face now in the cluster) so the People
        // card + suggestion anchor reflect the combined membership rather than
        // a stale rep. Fall back to unembedded faces when necessary.
        tx.execute(
            "UPDATE persons SET file_count = (SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id = ?1) WHERE id = ?1",
            rusqlite::params![dst],
        )?;
        tx.execute(
            "UPDATE persons SET representative_face_id = COALESCE( \
                (SELECT fp.id FROM face_prints fp WHERE fp.person_id=?1 AND fp.arcface_embedding IS NOT NULL ORDER BY COALESCE(fp.face_quality,0) DESC,fp.id LIMIT 1), \
                (SELECT fp.id FROM face_prints fp WHERE fp.person_id=?1 ORDER BY COALESCE(fp.face_quality,0) DESC,fp.id LIMIT 1),representative_face_id) WHERE id=?1",
            rusqlite::params![dst],
        )?;
        tx.commit()?;
        Ok(BulkActionResult {
            action: "mergeClusters".into(),
            succeeded: 1,
            failed: 0,
            messages: vec![BulkActionItem {
                file_id: None,
                ok: true,
                message: Some(format!("moved {moved} face prints from #{src} into #{dst}")),
            }],
        })
    })
    .await;

    emit_bulk_result(&sink, "mergeClusters", result).await;
}

pub(crate) async fn emit_bulk_result(
    sink: &Sink,
    action: &str,
    result: Result<anyhow::Result<BulkActionResult>, tokio::task::JoinError>,
) {
    match result {
        Ok(Ok(r)) => {
            sink.send(IpcEvent::now(EventPayload::BulkActionResult(Wrap::new(r))))
                .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, action, "bulk action failed");
            sink.send(IpcEvent::now(EventPayload::BulkActionResult(Wrap::new(
                BulkActionResult {
                    action: action.into(),
                    succeeded: 0,
                    failed: 1,
                    messages: vec![BulkActionItem {
                        file_id: None,
                        ok: false,
                        message: Some(format!("{err}")),
                    }],
                },
            ))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, action, "bulk action spawn_blocking failed");
            sink.send(IpcEvent::now(EventPayload::BulkActionResult(Wrap::new(
                BulkActionResult {
                    action: action.into(), succeeded: 0, failed: 1,
                    messages: vec![BulkActionItem { file_id: None, ok: false,
                        message: Some(format!("Operation did not complete: {err}")) }],
                },
            )))).await;
        }
    }
}

/// Save the structured-name fields (title/first/middle/last/suffix) for a
/// person cluster through the engine's single-writer connection.
fn update_person_name(
    tx: &rusqlite::Transaction<'_>,
    payload: &ipc::RenamePersonPayload,
) -> anyhow::Result<(Option<String>, usize)> {
    let title = payload.title.as_deref().filter(|s| !s.trim().is_empty());
    let first = payload.first_name.as_deref().filter(|s| !s.trim().is_empty());
    let middle = payload.middle_name.as_deref().filter(|s| !s.trim().is_empty());
    let last = payload.last_name.as_deref().filter(|s| !s.trim().is_empty());
    let suffix = payload.suffix.as_deref().filter(|s| !s.trim().is_empty());
    let display = match (first, last) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f.to_string()),
        (None, Some(l)) => Some(l.to_string()),
        _ => None,
    };
    let changed = tx.execute(
        "UPDATE persons SET title=?1, first_name=?2, middle_name=?3, last_name=?4, suffix=?5, name=?6, is_unknown=0 WHERE id=?7",
        rusqlite::params![title, first, middle, last, suffix, display, payload.person_id],
    )?;
    Ok((display, changed))
}

pub(crate) async fn handle_rename_person(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RenamePersonPayload,
) {
    let result = tokio::task::spawn_blocking(move || {
        let conn = db.lock();
        save_person_name(&conn, &payload)
    })
    .await;

    emit_bulk_result(&sink, "renamePerson", result).await;
}

fn save_person_name(
    conn: &rusqlite::Connection,
    payload: &ipc::RenamePersonPayload,
) -> anyhow::Result<BulkActionResult> {
    let tx = conn.unchecked_transaction()?;
    let title = payload.title.as_deref().filter(|s| !s.trim().is_empty());
    let first = payload.first_name.as_deref().filter(|s| !s.trim().is_empty());
    let middle = payload.middle_name.as_deref().filter(|s| !s.trim().is_empty());
    let last = payload.last_name.as_deref().filter(|s| !s.trim().is_empty());
    let suffix = payload.suffix.as_deref().filter(|s| !s.trim().is_empty());
    let display = match (first, last) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f.to_string()),
        (None, Some(l)) => Some(l.to_string()),
        _ => None,
    };
    let affected = tx.execute(
        "UPDATE persons SET title=?1, first_name=?2, middle_name=?3, last_name=?4, suffix=?5, name=COALESCE(?6, name), is_unknown=0 WHERE id=?7",
        rusqlite::params![title, first, middle, last, suffix, display, payload.person_id],
    )?;
    tx.commit()?;
    Ok(BulkActionResult {
        action: "renamePerson".into(),
        succeeded: u32::from(affected != 0),
        failed: u32::from(affected == 0),
        messages: vec![BulkActionItem {
            file_id: Some(payload.person_id),
            ok: affected != 0,
            message: if affected == 0 {
                Some("Person no longer exists.".into())
            } else {
                display
            },
        }],
    })
}

/// FEAT-CRIT-1: bulk "Mark as unknown" for multi-select people view. Sets
/// persons.is_unknown = 1 for every id in the payload + clears the display
/// name (so a previously-named cluster becomes anonymous when the user
/// reverses an assignment).
pub(crate) async fn handle_mark_persons_as_unknown(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::MarkPersonsAsUnknownPayload,
) {
    let result = tokio::task::spawn_blocking(move || {
        let conn = db.lock();
        mark_persons_unknown(&conn, &payload)
    })
    .await;

    emit_bulk_result(&sink, "markPersonsAsUnknown", result).await;
}

fn mark_persons_unknown(
    conn: &rusqlite::Connection,
    payload: &ipc::MarkPersonsAsUnknownPayload,
) -> anyhow::Result<BulkActionResult> {
    let tx = conn.unchecked_transaction()?;
    let mut succeeded = 0u32;
    let mut failed = 0u32;
    let mut messages = Vec::new();
    for id in &payload.person_ids {
        match tx.execute(
            // R4-05: clear EVERY name-bearing column (name + all five
            // structured fields), not just name/first/last — otherwise a
            // title/middle_name/suffix survives, the re-cluster snapshot
            // carries a stale partial identity, and the editor pre-fills it.
            "UPDATE persons SET is_unknown = 1, name = NULL, title = NULL, first_name = NULL, middle_name = NULL, last_name = NULL, suffix = NULL WHERE id = ?1",
            rusqlite::params![id],
        ) {
            Ok(0) => {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*id),
                    ok: false,
                    message: Some("Person no longer exists.".into()),
                });
            }
            Ok(_) => {
                succeeded += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*id),
                    ok: true,
                    message: None,
                });
            }
            Err(e) => {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*id),
                    ok: false,
                    message: Some(e.to_string()),
                });
            }
        }
    }
    tx.commit()?;
    Ok(BulkActionResult {
        action: "markPersonsAsUnknown".into(),
        succeeded,
        failed,
        messages,
    })
}

/// Record a user "different people" verdict for a suggested pair. Persists into
/// face_verifications keyed on BOTH the person pair (PK, for compat + the VLM
/// path) and the stable (min,max) anchor face_print pair (v13), so
/// findMergeSuggestions keeps suppressing the pair across re-clustering. Routed
/// here so the write goes through the engine's single-writer connection rather
/// than a second app-side writer. Fire-and-forget: emits an Error event only on
/// failure; the app updates its status text optimistically.
pub(crate) async fn handle_mark_persons_different(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::MarkPersonsDifferentPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let (pa, pb) = if payload.source_person_id <= payload.destination_person_id {
            (payload.source_person_id, payload.destination_person_id)
        } else {
            (payload.destination_person_id, payload.source_person_id)
        };
        let (fa, fb) = if payload.source_anchor_face_id <= payload.destination_anchor_face_id {
            (payload.source_anchor_face_id, payload.destination_anchor_face_id)
        } else {
            (payload.destination_anchor_face_id, payload.source_anchor_face_id)
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let conn = db.lock();
        // R3-15: resolve the churn-stable (file_id, bbox) key for each anchor face
        // so the verdict survives a faces_evaluated re-scan that DELETE+re-INSERTs
        // face_print ids. NULL when the face row is somehow already gone — the
        // apply path then falls back to the legacy face_a/face_b id.
        let stable_key = |id: i64| -> (Option<i64>, Option<String>) {
            conn.query_row(
                "SELECT file_id, bbox FROM face_prints WHERE id = ?1",
                [id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .map(|(f, b)| (Some(f), Some(b)))
            .unwrap_or((None, None))
        };
        let (file_a, bbox_a) = stable_key(fa);
        let (file_b, bbox_b) = stable_key(fb);
        conn.execute(
            "INSERT OR REPLACE INTO face_verifications
                (person_a, person_b, same_person, confidence, vlm_model, verified_at,
                 face_a, face_b, file_a, bbox_a, file_b, bbox_b)
             VALUES (?1, ?2, 0, 1.0, 'user-verified', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![pa, pb, now, fa, fb, file_a, bbox_a, file_b, bbox_b],
        )?;
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!(?err, "mark_persons_different failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "mark_persons_different_failed".into(),
                message: format!("Mark different failed: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "mark_persons_different spawn failed");
        }
    }
}

/// Find merge-candidate cluster pairs by ArcFace cosine similarity in the
/// suggestion band (MERGE_SUGGEST_COS_LOW..MERGE_SUGGEST_COS_HIGH from
/// face_clustering — 0.55..0.97, distinct from the clusterer's own VLM-verify
/// band). The floor drops impostor-territory noise; the ceiling surfaces the
/// genuine same-person fragments that over-split stranded above the Pass-1
/// threshold. Pairs already confirmed-different in face_verifications are
/// filtered out so the suggested-merges sheet doesn't keep re-prompting.
pub(crate) async fn handle_find_merge_suggestions(
    sink: Sink,
    db_path: std::path::PathBuf,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<MergeSuggestions> {
        // Read-only connection so this never contends on the single writer mutex
        // (clustering can hold it for seconds on a large over-split library).
        let conn = crate::db::open_read(&db_path)?;
        // One row per person via a JOIN to the representative face (its anchor
        // embedding + id) plus a COUNT JOIN for member size — replaces the two
        // per-person correlated subqueries the old query ran. representative_
        // face_id is the cluster anchor (highest-quality embedded face), kept
        // current by clustering + handle_merge_clusters.
        // Scope the prepared statement so its borrow of `conn` ends here,
        // letting the writer lock be released before the cosine sweep below.
        let rows: Vec<(i64, i64, i64, Vec<u8>)> = {
            let mut stmt = conn.prepare(
                "SELECT p.id,
                        COALESCE(p.representative_face_id, (SELECT fp2.id FROM face_prints fp2 WHERE fp2.person_id = p.id AND fp2.arcface_embedding IS NOT NULL ORDER BY COALESCE(fp2.face_quality, 0) DESC LIMIT 1)) AS anchor_id,
                        COUNT(fpc.id),
                        rep.arcface_embedding
                 FROM persons p
                 JOIN face_prints rep ON rep.id = COALESCE(p.representative_face_id, (SELECT fp2.id FROM face_prints fp2 WHERE fp2.person_id = p.id AND fp2.arcface_embedding IS NOT NULL ORDER BY COALESCE(fp2.face_quality, 0) DESC LIMIT 1))
                 JOIN face_prints fpc ON fpc.person_id = p.id
                 WHERE COALESCE(p.is_unknown, 0) = 0
                 GROUP BY p.id",
            )?;
            // Bind to a local so the borrowing iterator temporary is dropped at
            // this `;` — before `stmt` — letting the block return an owned Vec.
            let collected: Vec<(i64, i64, i64, Vec<u8>)> = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Vec<u8>>(3).unwrap_or_default(),
                    ))
                })?
                .filter_map(|r| r.ok())
                .filter(|(_, _, _, blob)| !blob.is_empty() && blob.len() % 4 == 0)
                .collect();
            collected
        };

        let decode = |blob: &[u8]| -> Vec<f32> {
            blob.as_chunks::<4>().0.iter()
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        };
        // Length guard: a dimension mismatch must never masquerade as a
        // near-merge. zip() silently truncates to the shorter slice, inflating
        // the dot product; returning -1.0 is safely excluded by the
        // MERGE_SUGGEST_COS_LOW band check below so a mismatched pair is never
        // suggested (#17).
        let cos = |a: &[f32], b: &[f32]| -> f32 {
            if a.len() != b.len() {
                return -1.0;
            }
            a.iter().zip(b).map(|(x, y)| x * y).sum()
        };

        // "Different people" verdicts. Person-keyed pairs cover legacy rows;
        // face-anchor-keyed pairs (v13) survive re-clustering because
        // face_prints ids are stable. A candidate is suppressed if ANY key
        // matches (legacy person pair, exact-anchor face pair, or the
        // current-membership person pair derived below).
        let mut verified_persons: std::collections::HashSet<(i64, i64)> =
            std::collections::HashSet::new();
        let mut verified_faces: std::collections::HashSet<(i64, i64)> =
            std::collections::HashSet::new();
        // Stored verified face pairs, retained so the verdict can be re-projected
        // onto CURRENT cluster membership below. The anchor-keyed `verified_faces`
        // set only matches when the stored faces are still the live anchors, but
        // anchor selection (highest-quality embedded face) changes under
        // re-clustering — so a "different people" verdict could resurface as a
        // suggestion even though both verified faces still belong to the same two
        // clusters. Re-deriving the person pair from current membership closes
        // that gap without a schema change.
        let mut verified_face_pairs: Vec<(i64, i64)> = Vec::new();
        {
            // Propagate (not `.ok()`-swallow) any failure loading the user's
            // "different people" verdicts: silently dropping them left
            // `verified_persons` empty so the suppression below never fired and
            // already-rejected pairs resurfaced as suggestions — a silent
            // correctness loss. Failing visibly is correct: a broken verdicts
            // read means the suggestion set can't be trusted. (audit F-A2)
            let mut vstmt = conn.prepare(
                "SELECT person_a, person_b, face_a, face_b, file_a, bbox_a, file_b, bbox_b \
                 FROM face_verifications WHERE same_person = 0",
            )?;
            let verdicts = vstmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            // R3-15: resolve each anchor by its churn-stable (file_id, bbox) key to
            // the face id that CURRENTLY occupies that slot (legacy face-id fallback),
            // so a verdict still suppresses the re-prompt after a faces_evaluated
            // re-scan churns face_print ids — mirroring the clustering-apply path.
            let resolve = |legacy: Option<i64>, file: Option<i64>, bbox: Option<String>| -> Option<i64> {
                if let (Some(f), Some(b)) = (file, bbox) {
                    if let Ok(id) = conn.query_row(
                        "SELECT id FROM face_prints WHERE file_id = ?1 AND bbox = ?2 LIMIT 1",
                        rusqlite::params![f, b],
                        |r| r.get::<_, i64>(0),
                    ) {
                        return Some(id);
                    }
                }
                match legacy {
                    Some(l)
                        if conn
                            .query_row("SELECT 1 FROM face_prints WHERE id = ?1", [l], |_| Ok(()))
                            .is_ok() =>
                    {
                        Some(l)
                    }
                    _ => None,
                }
            };
            for (pa, pb, fa, fb, file_a, bbox_a, file_b, bbox_b) in verdicts {
                let pk = if pa < pb { (pa, pb) } else { (pb, pa) };
                verified_persons.insert(pk);
                if let (Some(rfa), Some(rfb)) =
                    (resolve(fa, file_a, bbox_a), resolve(fb, file_b, bbox_b))
                {
                    let fk = if rfa < rfb { (rfa, rfb) } else { (rfb, rfa) };
                    verified_faces.insert(fk);
                    verified_face_pairs.push((rfa, rfb));
                }
            }
        }

        // Re-project each stored face pair onto the person it CURRENTLY belongs
        // to and suppress that (min,max) person pair. Only the verified faces are
        // looked up (bounded by the verdict count), not the whole table.
        let mut verified_membership_persons: std::collections::HashSet<(i64, i64)> =
            std::collections::HashSet::new();
        if !verified_face_pairs.is_empty() {
            let mut face_person: std::collections::HashMap<i64, i64> =
                std::collections::HashMap::new();
            if let Ok(mut fpstmt) =
                conn.prepare("SELECT person_id FROM face_prints WHERE id = ?1")
            {
                for &(fa, fb) in &verified_face_pairs {
                    for fid in [fa, fb] {
                        if let std::collections::hash_map::Entry::Vacant(slot) =
                            face_person.entry(fid)
                        {
                            if let Ok(Some(pid)) = fpstmt.query_row(
                                rusqlite::params![fid],
                                |r| r.get::<_, Option<i64>>(0),
                            ) {
                                slot.insert(pid);
                            }
                        }
                    }
                }
            }
            for (fa, fb) in verified_face_pairs {
                if let (Some(&pa), Some(&pb)) = (face_person.get(&fa), face_person.get(&fb)) {
                    if pa != pb {
                        let pk = if pa < pb { (pa, pb) } else { (pb, pa) };
                        verified_membership_persons.insert(pk);
                    }
                }
            }
        }

        let embeddings: Vec<(i64, i64, i64, Vec<f32>)> = rows
            .into_iter()
            .map(|(pid, anchor_id, count, blob)| (pid, anchor_id, count, decode(&blob)))
            .collect();

        // Every DB read is done; the O(P²) cosine sweep below is pure in-memory
        // math. Release the single-writer lock so the (potentially multi-second
        // on a large over-split library) sweep doesn't serialize other writes.
        drop(conn);

        let mut pairs: Vec<MergeSuggestion> = Vec::new();
        for i in 0..embeddings.len() {
            for j in (i + 1)..embeddings.len() {
                let (pa, anchor_a, count_a, ref ea) = embeddings[i];
                let (pb, anchor_b, count_b, ref eb) = embeddings[j];
                let pk = if pa < pb { (pa, pb) } else { (pb, pa) };
                let fk = if anchor_a < anchor_b {
                    (anchor_a, anchor_b)
                } else {
                    (anchor_b, anchor_a)
                };
                if verified_persons.contains(&pk)
                    || verified_faces.contains(&fk)
                    || verified_membership_persons.contains(&pk)
                {
                    continue;
                }
                let s = cos(ea, eb);
                if s >= MERGE_SUGGEST_COS_LOW && s < MERGE_SUGGEST_COS_HIGH {
                    pairs.push(MergeSuggestion {
                        source_person_id: pa,
                        destination_person_id: pb,
                        similarity: s,
                        source_anchor_face_id: anchor_a,
                        destination_anchor_face_id: anchor_b,
                        source_member_count: count_a,
                        destination_member_count: count_b,
                    });
                }
            }
        }
        pairs.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if pairs.len() > 50 {
            pairs.truncate(50);
        }

        Ok(MergeSuggestions { pairs })
    })
    .await;

    match result {
        Ok(Ok(s)) => {
            sink.send(IpcEvent::now(EventPayload::MergeSuggestions(Wrap::new(s))))
                .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "find_merge_suggestions failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "find_merge_suggestions_failed".into(),
                message: format!("Find merge suggestions failed: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "find_merge_suggestions spawn failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renaming_unknown_person_marks_them_known_and_clears_stale_name_on_blank() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE persons (
                id INTEGER PRIMARY KEY,
                title TEXT,
                first_name TEXT,
                middle_name TEXT,
                last_name TEXT,
                suffix TEXT,
                name TEXT,
                is_unknown INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO persons (id, name, is_unknown) VALUES (1, NULL, 1);",
        )
        .unwrap();

        let tx = conn.unchecked_transaction().unwrap();
        let (display, changed) = update_person_name(
            &tx,
            &ipc::RenamePersonPayload {
                person_id: 1,
                title: None,
                first_name: Some("Ada".into()),
                middle_name: None,
                last_name: Some("Lovelace".into()),
                suffix: None,
            },
        )
        .unwrap();
        assert_eq!(display.as_deref(), Some("Ada Lovelace"));
        assert_eq!(changed, 1);
        tx.commit().unwrap();
        assert_eq!(
            conn.query_row("SELECT name FROM persons WHERE id=1", [], |row| row.get::<_, Option<String>>(0))
                .unwrap()
                .as_deref(),
            Some("Ada Lovelace")
        );
        assert_eq!(
            conn.query_row("SELECT is_unknown FROM persons WHERE id=1", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );

        let tx = conn.unchecked_transaction().unwrap();
        let (display, changed) = update_person_name(
            &tx,
            &ipc::RenamePersonPayload {
                person_id: 1,
                title: None,
                first_name: None,
                middle_name: None,
                last_name: None,
                suffix: None,
            },
        )
        .unwrap();
        assert_eq!(display, None);
        assert_eq!(changed, 1);
        tx.commit().unwrap();
        assert_eq!(
            conn.query_row("SELECT name FROM persons WHERE id=1", [], |row| row.get::<_, Option<String>>(0))
                .unwrap(),
            None
        );
    }

    fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("fileid-bulk-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[test]
    fn naming_unknown_person_restores_known_cluster() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute(
            "INSERT INTO persons (id, created_at, is_unknown) VALUES (42, 1, 1)",
            [],
        ).unwrap();
        let result = save_person_name(&conn, &ipc::RenamePersonPayload {
            person_id: 42,
            title: None,
            first_name: Some("Kira".into()),
            middle_name: None,
            last_name: Some("Park".into()),
            suffix: None,
        }).unwrap();
        assert_eq!((result.succeeded, result.failed), (1, 0));
        let (name, is_unknown): (String, i64) = conn
            .query_row("SELECT name,is_unknown FROM persons WHERE id=42", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(name, "Kira Park");
        assert_eq!(is_unknown, 0);
    }

    #[test]
    fn removed_person_rename_reports_failure() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute("INSERT INTO persons (id, created_at) VALUES (42, 1)", []).unwrap();
        conn.execute("DELETE FROM persons WHERE id=42", []).unwrap();
        let result = save_person_name(&conn, &ipc::RenamePersonPayload {
            person_id: 42,
            title: None,
            first_name: Some("Kira".into()),
            middle_name: None,
            last_name: None,
            suffix: None,
        }).unwrap();
        assert_eq!((result.succeeded, result.failed), (0, 1));
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].file_id, Some(42));
        assert!(!result.messages[0].ok);
        assert!(result.messages[0].message.as_deref().unwrap().contains("no longer exists"));
    }

    #[test]
    fn removed_person_unknown_batch_reports_failures_and_saves_survivors() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO persons (id, name, first_name, created_at) VALUES
             (1, 'Kira', 'Kira', 1), (2, 'Maya', 'Maya', 1);
             DELETE FROM persons WHERE id=2;"
        ).unwrap();
        let result = mark_persons_unknown(
            &conn,
            &ipc::MarkPersonsAsUnknownPayload { person_ids: vec![1, 2, 999] },
        ).unwrap();
        assert_eq!((result.succeeded, result.failed), (1, 2));
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.messages[0].file_id, Some(1));
        assert!(result.messages[0].ok);
        for (item, id) in result.messages[1..].iter().zip([2, 999]) {
            assert_eq!(item.file_id, Some(id));
            assert!(!item.ok);
            assert!(item.message.as_deref().unwrap().contains("no longer exists"));
        }
        let survivor: (i64, Option<String>, Option<String>) = conn
            .query_row("SELECT is_unknown, name, first_name FROM persons WHERE id=1", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            }).unwrap();
        assert_eq!(survivor, (1, None, None));
    }

    #[tokio::test]
    async fn merge_delete_failure_keeps_names_and_assignments() -> anyhow::Result<()> {
        let root = unique_temp_dir("merge-rollback");
        let conn = crate::db::open_writer(&root.join("catalog.sqlite"))?;
        conn.execute_batch("INSERT INTO persons(id,created_at,is_unknown) VALUES(1,123,1); INSERT INTO persons(id,first_name,created_at) VALUES(2,'Alex',123); INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,'/offline/photo.jpg',1,1,0,'image','jpg'); INSERT INTO face_prints(id,file_id,person_id,print_data,bbox) VALUES(1,1,2,X'00','[0,0,1,1]'); CREATE TRIGGER fixture_merge_failure BEFORE DELETE ON persons WHEN OLD.id=2 BEGIN SELECT RAISE(ABORT,'fixture failure'); END;")?;
        let db = std::sync::Arc::new(parking_lot::Mutex::new(conn));
        let (sink,mut rx) = Sink::channel_for_test(4);
        handle_merge_clusters(sink,db.clone(),ipc::MergeClustersPayload { source_person_id:2,destination_person_id:1 }).await;
        let event = rx.recv().await.ok_or_else(||anyhow::anyhow!("missing merge result"))?;
        if let ipc::EventPayload::BulkActionResult(result) = event.payload {
            assert_eq!(result.inner.failed,1);
            assert_eq!(result.inner.succeeded,0);
        } else { anyhow::bail!("wrong merge event"); }
        {
            let conn = db.lock();
            assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=1",[],|r|r.get::<_,i64>(0))?,2);
            assert_eq!(conn.query_row("SELECT first_name,is_unknown FROM persons WHERE id=1",[],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,i64>(1)?)))?,(None,1));
            assert_eq!(conn.query_row("SELECT first_name FROM persons WHERE id=2",[],|r|r.get::<_,String>(0))?,"Alex");
        }
        drop(db);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[tokio::test]
    async fn worker_failure_emits_a_failed_completion() -> anyhow::Result<()> {
        let (sink,mut rx) = Sink::channel_for_test(4);
        let result = tokio::task::spawn_blocking(|| -> anyhow::Result<BulkActionResult> { panic!("fixture worker failure") }).await;
        emit_bulk_result(&sink,"mergeClusters",result).await;
        let event = rx.recv().await.ok_or_else(||anyhow::anyhow!("missing worker failure result"))?;
        if let ipc::EventPayload::BulkActionResult(result) = event.payload {
            assert_eq!(result.inner.failed,1);
            assert_eq!(result.inner.succeeded,0);
            assert!(result.inner.messages.iter().any(|m|!m.ok));
        } else { anyhow::bail!("wrong worker failure event"); }
        Ok(())
    }

    // C1-012: the recovery line carries the file_id + src + dst so disk vs DB
    // can be reconciled. Pure wire-shape check (no filesystem).
    #[test]
    fn rename_recovery_line_carries_id_src_dst() {
        let line = rename_recovery_line(42, r"C:\a\old.jpg", r"C:\a\new.jpg");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["file_id"], 42);
        assert_eq!(v["src"], r"C:\a\old.jpg");
        assert_eq!(v["dst"], r"C:\a\new.jpg");
    }

    // C1-012: a commit-failure path records EVERY on-disk move to the recovery
    // sidecar (NDJSON, append-only). Before the fix there was no sidecar at all,
    // so a failed end-of-batch commit left the whole batch silently desynced.
    #[test]
    fn commit_failure_writes_recovery_sidecar_for_every_move() {
        let dir = unique_temp_dir("recover");
        // Simulate the commit-failure reconciliation loop: write one line per
        // on-disk move that the rolled-back transaction left stale.
        let moves = [
            (1i64, r"C:\lib\a-old.jpg".to_string(), r"C:\lib\a-new.jpg".to_string()),
            (2i64, r"C:\lib\b-old.png".to_string(), r"C:\lib\b-new.png".to_string()),
        ];
        for (fid, src, dst) in &moves {
            write_rename_recovery_line(&dir, &rename_recovery_line(*fid, src, dst));
        }

        let sidecar = dir.join("rename_recover.ndjson");
        assert!(sidecar.exists(), "recovery sidecar must be written");
        let contents = std::fs::read_to_string(&sidecar).unwrap();
        let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "one recovery line per on-disk move");

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["file_id"], 1);
        assert_eq!(first["dst"], r"C:\lib\a-new.jpg");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["file_id"], 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    // C1-012: a second write appends rather than truncating (NDJSON growth).
    #[test]
    fn recovery_sidecar_appends() {
        let dir = unique_temp_dir("append");
        write_rename_recovery_line(&dir, &rename_recovery_line(1, "a", "b"));
        write_rename_recovery_line(&dir, &rename_recovery_line(2, "c", "d"));
        let contents = std::fs::read_to_string(dir.join("rename_recover.ndjson")).unwrap();
        assert_eq!(contents.lines().filter(|l| !l.is_empty()).count(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn exact_trash_rejects_changed_bytes_and_missing_keeper() {
        let dir = unique_temp_dir("exact-trash");
        let keeper = dir.join("keeper.bin");
        let victim = dir.join("victim.bin");
        std::fs::write(&keeper, b"same").unwrap();
        std::fs::write(&victim, b"same").unwrap();
        let digest = hex::encode(crate::util::content_hash::exact_file_sha256(&keeper, 4).unwrap());
        let identity = ipc::ExactTrashIdentity {
            file_id: 2,
            path: victim.to_string_lossy().into_owned(),
            size_bytes: 4,
            sha256_hex: digest.clone(),
            keeper_path: keeper.to_string_lossy().into_owned(),
            keeper_size_bytes: 4,
            keeper_sha256_hex: digest,
        };
        let check = || exact_trash_identity_valid(&identity, 2, &victim, 4,
            |path, size| crate::util::content_hash::exact_file_sha256(path, size).ok());
        assert!(check());
        std::fs::write(&victim, b"diff").unwrap();
        assert!(!check());
        std::fs::write(&victim, b"same").unwrap();
        std::fs::remove_file(&keeper).unwrap();
        assert!(!check());
        std::fs::remove_dir_all(dir).unwrap();
    }

}
