// DBWriter — drains the Tagging → DB channel and writes 100-file or
// 200ms batches into the single SQLite writer connection.
//
// Single-writer is by design: WAL permits concurrent readers but only
// one writer. Every insert + the resume cursor update + the FTS5 OCR
// row land in the same transaction so a crash mid-batch leaves no
// partial state.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use tokio::sync::mpsc;

use crate::coordinator::ScanCoordinator;
use crate::pipeline::tagging::TaggedFile;
use crate::platform::{dbwriter_batch_size_for, memory_tier};

/// Fallback flush trigger if the adaptive sizing yields nothing.
/// `current_batch_size()` polls memory tier and picks a tier-appropriate
/// value (Low=64, Balanced=250, High=500). 250 is the Balanced default;
/// previous behavior was 100/200ms.
const BATCH_SIZE_FALLBACK: usize = 250;
const FLUSH_INTERVAL: Duration = Duration::from_millis(200);

/// Adaptive batch size driven by available RAM. Re-evaluated at the top
/// of each batch so a memory-pressure shift mid-scan downshifts batch
/// size before we OOM (rather than tripping the OS-level reaper).
fn current_batch_size() -> usize {
    dbwriter_batch_size_for(memory_tier()).max(1)
}

/// Stats reported per batch — fed into the `batchSummary` IPC event so
/// the app sidebar can show throughput in real time.
#[derive(Debug, Clone, Default)]
pub struct BatchStats {
    pub batch_index: u32,
    pub files_in_batch: u32,
    pub processed_total: u64,
    /// Cumulative failed-file count. Plumbed through Progress events so
    /// the sidebar "Failures" stat updates during scan instead of waiting
    /// for ScanComplete.
    pub failed_total: u64,
    pub wall_seconds: f64,
    pub files_per_second: f64,
    pub utilization: f64,
    pub vision_p50_ms: f64,
    pub vision_p95_ms: f64,
    pub clip_p50_ms: f64,
    pub clip_p95_ms: f64,
    pub store_insert_p50_ms: f64,
    pub store_insert_p95_ms: f64,
}

pub struct DbWriter {
    conn: Arc<Mutex<Connection>>,
    coordinator: ScanCoordinator,
}

impl DbWriter {
    pub fn new(conn: Arc<Mutex<Connection>>, coordinator: ScanCoordinator) -> Self {
        Self { conn, coordinator }
    }

    /// Drain the input receiver until the channel closes, flushing on
    /// the lesser of `BATCH_SIZE` accumulated rows or `FLUSH_INTERVAL`
    /// since the first row in the current batch.
    ///
    /// Returns total processed + total failed counts when finished.
    pub async fn run<F>(self, mut input: mpsc::Receiver<TaggedFile>, mut on_batch: F)
        -> Result<(u64, u64)>
    where
        F: FnMut(BatchStats),
    {
        let mut buffer: Vec<TaggedFile> = Vec::with_capacity(BATCH_SIZE_FALLBACK);
        let mut deadline: Option<Instant> = None;
        let mut total: u64 = 0;
        let mut failed: u64 = 0;
        let mut batch_index: u32 = 0;
        let mut current_target = current_batch_size();
        // Re-check memory tier every 30s so a pressure shift mid-scan
        // downshifts batch size before we trip the OOM reaper.
        let mut next_tier_check = Instant::now() + Duration::from_secs(30);

        loop {
            if Instant::now() >= next_tier_check {
                let new_target = current_batch_size();
                if new_target != current_target {
                    tracing::info!(
                        old_batch = current_target,
                        new_batch = new_target,
                        tier = memory_tier().as_str(),
                        "[DBWRITER] adaptive batch size refreshed"
                    );
                    current_target = new_target;
                }
                next_tier_check = Instant::now() + Duration::from_secs(30);
            }

            let timeout = deadline
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(FLUSH_INTERVAL);

            let recv = tokio::time::timeout(timeout, input.recv()).await;
            match recv {
                Ok(Some(file)) => {
                    if buffer.is_empty() {
                        deadline = Some(Instant::now() + FLUSH_INTERVAL);
                    }
                    buffer.push(file);
                    if buffer.len() >= current_target {
                        let stats = self.flush(&mut buffer, &mut total, &mut failed, batch_index)?;
                        batch_index += 1;
                        deadline = None;
                        on_batch(stats);
                    }
                }
                Ok(None) => {
                    if !buffer.is_empty() {
                        let stats = self.flush(&mut buffer, &mut total, &mut failed, batch_index)?;
                        on_batch(stats);
                    }
                    break;
                }
                Err(_) => {
                    if !buffer.is_empty() {
                        let stats = self.flush(&mut buffer, &mut total, &mut failed, batch_index)?;
                        batch_index += 1;
                        deadline = None;
                        on_batch(stats);
                    }
                }
            }
            if self.coordinator.is_cancelled() {
                // Flush any rows that finished the (paid-for) ML pipeline before
                // the cancel landed but hadn't hit a batch boundary yet — else up
                // to current_target-1 fully-tagged files are dropped and must be
                // fully re-processed on the next scan. Mirrors the Ok(None)/Err
                // drain arms above.
                if !buffer.is_empty() {
                    let stats = self.flush(&mut buffer, &mut total, &mut failed, batch_index)?;
                    on_batch(stats);
                }
                break;
            }
        }
        Ok((total, failed))
    }

    /// Persist `buffer` in a single transaction. Empties the buffer.
    fn flush(
        &self,
        buffer: &mut Vec<TaggedFile>,
        total: &mut u64,
        failed: &mut u64,
        batch_index: u32,
    ) -> Result<BatchStats> {
        if buffer.is_empty() {
            return Ok(BatchStats::default());
        }

        let started = Instant::now();
        let mut vision = Vec::with_capacity(buffer.len());
        let mut clip = Vec::with_capacity(buffer.len());
        let mut store = Vec::with_capacity(buffer.len());
        let files_in_batch = buffer.len() as u32;

        // Face-crop ids orphaned by faces_evaluated re-processing, pruned AFTER
        // the batch commits — never inside the tx, so a batch rollback (which
        // restores the old face_prints rows) can't leave them crop-less.
        let mut crop_ids_to_prune: Vec<i64> = Vec::new();

        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction().context("opening tx")?;
        {
            // INSERT ... RETURNING id (SQLite 3.35+, bundled is 3.46+)
            // eliminates the per-row "SELECT id FROM files WHERE path_text = ?"
            // round-trip. Previously the dbwriter ran one INSERT + one SELECT
            // per file = 2N statement executions per batch; this drops to N.
            // The RETURNING clause yields the row id whether the row was
            // freshly inserted OR updated via the ON CONFLICT DO UPDATE
            // branch — same id stability the SELECT provided.
            let mut file_stmt = tx
                .prepare_cached(INSERT_FILE_RETURNING_ID_SQL)
                .context("preparing file insert (RETURNING)")?;
            let mut heal_lookup_stmt = tx
                .prepare_cached(HEAL_LOOKUP_SQL)
                .context("preparing rename-heal lookup")?;
            let mut heal_update_stmt = tx
                .prepare_cached(HEAL_UPDATE_SQL)
                .context("preparing rename-heal update")?;
            let mut clip_stmt = tx
                .prepare_cached(INSERT_CLIP_SQL)
                .context("preparing clip insert")?;
            let mut text_embed_stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO text_embeddings (file_id, embedding, model) \
                     VALUES (?1, ?2, ?3)",
                )
                .context("preparing text_embeddings insert")?;
            let mut face_delete = tx
                .prepare_cached("DELETE FROM face_prints WHERE file_id = ?1")
                .context("preparing face delete")?;
            let mut face_stmt = tx
                .prepare_cached(INSERT_FACE_SQL)
                .context("preparing face insert")?;
            let mut tag_delete = tx
                .prepare_cached("DELETE FROM tags WHERE file_id = ?1 AND source = 'auto'")
                .context("preparing tag delete")?;
            let mut tag_insert = tx
                .prepare_cached("INSERT OR REPLACE INTO tags (file_id, tag, source, score) VALUES (?1, ?2, 'auto', ?3)")
                .context("preparing tag insert")?;
            let mut ocr_text_stmt = tx
                .prepare_cached("INSERT OR REPLACE INTO ocr_text (file_id, text) VALUES (?1, ?2)")
                .context("preparing ocr_text insert")?;
            let mut ocr_fts_delete = tx
                .prepare_cached("DELETE FROM ocr_fts WHERE rowid = ?1")
                .context("preparing ocr_fts delete")?;
            let mut ocr_text_delete = tx
                .prepare_cached("DELETE FROM ocr_text WHERE file_id = ?1")
                .context("preparing ocr_text delete")?;
            let mut ocr_fts_stmt = tx
                .prepare_cached("INSERT INTO ocr_fts (rowid, text) VALUES (?1, ?2)")
                .context("preparing ocr_fts insert")?;
            // Phase 4: document text + FTS5 (same shape as ocr_text/ocr_fts).
            let mut doc_text_stmt = tx
                .prepare_cached("INSERT OR REPLACE INTO doc_text (file_id, text) VALUES (?1, ?2)")
                .context("preparing doc_text insert")?;
            let mut doc_fts_delete = tx
                .prepare_cached("DELETE FROM doc_fts WHERE rowid = ?1")
                .context("preparing doc_fts delete")?;
            let mut doc_text_delete = tx
                .prepare_cached("DELETE FROM doc_text WHERE file_id = ?1")
                .context("preparing doc_text delete")?;
            let mut doc_fts_stmt = tx
                .prepare_cached("INSERT INTO doc_fts (rowid, text) VALUES (?1, ?2)")
                .context("preparing doc_fts insert")?;
            for f in buffer.iter() {
                let insert_started = Instant::now();
                let path_text = f.path.to_string_lossy();
                // Redacted path for error context only — `path_text` stays the
                // raw value for the SQL binds. A flush error's `with_context`
                // string lands in the log + on the IPC wire, so it must never
                // carry an unredacted user path.
                let rp = crate::platform::redact_path_for_log(&f.path);
                let path_hash = crate::util::path_safety::stable_path_hash(&path_text);
                let extension = f
                    .path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();

                // Rename/move heal: if an existing row matches this file's
                // content identity at a DIFFERENT path, move it to the new
                // path BEFORE the INSERT. The ON CONFLICT(path_text) clause
                // below then updates the (now-relocated) existing row,
                // preserving its id + every FK-linked row (tags / embeddings /
                // faces / OCR) — what the rename-heal is for. Skipped when we
                // have neither identity (no heal possible).
                if f.file_ref.is_some() || f.content_hash.is_some() {
                    let ch_bytes = f.content_hash.as_ref().map(|h| h.as_slice());
                    let candidates: Vec<(i64, String, bool)> = heal_lookup_stmt
                        .query_map(
                            params![f.file_ref.map(|r| r as i64), ch_bytes, path_text.as_ref()],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)),
                        )
                        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                        .with_context(|| format!("rename-heal lookup for {rp}"))?;
                    // Heal the FIRST identity match whose old path genuinely MOVED
                    // (is gone from disk). Iterating — rather than the old
                    // LIMIT-1/no-ORDER-BY single fetch — ensures a still-present
                    // coexisting COPY returned ahead of the real orphan doesn't
                    // skip the heal and leave the genuinely-moved file's prior row
                    // (with its tags/faces) orphaned forever. file_ref matches are
                    // ordered first in SQL (the precise rename signal).
                    if let Some((id, _old, _by_ref)) = candidates
                        .into_iter()
                        .find(|(_, old, by_ref)| heal_candidate_moved(*by_ref, old))
                    {
                        heal_update_stmt
                            .execute(params![path_text, path_hash, id])
                            .with_context(|| format!("rename-heal update for {rp}"))?;
                        tracing::info!(
                            id,
                            new_path = %crate::platform::redact_path_for_log(&f.path),
                            "[RENAME-HEAL] re-bound existing row to new path"
                        );
                    }
                }

                let file_id: i64 = file_stmt
                    .query_row(
                        params![
                            path_text,
                            path_hash,
                            f.size_bytes as i64,
                            f.created_unix,
                            f.modified_unix,
                            f.scanned_unix,
                            f.kind.as_str(),
                            extension,
                            f.phash,
                            f.aesthetic,
                            f.has_faces as i64,
                            f.has_text as i64,
                            f.camera_model,
                            f.location_lat,
                            f.location_lon,
                            f.failed as i64,
                            f.error_message,
                            f.content_hash.as_ref().map(|h| h.as_slice()),
                            f.file_ref.map(|r| r as i64),
                        ],
                        |row| row.get(0),
                    )
                    .with_context(|| format!("insert+id for {rp}"))?;

                if let Some(emb) = &f.clip_embedding {
                    let bytes = floats_to_le_bytes(emb);
                    clip_stmt
                        .execute(params![file_id, bytes, "mobileclip_s2"])
                        .with_context(|| format!("clip insert for {rp}"))?;
                }

                // BGE-small text embeddings (Phase 4b) — parallel to clip
                // above but in a different vector space; persisted into
                // `text_embeddings` keyed by model so future embeddings
                // (BGE-m3, Nomic, ...) can coexist without table churn.
                if let Some(emb) = &f.text_embedding {
                    let bytes = floats_to_le_bytes(emb);
                    text_embed_stmt
                        .execute(params![file_id, bytes, "bge_small_en_v1_5"])
                        .with_context(|| format!("text_embeddings insert for {rp}"))?;
                }

                // Key the stale-face DELETE on whether the face stage actually
                // ran this session, NOT on `faces.is_empty()`: an edited/zero-
                // face re-process must clear orphaned face_prints (else they
                // keep polluting clusters), while a face-disabled / GPU-dead
                // session leaves still-valid rows intact (#5). The insert loop
                // is naturally a no-op when there are no faces.
                if f.faces_evaluated {
                    // Capture the face ids being replaced so their now-orphaned
                    // crop JPEGs (face_crops/<id>.jpg) can be pruned below: the
                    // re-inserted faces get fresh AUTOINCREMENT ids, so without
                    // this every faces_evaluated re-process leaks the prior crops
                    // on disk (face_crops/ grows unbounded across re-scans).
                    let stale_face_ids: Vec<i64> = {
                        let mut q =
                            tx.prepare_cached("SELECT id FROM face_prints WHERE file_id = ?1")?;
                        let ids = q
                            .query_map(params![file_id], |r| r.get::<_, i64>(0))?
                            .filter_map(|r| r.ok())
                            .collect::<Vec<_>>();
                        ids
                    };
                    face_delete
                        .execute(params![file_id])
                        .with_context(|| format!("face delete for {rp}"))?;
                    for face in &f.faces {
                        let bbox_json = serde_json::json!({
                            "x": face.bbox[0],
                            "y": face.bbox[1],
                            "w": face.bbox[2],
                            "h": face.bbox[3],
                            "roll": face.roll,
                            "yaw": face.yaw,
                            "pitch": face.pitch,
                        })
                        .to_string();
                        let arcface_bytes = floats_to_le_bytes(&face.embedding);
                        // print_data legacy: same bytes as arcface_embedding so old code keeps working.
                        face_stmt
                            .execute(params![
                                file_id,
                                arcface_bytes.clone(),
                                bbox_json,
                                arcface_bytes,
                                face.quality as f64,
                            ])
                            .with_context(|| format!("face insert for {rp}"))?;

                        if let Some(crop) = &face.crop_rgb_112 {
                            let face_id = tx.last_insert_rowid();
                            if let Err(err) = save_face_crop(face_id, crop) {
                                tracing::warn!(?err, face_id, "face crop write failed");
                            }
                        }
                    }
                    // New AUTOINCREMENT ids never collide with the deleted ones,
                    // so every captured id is now orphaned. Defer the file delete
                    // until after commit (below) so a rollback can't orphan crops.
                    crop_ids_to_prune.extend(stale_face_ids);
                }

                // Delete-then-conditional-insert, but ONLY when the OCR stage
                // actually ran this session — never on the ambiguous default-
                // skip path. This clears stale ocr_text/ocr_fts when a
                // re-process now yields empty text (phantom FTS hits, #11) while
                // leaving valid prior text untouched on the common skipped
                // sessions.
                if f.ocr_stage_ran {
                    ocr_fts_delete
                        .execute(params![file_id])
                        .with_context(|| format!("ocr_fts delete for {rp}"))?;
                    ocr_text_delete
                        .execute(params![file_id])
                        .with_context(|| format!("ocr_text delete for {rp}"))?;
                    if let Some(text) = &f.ocr_text {
                        if !text.trim().is_empty() {
                            ocr_text_stmt
                                .execute(params![file_id, text])
                                .with_context(|| format!("ocr_text insert for {rp}"))?;
                            ocr_fts_stmt
                                .execute(params![file_id, text])
                                .with_context(|| format!("ocr_fts insert for {rp}"))?;
                        }
                    }
                }

                // Phase 4: document text + FTS5 — same stage-ran-gated
                // delete-then-conditional-insert as ocr_text/ocr_fts above (#11).
                if f.doc_stage_ran {
                    doc_fts_delete
                        .execute(params![file_id])
                        .with_context(|| format!("doc_fts delete for {rp}"))?;
                    doc_text_delete
                        .execute(params![file_id])
                        .with_context(|| format!("doc_text delete for {rp}"))?;
                    if let Some(text) = &f.doc_text {
                        if !text.trim().is_empty() {
                            doc_text_stmt
                                .execute(params![file_id, text])
                                .with_context(|| format!("doc_text insert for {rp}"))?;
                            doc_fts_stmt
                                .execute(params![file_id, text])
                                .with_context(|| format!("doc_fts insert for {rp}"))?;
                        }
                    }
                }

                // Auto-tags (classifier output + enriched extras). Gate the
                // delete-then-reinsert on whether the tagging stage actually ran
                // this session — exactly like faces_evaluated / ocr_stage_ran /
                // doc_stage_ran above. A per-file timeout row or a GPU-dead
                // short-circuit emits an EMPTY `tags` vec; without this gate the
                // unconditional DELETE would wipe a file's previously-persisted
                // RAM++/CLIP-scene/Year/camera auto-tags on a transient slow read
                // or a mid-scan GPU TDR, with nothing re-inserted (data loss).
                // When the stage DID run, delete any prior `source='auto'` rows
                // and re-insert the fresh set atomically. User tags
                // (`source='user'`) are untouched either way.
                if f.tags_evaluated {
                    tag_delete
                        .execute(params![file_id])
                        .with_context(|| format!("tag delete for {rp}"))?;
                    for (tag, score) in &f.tags {
                        let trimmed = tag.trim();
                        if trimmed.is_empty() { continue; }
                        tag_insert
                            .execute(params![file_id, trimmed, score.map(|s| s as f64)])
                            .with_context(|| format!("tag insert for {rp}"))?;
                    }
                }

                if f.failed {
                    *failed += 1;
                }
                *total += 1;
                vision.push(f.vision_ms);
                clip.push(f.clip_ms);
                let insert_ms = insert_started.elapsed().as_secs_f64() * 1000.0;
                store.push(insert_ms);
                if std::env::var_os("FILEID_PERF_TRACE").is_some() {
                    tracing::debug!(
                        target: "FileIDEngine::perf",
                        stage = "db_write_done",
                        path = %crate::platform::redact_path_for_log(&f.path),
                        elapsed_ms = insert_ms,
                        "[PERF]"
                    );
                }
            }
        }
        tx.commit().context("commit batch")?;

        // Batch is durable; now prune crop JPEGs for the face ids it replaced.
        // (After commit so a rolled-back batch never deletes a live crop.)
        for old_id in crop_ids_to_prune {
            remove_face_crop(old_id);
        }

        // Periodic WAL checkpoint to keep the -wal file from growing
        // unboundedly on long scans. SQLite's auto-checkpoint (on this
        // connection) fires at ~1000 pages, but a -wal that never goes
        // through TRUNCATE keeps growing on disk. Every WAL_CHECKPOINT_BATCHES
        // commits we ask for a PASSIVE checkpoint; on success the WAL
        // gets truncated next time it crosses the threshold. PASSIVE
        // doesn't block readers, so this is safe to call from the
        // hot scan path.
        const WAL_CHECKPOINT_BATCHES: u32 = 32;
        if batch_index > 0 && batch_index % WAL_CHECKPOINT_BATCHES == 0 {
            // Invariant: no transaction open at this point — tx.commit
            // above closes it, and we're the only writer (the mutex
            // around conn enforces single-writer). Asserting via
            // is_autocommit() catches a future regression where someone
            // adds a BEGIN before this block.
            debug_assert!(
                conn.is_autocommit(),
                "WAL checkpoint must not run inside an open transaction"
            );
            // Best-effort — failure here just means the WAL stays a
            // little larger; it doesn't break correctness. A
            // SQLITE_BUSY here is normal if a reader is mid-query.
            if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)") {
                tracing::debug!(?e, batch_index, "periodic WAL checkpoint failed (transient, continuing)");
            }
        }

        drop(conn);

        let wall = started.elapsed().as_secs_f64();
        buffer.clear();

        Ok(BatchStats {
            batch_index,
            files_in_batch,
            processed_total: *total,
            failed_total: *failed,
            wall_seconds: wall,
            files_per_second: if wall > 0.0 { f64::from(files_in_batch) / wall } else { 0.0 },
            utilization: 0.0,
            vision_p50_ms: percentile(&mut vision, 0.50),
            vision_p95_ms: percentile(&mut vision, 0.95),
            clip_p50_ms: percentile(&mut clip, 0.50),
            clip_p95_ms: percentile(&mut clip, 0.95),
            store_insert_p50_ms: percentile(&mut store, 0.50),
            store_insert_p95_ms: percentile(&mut store, 0.95),
        })
    }
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((values.len() as f64 - 1.0) * p).round() as usize;
    values[idx]
}

/// Bare INSERT (no RETURNING) — retained for test fixtures that don't
/// need the id. The hot-path writer uses `INSERT_FILE_RETURNING_ID_SQL`
/// below, which is identical plus a `RETURNING id` suffix.
#[allow(dead_code)]  // used by test fixtures only; bin path uses the RETURNING variant.
const INSERT_FILE_SQL: &str = r#"
    INSERT INTO files (
        path_text, path_hash, size_bytes,
        created_at, modified_at, scanned_at,
        kind, extension,
        phash, aesthetic,
        has_faces, has_text,
        camera_model, location_lat, location_lon,
        failed, error_message,
        content_hash, file_ref
    )
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
    ON CONFLICT(path_text) DO UPDATE SET
        path_hash    = excluded.path_hash,
        size_bytes   = excluded.size_bytes,
        modified_at  = excluded.modified_at,
        scanned_at   = excluded.scanned_at,
        kind         = excluded.kind,
        extension    = excluded.extension,
        phash        = excluded.phash,
        has_faces    = excluded.has_faces,
        has_text     = excluded.has_text,
        camera_model = excluded.camera_model,
        location_lat = excluded.location_lat,
        location_lon = excluded.location_lon,
        failed       = excluded.failed,
        error_message= excluded.error_message,
        content_hash = COALESCE(excluded.content_hash, content_hash),
        file_ref     = COALESCE(excluded.file_ref, file_ref)
"#;

/// Hot-path INSERT. Returns `id` whether the row was freshly inserted or
/// updated via the ON CONFLICT DO UPDATE branch — SQLite 3.35+ guarantees
/// RETURNING fires on both paths. Eliminates the per-row SELECT round
/// trip the previous implementation paid (2N statement executions per
/// batch → N).
const INSERT_FILE_RETURNING_ID_SQL: &str = r#"
    INSERT INTO files (
        path_text, path_hash, size_bytes,
        created_at, modified_at, scanned_at,
        kind, extension,
        phash, aesthetic,
        has_faces, has_text,
        camera_model, location_lat, location_lon,
        failed, error_message,
        content_hash, file_ref
    )
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
    ON CONFLICT(path_text) DO UPDATE SET
        path_hash    = excluded.path_hash,
        size_bytes   = excluded.size_bytes,
        modified_at  = excluded.modified_at,
        scanned_at   = excluded.scanned_at,
        kind         = excluded.kind,
        extension    = excluded.extension,
        phash        = excluded.phash,
        has_faces    = excluded.has_faces,
        has_text     = excluded.has_text,
        camera_model = excluded.camera_model,
        location_lat = excluded.location_lat,
        location_lon = excluded.location_lon,
        failed       = excluded.failed,
        error_message= excluded.error_message,
        -- Preserve a previously-computed identity if the incoming value is
        -- NULL (e.g. an online-only re-scan after the original full read).
        content_hash = COALESCE(excluded.content_hash, content_hash),
        file_ref     = COALESCE(excluded.file_ref, file_ref)
    RETURNING id
"#;

// Rename/move heal lookup (v8 identity). Find an existing row whose
// `file_ref` (volume-local; the common rename case) or `content_hash`
// (cross-volume) matches, but at a DIFFERENT path. NULL identity columns
// never match — a row without identity can't be healed. Also returns the
// candidate's current `path_text` and a `by_ref` flag so the caller can
// distinguish a true MOVE (file_ref reused only for the same file) from a
// coexisting byte-identical COPY (two distinct files share a content_hash);
// only the former may heal unconditionally — see the call site.
const HEAL_LOOKUP_SQL: &str = r#"
    SELECT id, path_text,
           (?1 IS NOT NULL AND file_ref IS NOT NULL AND file_ref = ?1) AS by_ref
    FROM files
    WHERE path_text != ?3
      AND (
          (file_ref IS NOT NULL AND file_ref = ?1)
          OR (content_hash IS NOT NULL AND content_hash = ?2)
      )
    ORDER BY by_ref DESC
    LIMIT 32
"#;

// Heal: move the existing row to the new path. `UPDATE OR REPLACE` handles
// the rare case where ANOTHER row already sits at the new path (e.g. a copy
// preceded the rename) — SQLite REPLACE deletes the colliding row and
// FK-cascades its tags/embeddings/faces, then the healed row wins.
const HEAL_UPDATE_SQL: &str = r#"
    UPDATE OR REPLACE files
       SET path_text = ?1, path_hash = ?2
     WHERE id = ?3
"#;

/// Decide whether a heal candidate (an existing row matched by identity at a
/// different path) genuinely MOVED, and may therefore re-bind to the new path.
///
/// Heal ONLY when the candidate's previous path no longer exists on disk — a
/// genuine rename/move always leaves its old path gone. This single gate is
/// required for BOTH match kinds. A `content_hash`-only match also fires for a
/// COEXISTING byte-identical COPY (two distinct files share one BLAKE3). A
/// `file_ref` (NTFS MFT id) match is only VOLUME-LOCAL, so two distinct files
/// on different volumes (an external / SD / NAS drive scanned into the same
/// library), or two hardlinks to one file, can collide on the same ref — the
/// old `by_ref` short-circuit healed those unconditionally. Healing a
/// coexisting file steals the original's row and, via `UPDATE OR REPLACE`,
/// FK-cascades its tags/faces away — silent data loss. The old-path-gone gate
/// keeps coexisting files as distinct rows while still healing every real move
/// (whose old path is, by definition, gone). `symlink_metadata` (not
/// `metadata`) so a dangling symlink still counts as present and is not treated
/// as a move. (`_by_ref` is retained for the call site's tuple; the decision no
/// longer depends on it.)
fn heal_candidate_moved(_by_ref: bool, old_path: &str) -> bool {
    std::fs::symlink_metadata(crate::util::path_safety::to_extended_length(
        std::path::Path::new(old_path),
    ))
    .is_err()
}

const INSERT_CLIP_SQL: &str = r#"
    INSERT INTO clip_embeddings (file_id, embedding, model)
    VALUES (?1, ?2, ?3)
    ON CONFLICT(file_id) DO UPDATE SET
        embedding = excluded.embedding,
        model     = excluded.model
"#;

const INSERT_FACE_SQL: &str = r#"
    INSERT INTO face_prints (file_id, print_data, bbox, arcface_embedding, face_quality)
    VALUES (?1, ?2, ?3, ?4, ?5)
"#;

/// Convert a slice of f32 to little-endian bytes for BLOB storage.
/// Matches the macOS GRDB layout exactly (CoreML / ORT both produce
/// host-endian f32 → we always normalize to LE on the way to disk so a
/// macOS DB opens cleanly on Windows even if endianness ever drifts).
fn floats_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Encode a 112×112 RGB crop as JPEG and write to face_crops/<face_id>.jpg.
/// Cheap (37 KB raw → ~5 KB JPEG @ q85). Lets the People tab card render
/// real faces instead of placeholder gray circles.
fn save_face_crop(face_id: i64, crop_rgb_112: &[u8]) -> anyhow::Result<()> {
    use anyhow::Context;
    let dir = crate::paths::faces_dir().context("resolving faces dir")?;
    std::fs::create_dir_all(&dir).ok();
    let dest = dir.join(format!("{face_id}.jpg"));
    let img: image::ImageBuffer<image::Rgb<u8>, _> =
        image::ImageBuffer::from_raw(112, 112, crop_rgb_112.to_vec())
            .context("face crop bytes don't match 112x112")?;
    let dyn_img = image::DynamicImage::ImageRgb8(img);
    let mut bytes = Vec::with_capacity(8 * 1024);
    dyn_img
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .context("encode face crop JPEG")?;
    std::fs::write(&dest, &bytes).with_context(|| format!("write {}", dest.display()))?;
    Ok(())
}

/// Best-effort removal of a face crop JPEG (face_crops/<face_id>.jpg) orphaned
/// by a faces_evaluated re-process. Silent on any error — a leftover crop is
/// cosmetic disk use, never a correctness issue.
fn remove_face_crop(face_id: i64) {
    if let Ok(dir) = crate::paths::faces_dir() {
        let _ = std::fs::remove_file(dir.join(format!("{face_id}.jpg")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::discovery::FileKind;
    use rusqlite::OptionalExtension; // .optional() in ingest_with_heal (lib code no longer uses it)
    use std::path::PathBuf;

    /// Minimal mirror of the per-file body in `flush`. Exercises the
    /// real INSERT_FILE_SQL constant under test so any drift in the
    /// ON CONFLICT clause shows up here. Skips the embedding/face/ocr
    /// branches — they have their own contracts; this asserts the
    /// files-table de-dup contract specifically.
    fn insert_one(conn: &Connection, f: &TaggedFile) -> Result<()> {
        let path_text = f.path.to_string_lossy();
        let path_hash = crate::util::path_safety::stable_path_hash(&path_text);
        let extension = f
            .path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        conn.execute(
            INSERT_FILE_SQL,
            params![
                path_text,
                path_hash,
                f.size_bytes as i64,
                None::<f64>,
                f.modified_unix,
                f.scanned_unix,
                f.kind.as_str(),
                extension,
                f.phash,
                None::<f64>,
                f.has_faces as i64,
                f.has_text as i64,
                f.camera_model,
                f.location_lat,
                f.location_lon,
                f.failed as i64,
                f.error_message,
                f.content_hash.as_ref().map(|h| h.as_slice()),
                f.file_ref.map(|r| r as i64),
            ],
        )?;
        Ok(())
    }

    fn fixture(path: &str) -> TaggedFile {
        TaggedFile {
            path: PathBuf::from(path),
            kind: FileKind::Image,
            size_bytes: 1234,
            modified_unix: 1_700_000_000.0,
            created_unix: None,
            scanned_unix: 1_700_000_100.0,
            has_faces: false,
            faces: vec![],
            has_text: false,
            ocr_text: None,
            phash: None,
            aesthetic: None,
            image_width: 0,
            image_height: 0,
            clip_embedding: None,
            camera_model: None,
            location_lat: None,
            location_lon: None,
            vision_ms: 0.0,
            clip_ms: 0.0,
            total_ms: 0.0,
            failed: false,
            error_message: None,
            file_ref: None,
            content_hash: None,
            text_embedding: None,
            doc_text: None,
            tags: vec![],
            faces_evaluated: false,
            ocr_stage_ran: false,
            doc_stage_ran: false,
            tags_evaluated: true,
        }
    }

    fn in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).expect("migrations apply");
        conn
    }

    /// Re-ingesting the same path twice produces exactly one row. Guards
    /// against the ON CONFLICT clause regressing to INSERT OR IGNORE.
    #[test]
    fn duplicate_path_resolves_to_single_row() {
        let conn = in_memory_db();
        let f = fixture(r"C:\Users\adam\Pictures\IMG_0001.jpg");
        insert_one(&conn, &f).unwrap();
        insert_one(&conn, &f).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    /// `INSERT_FILE_RETURNING_ID_SQL` must yield the row id on BOTH the
    /// freshly-inserted and ON CONFLICT DO UPDATE branches. The hot-path
    /// flush relies on this — if RETURNING only fired on insert, every
    /// repeat-scan row would error with QueryReturnedNoRows. Guards the
    /// V15.9 redundant-SELECT elimination.
    #[test]
    fn insert_returning_id_yields_same_id_on_conflict() {
        let conn = in_memory_db();
        let f = fixture(r"C:\Users\adam\Pictures\IMG_RETURNING.jpg");
        let path_text = f.path.to_string_lossy();
        let path_hash = crate::util::path_safety::stable_path_hash(&path_text);
        let extension = f.path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
        let bind = |f: &TaggedFile| {
            let path_text = f.path.to_string_lossy().to_string();
            (path_text, path_hash, f.size_bytes as i64, None::<f64>,
             f.modified_unix, f.scanned_unix, f.kind.as_str().to_string(),
             extension.clone(), f.phash, None::<f64>,
             f.has_faces as i64, f.has_text as i64,
             f.camera_model.clone(), f.location_lat, f.location_lon,
             f.failed as i64, f.error_message.clone(),
             f.content_hash.as_ref().map(|h| h.to_vec()), f.file_ref.map(|r| r as i64))
        };
        let row = bind(&f);
        let id1: i64 = conn.query_row(
            INSERT_FILE_RETURNING_ID_SQL,
            params![row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7,
                    row.8, row.9, row.10, row.11, row.12, row.13, row.14, row.15, row.16,
                    row.17, row.18],
            |r| r.get(0),
        ).expect("first insert returns id");
        let id2: i64 = conn.query_row(
            INSERT_FILE_RETURNING_ID_SQL,
            params![row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7,
                    row.8, row.9, row.10, row.11, row.12, row.13, row.14, row.15, row.16,
                    row.17, row.18],
            |r| r.get(0),
        ).expect("ON CONFLICT branch must also return id");
        assert_eq!(id1, id2, "RETURNING must yield stable id across insert + update");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    /// ON CONFLICT must UPDATE (not skip) so a rescan with new
    /// size/modified_at writes them. Guards against INSERT OR IGNORE.
    #[test]
    fn duplicate_path_updates_size_and_modified() {
        let conn = in_memory_db();
        let mut f = fixture(r"C:\a.jpg");
        insert_one(&conn, &f).unwrap();
        f.size_bytes = 9999;
        f.modified_unix = 1_800_000_000.0;
        insert_one(&conn, &f).unwrap();
        let (size, modified): (i64, f64) = conn
            .query_row(
                "SELECT size_bytes, modified_at FROM files WHERE path_text = ?1",
                params![r"C:\a.jpg"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(size, 9999);
        assert!((modified - 1_800_000_000.0).abs() < 0.5);
    }

    proptest::proptest! {
        /// Arbitrary insert mix (with intentional duplicates) → row count
        /// must equal the number of distinct paths. Both the scan resume
        /// cursor and the People-tab dedup logic rely on path_text being
        /// unique.
        #[test]
        fn row_count_equals_distinct_paths(
            // Generate a small set of candidate paths…
            paths in proptest::collection::vec(r"C:\\test\\[a-z0-9]{1,8}\\f\.jpg", 1..6),
            // …then sample with repetition to force duplicates.
            order in proptest::collection::vec(0usize..6, 1..50),
        ) {
            let conn = in_memory_db();
            for idx in &order {
                let path = &paths[idx % paths.len()];
                insert_one(&conn, &fixture(path)).unwrap();
            }
            let distinct: std::collections::HashSet<&String> = order
                .iter()
                .map(|i| &paths[i % paths.len()])
                .collect();
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                .unwrap();
            proptest::prop_assert_eq!(n as usize, distinct.len());
        }

        /// Embedding BLOB round-trip must be byte-for-byte lossless and
        /// little-endian on every host. Reading via f32::from_le_bytes
        /// matches what the C# app and the macOS engine do; any future
        /// switch to to_ne_bytes would silently corrupt embeddings when
        /// the same DB file moves between architectures.
        ///
        /// We generate via u32 → f32::from_bits so NaN bit patterns are
        /// in scope: byte-level round-trip must preserve NaN payloads
        /// too, even though value equality wouldn't. We compare on bit
        /// patterns rather than f32 equality for that reason.
        #[test]
        fn embedding_le_bytes_round_trip(
            bits in proptest::collection::vec(proptest::num::u32::ANY, 1..520),
        ) {
            let values: Vec<f32> = bits.iter().copied().map(f32::from_bits).collect();
            let bytes = floats_to_le_bytes(&values);
            proptest::prop_assert_eq!(bytes.len(), values.len() * 4);
            let decoded: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            proptest::prop_assert_eq!(decoded.len(), values.len());
            for (i, (a, b)) in decoded.iter().zip(values.iter()).enumerate() {
                proptest::prop_assert_eq!(
                    a.to_bits(), b.to_bits(),
                    "mismatch at index {}", i,
                );
            }
        }
    }

    /// 512-d zero vector → 2048 zero bytes; matches the embedding column
    /// shape MobileCLIP and ArcFace both produce. Guards against a future
    /// Vec::with_capacity bug where capacity is allocated but data is not
    /// written.
    #[test]
    fn embedding_le_bytes_zero_vector() {
        let v = vec![0.0_f32; 512];
        let bytes = floats_to_le_bytes(&v);
        assert_eq!(bytes.len(), 2048);
        assert!(bytes.iter().all(|&b| b == 0));
    }

    // ---- B1 rename-heal data-loss regression --------------------------------

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTR: AtomicU64 = AtomicU64::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "fileid_test_{}_{}_{}",
            tag,
            std::process::id(),
            n
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Mirror of the heal+insert per-file body in `flush`, exercising the
    /// real HEAL_LOOKUP_SQL / `heal_candidate_moved` / HEAL_UPDATE_SQL /
    /// INSERT_FILE_RETURNING_ID_SQL so the B1 guard is under test end-to-end.
    fn ingest_with_heal(conn: &Connection, f: &TaggedFile) -> i64 {
        let path_text = f.path.to_string_lossy();
        let path_hash = crate::util::path_safety::stable_path_hash(&path_text);
        let extension = f
            .path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if f.file_ref.is_some() || f.content_hash.is_some() {
            let ch_bytes = f.content_hash.as_ref().map(|h| h.as_slice());
            let healed: Option<(i64, String, bool)> = conn
                .query_row(
                    HEAL_LOOKUP_SQL,
                    params![f.file_ref.map(|r| r as i64), ch_bytes, path_text.as_ref()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)),
                )
                .optional()
                .unwrap();
            if let Some((id, old_path, by_ref)) = healed {
                if heal_candidate_moved(by_ref, &old_path) {
                    conn.execute(HEAL_UPDATE_SQL, params![path_text, path_hash, id])
                        .unwrap();
                }
            }
        }
        conn.query_row(
            INSERT_FILE_RETURNING_ID_SQL,
            params![
                path_text,
                path_hash,
                f.size_bytes as i64,
                None::<f64>,
                f.modified_unix,
                f.scanned_unix,
                f.kind.as_str(),
                extension,
                f.phash,
                None::<f64>,
                f.has_faces as i64,
                f.has_text as i64,
                f.camera_model,
                f.location_lat,
                f.location_lon,
                f.failed as i64,
                f.error_message,
                f.content_hash.as_ref().map(|h| h.as_slice()),
                f.file_ref.map(|r| r as i64),
            ],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// The `by_ref` flag must be 1 only for a `file_ref` match and 0 (never
    /// SQL NULL) for a content_hash-only match — even when the incoming
    /// file_ref is NULL but the matched row has one.
    #[test]
    fn heal_lookup_flags_ref_match_but_not_hash_only() {
        let conn = in_memory_db();
        let mut a = fixture(r"C:\lib\old\IMG.jpg");
        a.content_hash = Some([7u8; 32]);
        a.file_ref = Some(0xABCD);
        ingest_with_heal(&conn, &a);

        let by_ref: bool = conn
            .query_row(
                HEAL_LOOKUP_SQL,
                params![Some(0xABCDu64), Some([7u8; 32].as_slice()), r"C:\lib\new\IMG.jpg"],
                |r| Ok(r.get::<_, i64>(2)? != 0),
            )
            .unwrap();
        assert!(by_ref, "file_ref match must set by_ref");

        // Incoming file_ref NULL, matched only via content_hash → by_ref = 0,
        // and crucially not NULL (which would break r.get::<_, i64>).
        let by_ref_none: bool = conn
            .query_row(
                HEAL_LOOKUP_SQL,
                params![None::<u64>, Some([7u8; 32].as_slice()), r"C:\lib\new\IMG.jpg"],
                |r| Ok(r.get::<_, i64>(2)? != 0),
            )
            .unwrap();
        assert!(!by_ref_none, "content_hash-only match must clear by_ref");
    }

    /// B1 core: two byte-identical files that COEXIST (a copy, not a move)
    /// must each get their own row. Before the fix the second file's
    /// content_hash heal stole the first's row and dropped it from the
    /// library.
    #[test]
    fn coexisting_byte_identical_copies_stay_distinct_rows() {
        let dir = unique_tmp_dir("b1_copy");
        let orig = dir.join("IMG_1558.HEIC");
        std::fs::write(&orig, b"same-bytes").unwrap();
        let copy = dir.join("IMG_1558(1).HEIC");
        std::fs::write(&copy, b"same-bytes").unwrap();

        let conn = in_memory_db();
        let mut a = fixture(orig.to_str().unwrap());
        a.content_hash = Some([0x11; 32]);
        a.file_ref = Some(1001);
        let id_a = ingest_with_heal(&conn, &a);

        let mut b = fixture(copy.to_str().unwrap());
        b.content_hash = Some([0x11; 32]); // identical bytes → identical hash
        b.file_ref = Some(2002); // a DISTINCT on-disk file → distinct MFT ref
        let id_b = ingest_with_heal(&conn, &b);

        assert_ne!(id_a, id_b, "copy must not steal the original's row");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "both coexisting byte-identical files must be catalogued");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B1: a genuine MOVE (content_hash match, old path gone from disk) heals
    /// to a single row, preserving the row id and its FK-linked tags.
    #[test]
    fn genuine_move_heals_and_preserves_fks() {
        let dir = unique_tmp_dir("b1_move");
        let new_path = dir.join("moved.jpg");
        std::fs::write(&new_path, b"payload").unwrap();
        // Old path is never created on disk → "gone" → a real move.
        let old_path = dir.join("gone").join("orig.jpg");

        let conn = in_memory_db();
        let mut a = fixture(old_path.to_str().unwrap());
        a.content_hash = Some([0x22; 32]);
        a.file_ref = None; // cross-volume move: only content_hash identity
        let id_a = ingest_with_heal(&conn, &a);
        conn.execute(
            "INSERT INTO tags (file_id, tag, source, score) VALUES (?1, 'cat', 'auto', 0.9)",
            params![id_a],
        )
        .unwrap();

        let mut b = fixture(new_path.to_str().unwrap());
        b.content_hash = Some([0x22; 32]);
        b.file_ref = None;
        let id_b = ingest_with_heal(&conn, &b);

        assert_eq!(id_a, id_b, "a real move must re-bind the SAME row id");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the moved file is one row, not two");
        let tag_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tags WHERE file_id = ?1 AND tag = 'cat'",
                params![id_b],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tag_count, 1, "FK-linked tag must survive the heal");
        let healed_path: String = conn
            .query_row("SELECT path_text FROM files WHERE id = ?1", params![id_b], |r| r.get(0))
            .unwrap();
        assert_eq!(healed_path, new_path.to_string_lossy());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A true rename detected via file_ref (NTFS MFT id) heals when the old
    /// path is GONE from disk — a real move always leaves its old path absent.
    #[test]
    fn file_ref_rename_with_old_path_gone_heals() {
        let dir = unique_tmp_dir("b1_ref_rename");
        let new_path = dir.join("after.png");
        std::fs::write(&new_path, b"x").unwrap();
        // Old path is never created on disk → "gone" → a real move.
        let old_path = dir.join("gone").join("before.png");

        let conn = in_memory_db();
        let mut a = fixture(old_path.to_str().unwrap());
        a.file_ref = Some(0xDEAD_BEEF);
        let id_a = ingest_with_heal(&conn, &a);

        let mut b = fixture(new_path.to_str().unwrap());
        b.file_ref = Some(0xDEAD_BEEF); // same MFT ref + old path gone → rename
        let id_b = ingest_with_heal(&conn, &b);

        assert_eq!(id_a, id_b, "file_ref match with old path gone is a rename → heal");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cross-volume / hardlink safety: a file_ref match while the OLD path is
    /// STILL present on disk is NOT a rename. The NTFS MFT reference is only
    /// volume-local, so two distinct files on different volumes (or two
    /// hardlinks to one file) can collide on the same ref. Healing such a
    /// collision would re-bind one file's row to the other's path and, via
    /// UPDATE OR REPLACE, FK-cascade the loser's tags/faces away — silent data
    /// loss. The old-path-gone gate keeps them as two distinct rows.
    #[test]
    fn file_ref_collision_with_both_paths_present_stays_distinct() {
        let dir = unique_tmp_dir("b1_ref_collision");
        let old_path = dir.join("before.png");
        std::fs::write(&old_path, b"x").unwrap(); // old path STILL present
        let new_path = dir.join("after.png");
        std::fs::write(&new_path, b"y").unwrap(); // a DISTINCT coexisting file

        let conn = in_memory_db();
        let mut a = fixture(old_path.to_str().unwrap());
        a.file_ref = Some(0xDEAD_BEEF);
        let id_a = ingest_with_heal(&conn, &a);

        let mut b = fixture(new_path.to_str().unwrap());
        b.file_ref = Some(0xDEAD_BEEF); // colliding ref, but old file still exists
        let id_b = ingest_with_heal(&conn, &b);

        assert_ne!(id_a, id_b, "a colliding ref with the old file present must not collapse");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "two coexisting files must stay distinct rows");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ENG-18: an NTFS `file_ref` with the high bit set (a non-zero sequence
    /// number lives in the top 16 bits) exceeds `i64::MAX`. rusqlite's
    /// `ToSql for u64` rejects values above `i64::MAX`, so binding the raw u64
    /// errored and the `?` aborted the entire flush batch — losing the whole
    /// catalog. We now bitcast `u64 -> i64` losslessly at every bind site; the
    /// insert must succeed and the value must still round-trip through the
    /// heal lookup (same MFT ref → rename → heal, not a duplicate row).
    #[test]
    fn high_bit_file_ref_does_not_abort_insert() {
        let dir = unique_tmp_dir("eng18_ref");
        let hi: u64 = 0xFFFF_0000_0000_0001; // > i64::MAX
        assert!(hi > i64::MAX as u64, "fixture must exercise the high-bit path");

        let conn = in_memory_db();
        let mut a = fixture(dir.join("a.png").to_str().unwrap());
        a.file_ref = Some(hi);
        let id_a = ingest_with_heal(&conn, &a); // must NOT error on the u64 bind

        let mut b = fixture(dir.join("b.png").to_str().unwrap());
        b.file_ref = Some(hi); // same MFT ref at a new path → a true rename
        let id_b = ingest_with_heal(&conn, &b);

        assert_eq!(id_a, id_b, "high-bit file_ref must round-trip and heal");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "rename must not create a duplicate row");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
