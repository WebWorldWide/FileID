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

fn no_clobber_rename(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::io::Result<()> {
    crate::util::rename_no_replace(src, dst)
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
        for (path, tags) in &sidecar_writes {
            if let Err(err) = crate::shell::tags::write_tags(std::path::Path::new(path), tags) {
                tracing::warn!(?err, path = %crate::platform::redact_path_for_log(path), "sidecar tag write failed");
            }
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

        // Phase 1 — resolve + validate every rename under a short writer-lock
        // scope (read-only SELECTs). Collect the moves so the per-file
        // filesystem work below runs with NO lock held: a large batch must not
        // wedge the engine's only writer (and any concurrent scan flush) across
        // every MoveFileExW — the pathology the apply-tags P0 fix removed.
        // (audit 2026-07-08)
        struct PlannedRename {
            file_id: i64,
            src: PathBuf,
            dest: PathBuf,
        }
        let mut planned: Vec<PlannedRename> = Vec::with_capacity(payload.renames.len());
        {
            let conn = db.lock();
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
                let path: Result<String, _> = conn.query_row(
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
                planned.push(PlannedRename {
                    file_id: entry.file_id,
                    src: path,
                    dest,
                });
            }
        }

        // Phase 2 — perform the filesystem moves with NO writer lock held. Each
        // move is a no-clobber MoveFileExW (no MOVEFILE_REPLACE_EXISTING): the
        // destination existence is re-checked by the kernel inside the move
        // itself, so a separate probe + rename would be a TOCTOU — an occupied
        // destination fails the move (failed++) rather than being silently
        // overwritten. The un-prefixed dest is used for DB path_text + user
        // messages so stored paths stay normal-form (#29). Mirrors
        // restructure_apply.rs::move_file (B3).
        // C1-012: every move that landed on disk is tracked so a failed per-row
        // UPDATE or a failed end-of-batch commit (which rolls back ALL the
        // per-move DB UPDATEs) can be reconciled via the recovery sidecar.
        let mut on_disk_moves: Vec<(i64, String, String)> = Vec::new();
        for p in &planned {
            if let Err(err) = no_clobber_rename(&p.src, &p.dest) {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(p.file_id),
                    ok: false,
                    message: Some(format!("rename failed: {err}")),
                });
                continue;
            }
            // Move the on-disk tags sidecar to follow the renamed file (#27).
            // Best-effort: a missing sidecar (the common case) or any error is
            // ignored so it never turns a successful rename into a failure.
            crate::shell::tags::move_sidecar(&p.src, &p.dest);
            on_disk_moves.push((
                p.file_id,
                p.src.to_string_lossy().to_string(),
                p.dest.to_string_lossy().to_string(),
            ));
        }

        // Phase 3 — persist the DB row updates for the moves that landed on
        // disk, in one short transaction under the writer lock.
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        for (file_id, src_text, dest_text) in &on_disk_moves {
            // ENG-91: keep path_hash in sync with path_text (lookups/dedup key
            // on it). ENG-92: do NOT swallow the UPDATE error and still claim
            // success — a file renamed on disk but with a failed DB write must
            // be reported as failed (the next scan's rename-heal rebinds it via
            // content_hash/file_ref).
            let dest_hash = crate::util::path_safety::stable_path_hash(dest_text);
            match tx.execute(
                // path_search NFC-normalized (not verbatim ?1) so an NFD-accented
                // renamed/moved file stays findable by its accented name. (audit parity)
                // OR ABORT is load-bearing: `path_text` is UNIQUE ON CONFLICT REPLACE,
                // so a PLAIN update colliding with a LIVE row already at dest (a
                // check→rename TOCTOU on non-Windows, or an external desync) would
                // silently REPLACE-delete that row + cascade its user metadata. OR
                // ABORT raises instead, routing into the Err arm below (record +
                // report failed) rather than losing data. (audit 2026-07 sibling)
                "UPDATE OR ABORT files SET path_text = ?1, path_hash = ?2, path_search = ?4 WHERE id = ?3",
                rusqlite::params![
                    dest_text,
                    dest_hash,
                    file_id,
                    crate::pipeline::dbwriter::nfc_path_search(dest_text)
                ],
            ) {
                Ok(1) => {
                    succeeded += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(*file_id),
                        ok: true,
                        message: Some(dest_text.clone()),
                    });
                }
                Ok(changed) => {
                    record_rename_recovery(*file_id, src_text, dest_text);
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(*file_id),
                        ok: false,
                        message: Some(format!(
                            "renamed on disk but DB update affected {changed} rows (expected 1)"
                        )),
                    });
                }
                Err(err) => {
                    // C1-012: file is renamed on disk but its row update failed.
                    // Record it to the recovery sidecar so the disk/DB desync is
                    // reconcilable even if the next scan never runs.
                    record_rename_recovery(*file_id, src_text, dest_text);
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(*file_id),
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

#[derive(Clone)]
struct TrashCandidate {
    file_id: i64,
    path: PathBuf,
    staging_path: PathBuf,
    indexed_size: u64,
    indexed_file_ref: Option<i64>,
    indexed_content_hash: Option<Vec<u8>>,
    source_identity: crate::platform::FileIdentity,
    expected_exact: Option<ipc::ExactTrashIdentity>,
}

#[derive(Debug)]
enum CheckedTrashOutcome {
    Trashed,
    Rejected(String),
    Failed(String),
}

#[cfg(test)]
fn exact_hash_at_stable_path_with(
    path: &std::path::Path,
    expected_size: u64,
    hash_file: impl FnOnce(
        &std::path::Path,
        u64,
    ) -> Result<([u8; 32], crate::platform::FileIdentity), String>,
) -> Result<[u8; 32], String> {
    let probe = crate::util::path_safety::to_extended_length(path);
    let before_metadata = std::fs::symlink_metadata(&probe)
        .map_err(|error| format!("exact file is missing or unreadable: {error}"))?;
    if !before_metadata.file_type().is_file() || before_metadata.len() != expected_size {
        return Err("exact file type or size changed before hashing".into());
    }
    let before_identity = crate::platform::file_identity(path)
        .ok_or_else(|| "could not capture exact file identity before hashing".to_string())?;
    let (hash, handle_identity) = hash_file(path, expected_size)?;
    let after_metadata = std::fs::symlink_metadata(&probe)
        .map_err(|error| format!("exact file disappeared after hashing: {error}"))?;
    let after_identity = crate::platform::file_identity(path)
        .ok_or_else(|| "could not recapture exact file identity after hashing".to_string())?;
    if !after_metadata.file_type().is_file()
        || after_metadata.len() != expected_size
        || handle_identity != before_identity
        || after_identity != before_identity
    {
        return Err("exact file path identity changed during hashing".into());
    }
    Ok(hash)
}

fn exact_hash_guard_at_stable_path(
    path: &std::path::Path,
    expected_size: u64,
    lock: crate::util::content_hash::ExactFileLock,
) -> Result<crate::util::content_hash::ExactFileHash, String> {
    let probe = crate::util::path_safety::to_extended_length(path);
    let before_metadata = std::fs::symlink_metadata(&probe)
        .map_err(|error| format!("exact file is missing or unreadable: {error}"))?;
    if !before_metadata.file_type().is_file() || before_metadata.len() != expected_size {
        return Err("exact file type or size changed before hashing".into());
    }
    let before_identity = crate::platform::file_identity(path)
        .ok_or_else(|| "could not capture exact file identity before hashing".to_string())?;
    let proof = crate::util::content_hash::exact_file_sha256_guard(path, expected_size, lock)
    .map_err(|error| format!("could not hash exact file contents: {error}"))?;
    let after_metadata = std::fs::symlink_metadata(&probe)
        .map_err(|error| format!("exact file disappeared after hashing: {error}"))?;
    let after_identity = crate::platform::file_identity(path)
        .ok_or_else(|| "could not recapture exact file identity after hashing".to_string())?;
    if !after_metadata.file_type().is_file()
        || after_metadata.len() != expected_size
        || proof.identity != before_identity
        || after_identity != before_identity
    {
        return Err("exact file path identity changed during hashing".into());
    }
    Ok(proof)
}

struct ExactTrashGuards {
    _keeper: crate::util::content_hash::ExactFileHash,
    _victim: crate::util::content_hash::ExactFileHash,
}

fn validate_trash_candidate_at(
    candidate: &TrashCandidate,
    actual_path: &std::path::Path,
    verify_contents: bool,
) -> Result<Option<ExactTrashGuards>, String> {
    if let Some(expected) = &candidate.expected_exact {
        let victim_hash = hex::decode(&expected.sha256_hex)
            .map_err(|_| "exact-cleanup evidence contains an invalid SHA-256".to_string())?;
        let keeper_hash = hex::decode(&expected.keeper_sha256_hex)
            .map_err(|_| "exact-cleanup keeper SHA-256 is invalid".to_string())?;
        if expected.path != candidate.path.to_string_lossy()
            || expected.size_bytes != candidate.indexed_size as i64
            || expected.size_bytes != expected.keeper_size_bytes
            || victim_hash != keeper_hash
        {
            return Err("exact-cleanup evidence does not prove equal victim and keeper bytes".into());
        }
    }
    let probe = crate::util::path_safety::to_extended_length(actual_path);
    let metadata = std::fs::symlink_metadata(&probe)
        .map_err(|error| format!("file is missing or unreadable: {error}"))?;
    if !metadata.is_file() {
        return Err("indexed path no longer names a regular file".into());
    }
    if metadata.len() != candidate.indexed_size {
        return Err("file size changed since it was indexed".into());
    }

    match crate::platform::file_identity(actual_path) {
        Some(current_identity) if current_identity == candidate.source_identity => {}
        Some(_) => return Err("a different file now occupies the claimed path".into()),
        None => return Err("could not revalidate the claimed file identity".into()),
    }

    let current_ref = crate::platform::file_ref(actual_path);
    if let Some(indexed_ref) = candidate.indexed_file_ref {
        match current_ref {
            Some(current_ref) if current_ref == indexed_ref as u64 => {}
            Some(_) => return Err("a different file now occupies the indexed path".into()),
            None => return Err("could not revalidate the indexed file identity".into()),
        }
    }
    if !verify_contents {
        return Ok(None);
    }

    if let Some(expected) = &candidate.expected_exact {
        let keeper_size = u64::try_from(expected.keeper_size_bytes)
            .map_err(|_| "exact-cleanup keeper size is invalid".to_string())?;
        let keeper_expected = hex::decode(&expected.keeper_sha256_hex)
            .map_err(|_| "exact-cleanup keeper SHA-256 is invalid".to_string())?;
        let keeper_actual = exact_hash_guard_at_stable_path(
            std::path::Path::new(&expected.keeper_path),
            keeper_size,
            crate::util::content_hash::ExactFileLock::DenyMutation,
        )
        .map_err(|error| format!("could not revalidate the exact-duplicate keeper: {error}"))?;
        if keeper_actual.hash.as_slice() != keeper_expected.as_slice() {
            return Err("the exact-duplicate keeper changed before Trash".into());
        }
        let expected_hash = hex::decode(&expected.sha256_hex)
            .map_err(|_| "exact-cleanup evidence contains an invalid SHA-256".to_string())?;
        let victim_actual = exact_hash_guard_at_stable_path(
            actual_path,
            candidate.indexed_size,
            crate::util::content_hash::ExactFileLock::DenyWrite,
        )
        .map_err(|error| format!("could not verify exact file contents: {error}"))?;
        if victim_actual.hash.as_slice() != expected_hash.as_slice() {
            return Err("file contents changed after exact-duplicate verification".into());
        }
        return Ok(Some(ExactTrashGuards {
            _keeper: keeper_actual,
            _victim: victim_actual,
        }));
    } else if candidate.indexed_file_ref.is_none() {
        if candidate.indexed_size > crate::util::content_hash::FULL_HASH_MAX_BYTES {
            return Err(
                "the catalog has no stable identity for this large file; rescan before trashing it"
                    .into(),
            );
        }
        let Some(indexed_hash) = candidate.indexed_content_hash.as_deref() else {
            return Err(
                "the catalog has no stable identity for this file; rescan before trashing it"
                    .into(),
            );
        };
        if indexed_hash.len() != 32 {
            return Err("the catalog contains an invalid file identity".into());
        }
        let matches = crate::util::content_hash::matches_known_hash_hex(
            actual_path,
            candidate.indexed_size,
            &hex::encode(indexed_hash),
        )
        .map_err(|error| format!("could not revalidate file contents: {error}"))?;
        if !matches {
            return Err("file contents changed since they were indexed".into());
        }
    }
    Ok(None)
}

fn restore_failed_claim(candidate: &TrashCandidate, reason: String) -> CheckedTrashOutcome {
    match crate::util::rename_no_replace(&candidate.staging_path, &candidate.path) {
        Ok(()) => CheckedTrashOutcome::Rejected(reason),
        Err(error) => CheckedTrashOutcome::Failed(format!(
            "{reason}; the claimed file could not be restored ({error}) and remains at {}",
            candidate.staging_path.display()
        )),
    }
}

fn checked_trash_one_with(
    candidate: &TrashCandidate,
    trash: impl FnOnce(
        &std::path::Path,
        &std::path::Path,
        crate::platform::FileIdentity,
    ) -> anyhow::Result<()>,
) -> CheckedTrashOutcome {
    if let Err(reason) = validate_trash_candidate_at(candidate, &candidate.path, false) {
        return CheckedTrashOutcome::Rejected(reason);
    }
    if let Err(error) = crate::util::rename_no_replace(&candidate.path, &candidate.staging_path) {
        return CheckedTrashOutcome::Failed(format!("could not atomically claim file for Trash: {error}"));
    }
    let _exact_guards = match validate_trash_candidate_at(candidate, &candidate.staging_path, true) {
        Ok(guard) => guard,
        Err(reason) => return restore_failed_claim(candidate, reason),
    };
    match trash(
        &candidate.staging_path,
        &candidate.path,
        candidate.source_identity,
    ) {
        Ok(()) => CheckedTrashOutcome::Trashed,
        Err(error) => match crate::util::rename_no_replace(&candidate.staging_path, &candidate.path) {
            Ok(()) => CheckedTrashOutcome::Failed(error.to_string()),
            Err(restore_error) => CheckedTrashOutcome::Failed(format!(
                "{error}; the failed Trash claim also could not be restored ({restore_error}) and remains at {}",
                candidate.staging_path.display()
            )),
        },
    }
}

fn checked_trash_one(candidate: &TrashCandidate) -> CheckedTrashOutcome {
    checked_trash_one_with(candidate, crate::shell::trash::trash_path_as)
}

fn checked_trash_candidates(candidates: &[TrashCandidate]) -> Vec<CheckedTrashOutcome> {
    if candidates.len() <= 4 {
        return candidates.iter().map(checked_trash_one).collect();
    }

    const POOL_SIZE: usize = 8;
    let n = candidates.len();
    let (input_tx, input_rx) = crossbeam_channel::bounded::<(usize, TrashCandidate)>(n);
    let (output_tx, output_rx) =
        crossbeam_channel::bounded::<(usize, CheckedTrashOutcome)>(n);

    for _ in 0..POOL_SIZE.min(n) {
        let rx = input_rx.clone();
        let tx = output_tx.clone();
        std::thread::spawn(move || {
            while let Ok((index, candidate)) = rx.recv() {
                let _ = tx.send((index, checked_trash_one(&candidate)));
            }
        });
    }
    drop(output_tx);
    for (index, candidate) in candidates.iter().cloned().enumerate() {
        let _ = input_tx.send((index, candidate));
    }
    drop(input_tx);

    let mut outcomes: Vec<Option<CheckedTrashOutcome>> =
        std::iter::repeat_with(|| None).take(n).collect();
    while let Ok((index, outcome)) = output_rx.recv() {
        if let Some(slot) = outcomes.get_mut(index) {
            *slot = Some(outcome);
        }
    }
    outcomes
        .into_iter()
        .map(|outcome| {
            outcome.unwrap_or_else(|| CheckedTrashOutcome::Failed("Trash worker exited".into()))
        })
        .collect()
}

#[cfg(windows)]
fn bind_windows_trash_receipts_with(
    candidates: &[TrashCandidate],
    outcomes: &mut [CheckedTrashOutcome],
    entry: &mut TrashLogEntry,
    locate: impl FnOnce(&[&str]) -> std::collections::HashMap<String, std::path::PathBuf>,
    append: impl FnOnce(&TrashLogEntry) -> anyhow::Result<()>,
) {
    let claims: Vec<&str> = candidates
        .iter()
        .zip(outcomes.iter())
        .filter_map(|(candidate, outcome)| {
            matches!(outcome, CheckedTrashOutcome::Trashed)
                .then(|| candidate.staging_path.to_str())
                .flatten()
        })
        .collect();
    let physical = locate(&claims);
    let mut verified = Vec::new();
    for (index, (candidate, outcome)) in candidates.iter().zip(outcomes.iter_mut()).enumerate() {
        if !matches!(outcome, CheckedTrashOutcome::Trashed) {
            continue;
        }
        let key = crate::util::path_safety::normalize_for_exclusion(&candidate.staging_path);
        let Some(path) = physical.get(&key) else {
            *outcome = CheckedTrashOutcome::Failed(
                "Recycle Bin did not return an identity-bound receipt; the catalog row was retained"
                    .into(),
            );
            continue;
        };
        if crate::platform::file_identity(path) != Some(candidate.source_identity) {
            *outcome = CheckedTrashOutcome::Failed(
                "Recycle Bin receipt identity did not match the claimed file; the catalog row was retained"
                    .into(),
            );
            continue;
        }
        entry.items[index].recycle_physical_path = Some(path.to_string_lossy().into_owned());
        verified.push(index);
    }
    if !verified.is_empty() {
        if let Err(error) = append(entry) {
            for index in verified {
                outcomes[index] = CheckedTrashOutcome::Failed(format!(
                    "file moved to the Recycle Bin, but its identity receipt could not be journaled ({error}); the catalog row was retained"
                ));
                entry.items[index].recycle_physical_path = None;
            }
        }
    }
}

#[cfg(windows)]
fn bind_windows_trash_receipts(
    candidates: &[TrashCandidate],
    outcomes: &mut [CheckedTrashOutcome],
    entry: &mut TrashLogEntry,
) {
    bind_windows_trash_receipts_with(
        candidates,
        outcomes,
        entry,
        super::trash::invoke_windows_restore_batch,
        trash_log::append,
    );
}

#[cfg(not(windows))]
fn bind_windows_trash_receipts(
    _candidates: &[TrashCandidate],
    _outcomes: &mut [CheckedTrashOutcome],
    _entry: &mut TrashLogEntry,
) {
}

fn trash_commit_failure_result(
    batch_id: String,
    succeeded: u32,
    failed: u32,
    mut messages: Vec<BulkActionItem>,
    error: &str,
) -> BulkActionResult {
    for message in messages.iter_mut().filter(|message| message.ok) {
        message.ok = false;
        message.message = Some(format!(
            "moved to Trash, but the catalog commit failed ({error}); use Undo batch {batch_id} to restore"
        ));
    }
    messages.push(BulkActionItem {
        file_id: None,
        ok: false,
        message: Some(format!(
            "catalog commit failed after Trash; recovery batch: {batch_id}"
        )),
    });
    BulkActionResult {
        action: format!("trashFiles:{batch_id}"),
        succeeded: 0,
        failed: failed.saturating_add(succeeded),
        messages,
    }
}

/// Trash indexed files only after immediately revalidating their on-disk identity.
pub(crate) async fn handle_trash_files(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::TrashFilesPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();
        let mut file_ids = payload.file_ids;
        let mut seen_ids = std::collections::HashSet::with_capacity(file_ids.len());
        file_ids.retain(|file_id| seen_ids.insert(*file_id));
        let mut candidates = Vec::with_capacity(file_ids.len());
        let mut exact_by_id = std::collections::HashMap::new();
        for identity in payload.exact_identities.unwrap_or_default() {
            let file_id = identity.file_id;
            if exact_by_id.insert(file_id, identity).is_some() {
                anyhow::bail!("trashFiles contains duplicate exact identity #{file_id}");
            }
        }

        {
            let conn = db.lock();
            for fid in &file_ids {
                match conn.query_row(
                    "SELECT path_text,size_bytes,file_ref,content_hash FROM files WHERE id = ?1",
                    rusqlite::params![fid],
                    |row| {
                        let indexed_size = row.get::<_, i64>(1)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            indexed_size,
                            row.get::<_, Option<i64>>(2)?,
                            row.get::<_, Option<Vec<u8>>>(3)?,
                        ))
                    },
                ) {
                    Ok((path, indexed_size, indexed_file_ref, indexed_content_hash)) => {
                        match u64::try_from(indexed_size) {
                            Ok(indexed_size) => {
                                let path = PathBuf::from(path);
                                let staging_path = path.with_file_name(format!(
                                    ".fileid-trash-{}",
                                    uuid::Uuid::new_v4()
                                ));
                                match crate::platform::file_identity(&path) {
                                    Some(source_identity) => candidates.push(TrashCandidate {
                                        file_id: *fid,
                                        path,
                                        staging_path,
                                        indexed_size,
                                        indexed_file_ref,
                                        indexed_content_hash,
                                        source_identity,
                                        expected_exact: exact_by_id.remove(fid),
                                    }),
                                    None => {
                                        failed += 1;
                                        messages.push(BulkActionItem {
                                            file_id: Some(*fid),
                                            ok: false,
                                            message: Some(
                                                "could not capture a volume-qualified file identity; rescan before trashing"
                                                    .into(),
                                            ),
                                        });
                                    }
                                }
                            }
                            Err(_) => {
                                failed += 1;
                                messages.push(BulkActionItem {
                                    file_id: Some(*fid),
                                    ok: false,
                                    message: Some("catalog contains an invalid file size".into()),
                                });
                            }
                        }
                    }
                    Err(error) => {
                        failed += 1;
                        messages.push(BulkActionItem {
                            file_id: Some(*fid),
                            ok: false,
                            message: Some(format!("not found: {error}")),
                        });
                    }
                }
            }
        }

        let batch_id = uuid::Uuid::new_v4().to_string();
        let mut entry = TrashLogEntry {
            batch_id: batch_id.clone(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs_f64())
                .unwrap_or(0.0),
            items: candidates
                .iter()
                .map(|candidate| TrashLogItem {
                    file_id: candidate.file_id,
                    original_path: candidate.path.to_string_lossy().into_owned(),
                    recycle_bin_id: Some(candidate.staging_path.to_string_lossy().into_owned()),
                    recycle_physical_path: None,
                    source_identity: Some(candidate.source_identity),
                })
                .collect(),
        };
        if !candidates.is_empty() {
            trash_log::append(&entry).map_err(|error| {
                anyhow::anyhow!(
                    "Trash was not started because its recovery journal could not be written: {error}"
                )
            })?;
        }

        let mut outcomes = checked_trash_candidates(&candidates);
        bind_windows_trash_receipts(&candidates, &mut outcomes, &mut entry);
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        for (candidate, outcome) in candidates.iter().zip(outcomes) {
            match outcome {
                CheckedTrashOutcome::Trashed => {
                    let changed = tx.execute(
                        "DELETE FROM files \
                         WHERE id=?1 AND path_text=?2 AND size_bytes=?3 \
                           AND ((file_ref IS NULL AND ?4 IS NULL) OR file_ref=?4) \
                           AND ((content_hash IS NULL AND ?5 IS NULL) OR content_hash=?5)",
                        rusqlite::params![
                            candidate.file_id,
                            candidate.path.to_string_lossy().as_ref(),
                            candidate.indexed_size as i64,
                            candidate.indexed_file_ref,
                            candidate.indexed_content_hash.as_deref(),
                        ],
                    );
                    match changed {
                        Ok(1) => {
                            succeeded += 1;
                            messages.push(BulkActionItem {
                                file_id: Some(candidate.file_id),
                                ok: true,
                                message: Some(candidate.path.to_string_lossy().to_string()),
                            });
                        }
                        Ok(changed) => {
                            failed += 1;
                            messages.push(BulkActionItem {
                                file_id: Some(candidate.file_id),
                                ok: false,
                                message: Some(format!(
                                    "moved to Trash, but the catalog identity changed and the update affected {changed} rows; use Undo to restore"
                                )),
                            });
                        }
                        Err(error) => {
                            failed += 1;
                            messages.push(BulkActionItem {
                                file_id: Some(candidate.file_id),
                                ok: false,
                                message: Some(format!(
                                    "moved to Trash, but the catalog update failed ({error}); use Undo to restore"
                                )),
                            });
                        }
                    }
                }
                CheckedTrashOutcome::Rejected(reason) => {
                    tracing::warn!(
                        file_id = candidate.file_id,
                        path = %crate::platform::redact_path_for_log(&candidate.path),
                        reason,
                        "refusing to trash a stale catalog identity"
                    );
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(candidate.file_id),
                        ok: false,
                        message: Some(reason),
                    });
                }
                CheckedTrashOutcome::Failed(error) => {
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(candidate.file_id),
                        ok: false,
                        message: Some(format!("trash failed: {error}")),
                    });
                }
            }
        }
        if let Err(error) = tx.commit() {
            tracing::error!(
                error = %error,
                batch_id = %batch_id,
                "trash catalog commit failed after journal append; surfacing recoverable batch"
            );
            return Ok(trash_commit_failure_result(
                batch_id,
                succeeded,
                failed,
                messages,
                &error.to_string(),
            ));
        }

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
        let moved = tx.execute(
            "UPDATE face_prints SET person_id = ?1 WHERE person_id = ?2",
            rusqlite::params![dst, src],
        )? as u32;
        // R4-07: carry the source's user-assigned identity onto the destination
        // when the destination has NONE, BEFORE deleting src (the subqueries must
        // still see src). Merge direction is arbitrary (suggestions order by id;
        // drag/bulk by user choice), so a named cluster can be the source —
        // without this its name/title/first/middle/last/suffix is silently lost.
        // The WHERE gate fires only when EVERY name-bearing column on the
        // destination is NULL, so merging two differently-named people never
        // grafts the source's sub-fields onto an already-named destination
        // (R4-07 delta). is_unknown clears once the carried name lands.
        let _ = tx.execute(
            "UPDATE persons SET
                 name        = COALESCE(name,        (SELECT name        FROM persons WHERE id = ?2)),
                 title       = COALESCE(title,       (SELECT title       FROM persons WHERE id = ?2)),
                 first_name  = COALESCE(first_name,  (SELECT first_name  FROM persons WHERE id = ?2)),
                 middle_name = COALESCE(middle_name, (SELECT middle_name FROM persons WHERE id = ?2)),
                 last_name   = COALESCE(last_name,   (SELECT last_name   FROM persons WHERE id = ?2)),
                 suffix      = COALESCE(suffix,      (SELECT suffix      FROM persons WHERE id = ?2)),
                 is_unknown  = CASE WHEN COALESCE(name, (SELECT name FROM persons WHERE id = ?2)) IS NOT NULL THEN 0 ELSE is_unknown END
             WHERE id = ?1
               AND name IS NULL AND title IS NULL AND first_name IS NULL
               AND middle_name IS NULL AND last_name IS NULL AND suffix IS NULL",
            rusqlite::params![dst, src],
        );
        let _ = tx.execute("DELETE FROM persons WHERE id = ?1", rusqlite::params![src]);
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
        let _ = tx.execute(
            "DELETE FROM face_verifications WHERE (person_a = ?1 OR person_b = ?1) \
             AND (face_a IS NULL OR face_b IS NULL)",
            rusqlite::params![src],
        );
        // Recompute the destination's file_count AND representative_face_id
        // (highest-quality embedded face now in the cluster) so the People
        // card + suggestion anchor reflect the combined membership rather than
        // a stale rep. COALESCE keeps the old rep if no embedded face survives.
        let _ = tx.execute(
            "UPDATE persons SET file_count = (SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id = ?1) WHERE id = ?1",
            rusqlite::params![dst],
        );
        let _ = tx.execute(
            "UPDATE persons SET representative_face_id = COALESCE(
                 (SELECT fp.id FROM face_prints fp
                  WHERE fp.person_id = ?1 AND fp.arcface_embedding IS NOT NULL
                  ORDER BY COALESCE(fp.face_quality, 0) DESC LIMIT 1),
                 representative_face_id)
             WHERE id = ?1",
            rusqlite::params![dst],
        );
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
                    action: action.into(),
                    succeeded: 0,
                    failed: 1,
                    messages: vec![BulkActionItem {
                        file_id: None,
                        ok: false,
                        message: Some(format!("bulk action worker failed: {err}")),
                    }],
                },
            ))))
            .await;
        }
    }
}

/// Save the structured-name fields (title/first/middle/last/suffix) for a
/// person cluster through the engine's single-writer connection.
pub(crate) async fn handle_rename_person(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RenamePersonPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let title = payload.title.as_deref().filter(|s| !s.trim().is_empty());
        let first = payload.first_name.as_deref().filter(|s| !s.trim().is_empty());
        let middle = payload
            .middle_name
            .as_deref()
            .filter(|s| !s.trim().is_empty());
        let last = payload.last_name.as_deref().filter(|s| !s.trim().is_empty());
        let suffix = payload.suffix.as_deref().filter(|s| !s.trim().is_empty());
        let display = match (first, last) {
            (Some(f), Some(l)) => Some(format!("{f} {l}")),
            (Some(f), None) => Some(f.to_string()),
            (None, Some(l)) => Some(l.to_string()),
            _ => None,
        };
        tx.execute(
            "UPDATE persons SET title=?1, first_name=?2, middle_name=?3, last_name=?4, suffix=?5, name=COALESCE(?6, name) WHERE id=?7",
            rusqlite::params![title, first, middle, last, suffix, display, payload.person_id],
        )?;
        tx.commit()?;
        Ok(BulkActionResult {
            action: "renamePerson".into(),
            succeeded: 1,
            failed: 0,
            messages: vec![BulkActionItem {
                file_id: Some(payload.person_id),
                ok: true,
                message: display,
            }],
        })
    })
    .await;

    emit_bulk_result(&sink, "renamePerson", result).await;
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
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let conn = db.lock();
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
    })
    .await;

    emit_bulk_result(&sink, "markPersonsAsUnknown", result).await;
}

/// Record a user "different people" verdict for a suggested pair. Persists into
/// face_verifications keyed on BOTH the person pair (PK, for compat + the VLM
/// path) and the stable (min,max) anchor face_print pair (v13), so
/// findMergeSuggestions keeps suppressing the pair across re-clustering. Routed
/// here so the write goes through the engine's single-writer connection rather
/// than a second app-side writer. Completion uses the same awaited bulk result
/// contract as the adjacent person mutations.
pub(crate) async fn handle_mark_persons_different(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::MarkPersonsDifferentPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
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
        Ok(BulkActionResult {
            action: "markPersonsDifferent".into(),
            succeeded: 1,
            failed: 0,
            messages: vec![BulkActionItem {
                file_id: None,
                ok: true,
                message: None,
            }],
        })
    })
    .await;

    emit_bulk_result(&sink, "markPersonsDifferent", result).await;
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
                "SELECT p.id, rep.id, COUNT(fpc.id), rep.arcface_embedding
                 FROM persons p
                 JOIN face_prints rep
                   ON rep.id = p.representative_face_id AND rep.arcface_embedding IS NOT NULL
                 JOIN face_prints fpc ON fpc.person_id = p.id
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
            blob.chunks_exact(4)
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
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "find_merge_suggestions_failed".into(),
                message: format!("Find merge suggestions worker failed: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn trash_candidate(path: &std::path::Path, file_id: i64) -> TrashCandidate {
        let metadata = std::fs::metadata(path).unwrap();
        TrashCandidate {
            file_id,
            path: path.to_path_buf(),
            staging_path: path.with_file_name(format!(".fileid-test-stage-{file_id}")),
            indexed_size: metadata.len(),
            indexed_file_ref: crate::platform::file_ref(path).map(|value| value as i64),
            indexed_content_hash: Some(
                crate::util::content_hash::content_hash(path, metadata.len())
                    .unwrap()
                    .to_vec(),
            ),
            source_identity: crate::platform::file_identity(path).unwrap(),
            expected_exact: None,
        }
    }

    #[test]
    #[cfg(windows)]
    fn windows_trash_requires_a_durable_identity_bound_receipt() {
        let dir = unique_temp_dir("trash-windows-receipt");
        let path = dir.join("claimed.bin");
        std::fs::write(&path, b"payload").unwrap();
        let candidate = trash_candidate(&path, 7);
        let make_entry = || TrashLogEntry {
            batch_id: "receipt-test".into(),
            timestamp: 0.0,
            items: vec![TrashLogItem {
                file_id: candidate.file_id,
                original_path: candidate.path.to_string_lossy().into_owned(),
                recycle_bin_id: Some(candidate.staging_path.to_string_lossy().into_owned()),
                recycle_physical_path: None,
                source_identity: Some(candidate.source_identity),
            }],
        };
        let locate = |claims: &[&str]| {
            assert_eq!(claims, [candidate.staging_path.to_str().unwrap()]);
            std::collections::HashMap::from([(
                crate::util::path_safety::normalize_for_exclusion(&candidate.staging_path),
                path.clone(),
            )])
        };
        let appended = std::cell::Cell::new(false);
        let mut entry = make_entry();
        let mut outcomes = vec![CheckedTrashOutcome::Trashed];

        bind_windows_trash_receipts_with(
            std::slice::from_ref(&candidate),
            &mut outcomes,
            &mut entry,
            locate,
            |_| {
                appended.set(true);
                Ok(())
            },
        );

        assert!(matches!(outcomes[0], CheckedTrashOutcome::Trashed));
        assert!(appended.get());
        assert_eq!(
            entry.items[0].recycle_physical_path.as_deref(),
            path.to_str()
        );

        let mut entry = make_entry();
        let mut outcomes = vec![CheckedTrashOutcome::Trashed];
        bind_windows_trash_receipts_with(
            std::slice::from_ref(&candidate),
            &mut outcomes,
            &mut entry,
            locate,
            |_| anyhow::bail!("injected receipt append failure"),
        );
        assert!(matches!(outcomes[0], CheckedTrashOutcome::Failed(_)));
        assert_eq!(entry.items[0].recycle_physical_path, None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn trash_rejects_same_path_replacement_before_backend_call() {
        let dir = unique_temp_dir("trash-replacement");
        let path = dir.join("duplicate.bin");
        let original = dir.join("original.bin");
        std::fs::write(&path, b"original").unwrap();
        let candidate = trash_candidate(&path, 7);
        std::fs::rename(&path, &original).unwrap();
        std::fs::write(&path, b"replaced").unwrap();

        let called = std::cell::Cell::new(false);
        let outcome = checked_trash_one_with(&candidate, |_, _, _| {
            called.set(true);
            Ok(())
        });
        assert!(matches!(outcome, CheckedTrashOutcome::Rejected(_)));
        assert!(!called.get(), "a stale identity must never reach the Trash backend");
        assert_eq!(std::fs::read(&path).unwrap(), b"replaced");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_trash_evidence_rejects_same_size_in_place_changes_and_restores_claim() {
        let dir = unique_temp_dir("trash-exact-change");
        let path = dir.join("duplicate.bin");
        let keeper = dir.join("keeper.bin");
        std::fs::write(&path, b"original").unwrap();
        std::fs::write(&keeper, b"original").unwrap();
        let mut candidate = trash_candidate(&path, 8);
        candidate.expected_exact = Some(ipc::ExactTrashIdentity {
            file_id: 8,
            path: path.to_string_lossy().into_owned(),
            size_bytes: 8,
            sha256_hex: hex::encode(
                crate::util::content_hash::exact_file_sha256(&path, 8).unwrap(),
            ),
            keeper_path: keeper.to_string_lossy().into_owned(),
            keeper_size_bytes: 8,
            keeper_sha256_hex: hex::encode(
                crate::util::content_hash::exact_file_sha256(&keeper, 8).unwrap(),
            ),
        });
        std::fs::write(&path, b"replaced").unwrap();

        let called = std::cell::Cell::new(false);
        let outcome = checked_trash_one_with(&candidate, |_, _, _| {
            called.set(true);
            Ok(())
        });
        assert!(matches!(outcome, CheckedTrashOutcome::Rejected(_)));
        assert!(!called.get());
        assert_eq!(std::fs::read(&path).unwrap(), b"replaced");
        assert!(!candidate.staging_path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_trash_rejects_individually_valid_but_unequal_proof() {
        let dir = unique_temp_dir("trash-exact-unequal");
        let path = dir.join("victim.bin");
        let keeper = dir.join("keeper.bin");
        std::fs::write(&path, b"victim!!").unwrap();
        std::fs::write(&keeper, b"keeper!!").unwrap();
        let mut candidate = trash_candidate(&path, 10);
        candidate.expected_exact = Some(ipc::ExactTrashIdentity {
            file_id: 10,
            path: path.to_string_lossy().into_owned(),
            size_bytes: 8,
            sha256_hex: hex::encode(
                crate::util::content_hash::exact_file_sha256(&path, 8).unwrap(),
            ),
            keeper_path: keeper.to_string_lossy().into_owned(),
            keeper_size_bytes: 8,
            keeper_sha256_hex: hex::encode(
                crate::util::content_hash::exact_file_sha256(&keeper, 8).unwrap(),
            ),
        });

        let called = std::cell::Cell::new(false);
        let outcome = checked_trash_one_with(&candidate, |_, _, _| {
            called.set(true);
            Ok(())
        });
        assert!(matches!(outcome, CheckedTrashOutcome::Rejected(_)));
        assert!(!called.get());
        assert_eq!(std::fs::read(&path).unwrap(), b"victim!!");
        assert!(!candidate.staging_path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_hash_rejects_path_replacement_during_hashing() {
        let dir = unique_temp_dir("trash-exact-path-swap");
        let path = dir.join("keeper.bin");
        let held = dir.join("held.bin");
        std::fs::write(&path, b"same").unwrap();
        let result = exact_hash_at_stable_path_with(&path, 4, |current, size| {
            std::fs::rename(current, &held).map_err(|error| error.to_string())?;
            std::fs::write(current, b"swap").map_err(|error| error.to_string())?;
            let hash = crate::util::content_hash::exact_file_sha256(current, size)
                .map_err(|error| error.to_string())?;
            let hashed_identity = crate::platform::file_identity(current)
                .ok_or_else(|| "missing replacement identity".to_string())?;
            std::fs::remove_file(current).map_err(|error| error.to_string())?;
            std::fs::rename(&held, current).map_err(|error| error.to_string())?;
            Ok((hash, hashed_identity))
        });
        assert!(result.unwrap_err().contains("path identity changed"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(windows)]
    #[test]
    fn exact_trash_locks_keeper_and_victim_through_backend_call() {
        let dir = unique_temp_dir("trash-exact-file-locks");
        let path = dir.join("victim.bin");
        let keeper = dir.join("keeper.bin");
        std::fs::write(&path, b"same").unwrap();
        std::fs::write(&keeper, b"same").unwrap();
        let mut candidate = trash_candidate(&path, 11);
        candidate.expected_exact = Some(ipc::ExactTrashIdentity {
            file_id: 11,
            path: path.to_string_lossy().into_owned(),
            size_bytes: 4,
            sha256_hex: hex::encode(
                crate::util::content_hash::exact_file_sha256(&path, 4).unwrap(),
            ),
            keeper_path: keeper.to_string_lossy().into_owned(),
            keeper_size_bytes: 4,
            keeper_sha256_hex: hex::encode(
                crate::util::content_hash::exact_file_sha256(&keeper, 4).unwrap(),
            ),
        });

        let moved = dir.join("recycled.bin");
        let keeper_blocked = std::cell::Cell::new(false);
        let victim_write_blocked = std::cell::Cell::new(false);
        let outcome = checked_trash_one_with(&candidate, |actual, _, _| {
            keeper_blocked.set(std::fs::remove_file(&keeper).is_err());
            victim_write_blocked.set(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(actual)
                    .is_err(),
            );
            std::fs::rename(actual, &moved)?;
            Ok(())
        });
        assert!(keeper_blocked.get());
        assert!(victim_write_blocked.get());
        assert!(matches!(outcome, CheckedTrashOutcome::Trashed));
        assert!(!path.exists());
        assert!(moved.exists());
        assert!(keeper.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_trash_rejects_when_unselected_keeper_changes() {
        let dir = unique_temp_dir("trash-exact-keeper");
        let path = dir.join("victim.bin");
        let keeper = dir.join("keeper.bin");
        std::fs::write(&path, b"same").unwrap();
        std::fs::write(&keeper, b"same").unwrap();
        let expected = hex::encode(
            crate::util::content_hash::exact_file_sha256(&path, 4).unwrap(),
        );
        let mut candidate = trash_candidate(&path, 9);
        candidate.expected_exact = Some(ipc::ExactTrashIdentity {
            file_id: 9,
            path: path.to_string_lossy().into_owned(),
            size_bytes: 4,
            sha256_hex: expected.clone(),
            keeper_path: keeper.to_string_lossy().into_owned(),
            keeper_size_bytes: 4,
            keeper_sha256_hex: expected,
        });
        std::fs::write(&keeper, b"gone").unwrap();

        let called = std::cell::Cell::new(false);
        let outcome = checked_trash_one_with(&candidate, |_, _, _| {
            called.set(true);
            Ok(())
        });
        assert!(matches!(outcome, CheckedTrashOutcome::Rejected(_)));
        assert!(!called.get());
        assert_eq!(std::fs::read(&path).unwrap(), b"same");
        assert!(!candidate.staging_path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trash_identity_check_is_per_item_for_mixed_batches() {
        let dir = unique_temp_dir("trash-mixed");
        let good_path = dir.join("good.bin");
        let replaced_path = dir.join("replaced.bin");
        let missing_path = dir.join("missing.bin");
        std::fs::write(&good_path, b"good").unwrap();
        std::fs::write(&replaced_path, b"before").unwrap();
        std::fs::write(&missing_path, b"gone").unwrap();
        let good = trash_candidate(&good_path, 1);
        let replaced = trash_candidate(&replaced_path, 2);
        let missing = trash_candidate(&missing_path, 3);
        std::fs::rename(&replaced_path, dir.join("old-replaced.bin")).unwrap();
        std::fs::write(&replaced_path, b"after!").unwrap();
        std::fs::remove_file(&missing_path).unwrap();

        let backend_calls = std::cell::Cell::new(0usize);
        let outcomes: Vec<CheckedTrashOutcome> = [&good, &replaced, &missing]
            .into_iter()
            .map(|candidate| {
                checked_trash_one_with(candidate, |staged, _, _| {
                    backend_calls.set(backend_calls.get() + 1);
                    std::fs::remove_file(staged)?;
                    Ok(())
                })
            })
            .collect();
        assert!(matches!(outcomes[0], CheckedTrashOutcome::Trashed));
        assert!(matches!(outcomes[1], CheckedTrashOutcome::Rejected(_)));
        assert!(matches!(outcomes[2], CheckedTrashOutcome::Rejected(_)));
        assert_eq!(backend_calls.get(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trash_commit_failure_preserves_recovery_batch_and_reports_failure() {
        let result = trash_commit_failure_result(
            "batch-123".into(),
            1,
            0,
            vec![BulkActionItem {
                file_id: Some(7),
                ok: true,
                message: Some("old path".into()),
            }],
            "disk full",
        );
        assert_eq!(result.action, "trashFiles:batch-123");
        assert_eq!(result.succeeded, 0);
        assert_eq!(result.failed, 1);
        assert!(result.messages.iter().all(|message| !message.ok));
        assert!(result
            .messages
            .iter()
            .filter_map(|message| message.message.as_deref())
            .any(|message| message.contains("Undo batch batch-123")));
    }

    // C1-012: a second write appends rather than truncating (NDJSON growth).
    #[tokio::test]
    async fn bulk_join_failure_still_emits_terminal_result() {
        let joined = tokio::task::spawn_blocking(|| -> anyhow::Result<BulkActionResult> {
            panic!("injected worker panic")
        })
        .await;
        let (sink, mut events) = Sink::channel_for_test(1);
        emit_bulk_result(&sink, "trashFiles", joined).await;
        let event = events.recv().await.expect("terminal bulk event");
        let EventPayload::BulkActionResult(result) = event.payload else {
            panic!("expected BulkActionResult");
        };
        assert_eq!(result.inner.action, "trashFiles");
        assert_eq!(result.inner.succeeded, 0);
        assert_eq!(result.inner.failed, 1);
        assert!(result.inner.messages.iter().any(|item| {
            !item.ok
                && item
                    .message
                    .as_deref()
                    .unwrap_or_default()
                    .contains("worker failed")
        }));
    }

    #[tokio::test]
    async fn merge_suggestion_failure_emits_command_terminal_error() {
        let (sink, mut events) = Sink::channel_for_test(1);
        handle_find_merge_suggestions(
            sink,
            std::env::temp_dir().join(format!(
                "fileid-missing-suggestions-{}-{}.sqlite",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )),
        )
        .await;

        let event = events.recv().await.expect("terminal error event");
        assert!(matches!(
            event.payload,
            EventPayload::Error(Wrap {
                inner: EngineError { ref kind, .. }
            }) if kind == "find_merge_suggestions_failed"
        ));
    }

    #[tokio::test]
    async fn mark_persons_different_failure_emits_awaited_bulk_result() {
        let db = std::sync::Arc::new(parking_lot::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        ));
        let (sink, mut events) = Sink::channel_for_test(1);
        handle_mark_persons_different(
            sink,
            db,
            ipc::MarkPersonsDifferentPayload {
                source_person_id: 1,
                destination_person_id: 2,
                source_anchor_face_id: 3,
                destination_anchor_face_id: 4,
            },
        )
        .await;

        let event = events.recv().await.expect("terminal bulk event");
        let EventPayload::BulkActionResult(result) = event.payload else {
            panic!("expected BulkActionResult");
        };
        assert_eq!(result.inner.action, "markPersonsDifferent");
        assert_eq!(result.inner.succeeded, 0);
        assert_eq!(result.inner.failed, 1);
        assert!(result.inner.messages.iter().all(|item| !item.ok));
    }

    #[test]
    fn recovery_sidecar_appends() {
        let dir = unique_temp_dir("append");
        write_rename_recovery_line(&dir, &rename_recovery_line(1, "a", "b"));
        write_rename_recovery_line(&dir, &rename_recovery_line(2, "c", "d"));
        let contents = std::fs::read_to_string(dir.join("rename_recover.ndjson")).unwrap();
        assert_eq!(contents.lines().filter(|l| !l.is_empty()).count(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }
}
