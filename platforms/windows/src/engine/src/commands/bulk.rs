//! Bulk action handlers — every `BulkActionResult`-shaped IPC. Apply tags,
//! rename files, trash files, merge person clusters, rename persons, mark
//! persons as unknown, find merge suggestions. They share the
//! `emit_bulk_result` tail so the wire shape stays uniform.

use std::path::PathBuf;

use crate::ipc::{
    self, sink::Sink, BulkActionItem, BulkActionResult, EngineError, EventPayload, IpcEvent,
    MergeSuggestion, MergeSuggestions, TagMode, Wrap,
};
use crate::pipeline::face_clustering::{COS_LOW, MERGE_SUGGEST_COS_HIGH};

use super::trash_log::{self, TrashLogEntry, TrashLogItem};

/// Bulk-apply tags to a set of files. Updates DB `tags` table + writes the
/// sidecar JSON so Explorer + future scans see the same set.
pub(crate) async fn handle_apply_tags(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::ApplyTagsPayload,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();
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
                if let Err(err) = crate::shell::tags::write_tags(std::path::Path::new(&path), &tags)
                {
                    tracing::warn!(?err, path = %crate::platform::redact_path_for_log(&path), "sidecar tag write failed");
                }
                succeeded += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*fid),
                    ok: true,
                    message: None,
                });
            }
        }
        tx.commit()?;
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

/// Bulk-rename a set of files (filename only, same directory). Each move
/// is `MoveFileExW` semantics via std::fs::rename + DB row update.
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
            if dest.exists() {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(entry.file_id),
                    ok: false,
                    message: Some(format!("destination exists: {}", dest.display())),
                });
                continue;
            }
            if let Err(err) = std::fs::rename(&path, &dest) {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(entry.file_id),
                    ok: false,
                    message: Some(format!("rename failed: {err}")),
                });
                continue;
            }
            let dest_text = dest.to_string_lossy().to_string();
            // ENG-91: keep path_hash in sync with path_text (lookups/dedup key
            // on it). ENG-92: do NOT swallow the UPDATE error and still claim
            // success — a file renamed on disk but with a failed DB write must
            // be reported as failed (the next scan's rename-heal rebinds it via
            // content_hash/file_ref).
            let dest_hash = crate::util::path_safety::stable_path_hash(&dest_text);
            match tx.execute(
                "UPDATE files SET path_text = ?1, path_hash = ?2 WHERE id = ?3",
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
                    let existed = path.exists();
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
/// uncertain band (COS_LOW..COS_HIGH from face_clustering). Pairs already
/// confirmed-different in face_verifications are filtered out so the
/// suggested-merges sheet doesn't keep re-prompting.
pub(crate) async fn handle_find_merge_suggestions(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<MergeSuggestions> {
        let conn = db.lock();
        // One row per person via a JOIN to the representative face (its anchor
        // embedding + id) plus a COUNT JOIN for member size — replaces the two
        // per-person correlated subqueries the old query ran. representative_
        // face_id is the cluster anchor (highest-quality embedded face), kept
        // current by clustering + handle_merge_clusters.
        let mut stmt = conn.prepare(
            "SELECT p.id, rep.id, COUNT(fpc.id), rep.arcface_embedding
             FROM persons p
             JOIN face_prints rep
               ON rep.id = p.representative_face_id AND rep.arcface_embedding IS NOT NULL
             JOIN face_prints fpc ON fpc.person_id = p.id
             GROUP BY p.id",
        )?;
        let rows: Vec<(i64, i64, i64, Vec<u8>)> = stmt
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

        let decode = |blob: &[u8]| -> Vec<f32> {
            blob.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        };
        let cos = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };

        // "Different people" verdicts. Person-keyed pairs cover legacy rows;
        // face-anchor-keyed pairs (v13) survive re-clustering because
        // face_prints ids are stable. A candidate is suppressed if EITHER key
        // matches.
        let mut verified_persons: std::collections::HashSet<(i64, i64)> =
            std::collections::HashSet::new();
        let mut verified_faces: std::collections::HashSet<(i64, i64)> =
            std::collections::HashSet::new();
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
                    }
                }
            }
        }

        let embeddings: Vec<(i64, i64, i64, Vec<f32>)> = rows
            .into_iter()
            .map(|(pid, anchor_id, count, blob)| (pid, anchor_id, count, decode(&blob)))
            .collect();

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
                if verified_persons.contains(&pk) || verified_faces.contains(&fk) {
                    continue;
                }
                let s = cos(ea, eb);
                if s >= COS_LOW && s < MERGE_SUGGEST_COS_HIGH {
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
