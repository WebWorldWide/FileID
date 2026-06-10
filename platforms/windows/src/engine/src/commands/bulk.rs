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
    std::fs::rename(
        crate::util::path_safety::to_extended_length(src),
        crate::util::path_safety::to_extended_length(dst),
    )
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
            // ENG-91: keep path_hash in sync with path_text (lookups/dedup key
            // on it). ENG-92: do NOT swallow the UPDATE error and still claim
            // success — a file renamed on disk but with a failed DB write must
            // be reported as failed (the next scan's rename-heal rebinds it via
            // content_hash/file_ref).
            let dest_hash = crate::util::path_safety::stable_path_hash(&dest_text);
            match tx.execute(
                "UPDATE files SET path_text = ?1, path_hash = ?2, path_search = ?1 WHERE id = ?3",
                rusqlite::params![dest_text, dest_hash, entry.file_id],
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
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(entry.file_id),
                        ok: false,
                        message: Some(format!("renamed on disk but DB update failed: {err}")),
                    });
                }
            }
        }
        tx.commit()?;
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
        let mut path_for_id: Vec<(i64, PathBuf, bool)> = Vec::with_capacity(payload.file_ids.len());

        {
            let conn = db.lock();
            for fid in &payload.file_ids {
                if let Ok(p) = conn.query_row(
                    "SELECT path_text FROM files WHERE id = ?1",
                    rusqlite::params![fid],
                    |r| r.get::<_, String>(0),
                ) {
                    let path = PathBuf::from(p);
                    // Verbatim (\\?\) probe so a >260-char file is classified as
                    // present (and trashed) instead of "already missing" (#28).
                    let existed = std::fs::symlink_metadata(
                        crate::util::path_safety::to_extended_length(&path),
                    )
                    .is_ok();
                    path_for_id.push((*fid, path, existed));
                }
            }
        }

        let outcomes = crate::shell::trash::trash(
            &path_for_id
                .iter()
                .map(|(_, p, _)| p.clone())
                .collect::<Vec<_>>(),
        );

        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let mut log_items: Vec<TrashLogItem> = Vec::new();
        for ((fid, path, existed), trashed_ok) in path_for_id.iter().zip(outcomes) {
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
                    recycle_bin_id: None,
                });
            } else {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*fid),
                    ok: false,
                    message: Some(format!("trash failed: {}", path.display())),
                });
            }
        }
        tx.commit()?;

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
                tracing::warn!(?err, "trash_log append failed");
            }
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
        let _ = tx.execute("DELETE FROM persons WHERE id = ?1", rusqlite::params![src]);
        // Clean up face-verification verdicts referencing the merged-away source
        // person — otherwise findMergeSuggestions JOINs on a now-deleted persons
        // row and surfaces stale suggestions (orphan rows that never GC). The
        // "src != X" verdict is moot once src is folded into dst.
        let _ = tx.execute(
            "DELETE FROM face_verifications WHERE person_a = ?1 OR person_b = ?1",
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
                    failed: 0,
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
                "UPDATE persons SET is_unknown = 1, name = NULL, first_name = NULL, last_name = NULL WHERE id = ?1",
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
        conn.execute(
            "INSERT OR REPLACE INTO face_verifications
                (person_a, person_b, same_person, confidence, vlm_model, verified_at, face_a, face_b)
             VALUES (?1, ?2, 0, 1.0, 'user-verified', ?3, ?4, ?5)",
            rusqlite::params![pa, pb, now, fa, fb],
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
        if let Ok(mut vstmt) = conn.prepare(
            "SELECT person_a, person_b, face_a, face_b FROM face_verifications WHERE same_person = 0",
        ) {
            let rs = vstmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                    ))
                })
                .ok();
            if let Some(rs) = rs {
                for (pa, pb, fa, fb) in rs.flatten() {
                    let pk = if pa < pb { (pa, pb) } else { (pb, pa) };
                    verified_persons.insert(pk);
                    if let (Some(fa), Some(fb)) = (fa, fb) {
                        let fk = if fa < fb { (fa, fb) } else { (fb, fa) };
                        verified_faces.insert(fk);
                        verified_face_pairs.push((fa, fb));
                    }
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
