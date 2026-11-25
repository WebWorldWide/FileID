// DBWriter drains the Tagging → DB channel in adaptive-size or 200 ms batches on the single SQLite writer connection (WAL allows one writer).
// Every insert and the resume cursor update share one transaction, so a crash leaves no partial state. ocr_fts/doc_fts are maintained by the v15 sync triggers, never written here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::mpsc;

use crate::coordinator::ScanCoordinator;
use crate::pipeline::tagging::TaggedFile;
use crate::platform::{dbwriter_batch_size_for, memory_tier};

/// Fallback flush size if adaptive sizing yields nothing; `current_batch_size()` picks Low=64, Balanced=250, High=500.
const BATCH_SIZE_FALLBACK: usize = 250;
const FLUSH_INTERVAL: Duration = Duration::from_millis(200);

/// Re-evaluated at the top of each batch so a memory-pressure shift downshifts the batch before an OOM.
fn current_batch_size() -> usize {
    dbwriter_batch_size_for(memory_tier()).max(1)
}

#[derive(Debug, Clone, Copy)]
struct DeferredFaceCrop {
    face_id: i64,
    file_index: usize,
    face_index: usize,
}

fn take_deferred_face_crops(
    buffer: &mut [TaggedFile],
    deferred: Vec<DeferredFaceCrop>,
) -> Vec<(i64, Vec<u8>)> {
    deferred
        .into_iter()
        .filter_map(|item| {
            buffer
                .get_mut(item.file_index)?
                .faces
                .get_mut(item.face_index)?
                .crop_rgb_112
                .take()
                .map(|crop| (item.face_id, crop))
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct BatchStats {
    pub batch_index: u32,
    pub files_in_batch: u32,
    pub processed_total: u64,
    /// Cumulative processed Image/Video/Model files (the expensive vision path), for the kind-aware ETA: a blended files/sec oscillates by an order of magnitude on mixed libraries.
    pub processed_heavy: u64,
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
    rename_heal_enabled: bool,
}

impl DbWriter {
    pub fn new(conn: Arc<Mutex<Connection>>, coordinator: ScanCoordinator) -> Self {
        // A database with no file rows has no old path to heal and no legacy hash recipe; frozen at scan start so rows inserted by this scan don't trigger pointless heal queries and a second disk read per batch.
        let rename_heal_enabled = conn
            .lock()
            .query_row("SELECT EXISTS(SELECT 1 FROM files LIMIT 1)", [], |row| {
                row.get::<_, bool>(0)
            })
            .unwrap_or(true);
        Self { conn, coordinator, rename_heal_enabled }
    }

    /// Drains the receiver until the channel closes, flushing on `BATCH_SIZE` rows or `FLUSH_INTERVAL` after the first row. Returns total processed and failed counts.
    pub async fn run<F>(self, mut input: mpsc::Receiver<TaggedFile>, mut on_batch: F)
        -> Result<(u64, u64)>
    where
        F: FnMut(BatchStats),
    {
        let mut buffer: Vec<TaggedFile> = Vec::with_capacity(BATCH_SIZE_FALLBACK);
        let mut deadline: Option<Instant> = None;
        let mut total: u64 = 0;
        let mut heavy: u64 = 0;
        let mut failed: u64 = 0;
        let mut batch_index: u32 = 0;
        let mut current_target = current_batch_size();
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
                        let stats = self.flush_off_runtime(&mut buffer, &mut total, &mut heavy, &mut failed, batch_index)?;
                        batch_index += 1;
                        deadline = None;
                        on_batch(stats);
                    }
                }
                Ok(None) => {
                    if !buffer.is_empty() {
                        let stats = self.flush_off_runtime(&mut buffer, &mut total, &mut heavy, &mut failed, batch_index)?;
                        on_batch(stats);
                    }
                    break;
                }
                Err(_) => {
                    if !buffer.is_empty() {
                        let stats = self.flush_off_runtime(&mut buffer, &mut total, &mut heavy, &mut failed, batch_index)?;
                        batch_index += 1;
                        deadline = None;
                        on_batch(stats);
                    }
                }
            }
            if self.coordinator.is_cancelled() {
                // Flush rows that finished the paid-for ML pipeline before the cancel but hadn't hit a batch boundary; otherwise up to current_target-1 tagged files are re-processed next scan (mirrors the drain arms above).
                if !buffer.is_empty() {
                    let stats = self.flush_off_runtime(&mut buffer, &mut total, &mut heavy, &mut failed, batch_index)?;
                    on_batch(stats);
                }
                break;
            }
        }
        Ok((total, failed))
    }

    pub fn rename_heal_enabled(&self) -> bool {
        self.rename_heal_enabled
    }

    /// `flush` is synchronous SQLite + filesystem work; hand this worker's other tasks off for its duration instead of stalling them.
    fn flush_off_runtime(
        &self,
        buffer: &mut Vec<TaggedFile>,
        total: &mut u64,
        heavy: &mut u64,
        failed: &mut u64,
        batch_index: u32,
    ) -> Result<BatchStats> {
        let mut flush = || self.flush(buffer, total, heavy, failed, batch_index);
        match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
            Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(flush),
            _ => flush(),
        }
    }

    /// Per file, the `(id, old_path)` of identity-matched rows at a different path whose old path is gone (a genuine move), file_ref matches first.
    fn moved_heal_candidates(&self, buffer: &[TaggedFile]) -> Result<Vec<Vec<(i64, String)>>> {
        let mut per_file: Vec<Vec<(i64, String)>> = Vec::with_capacity(buffer.len());
        {
            let conn = self.conn.lock();
            let mut heal_lookup = conn
                .prepare_cached(HEAL_LOOKUP_SQL)
                .context("preparing rename-heal lookup")?;
            for f in buffer {
                if f.file_ref.is_none() && f.content_hash.is_none() {
                    per_file.push(Vec::new());
                    continue;
                }
                let legacy = f.legacy_hashes.as_ref();
                let rows = heal_lookup
                    .query_map(
                        params![
                            f.file_ref.map(|r| r as i64),
                            f.content_hash.as_ref().map(|h| h.as_slice()),
                            f.path.to_string_lossy().as_ref(),
                            legacy.map(|h| h.v2.as_slice()),
                            f.size_bytes as i64,
                            legacy.and_then(|h| h.v1.as_ref()).map(|h| h.as_slice())
                        ],
                        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
                    )
                    .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                    .with_context(|| {
                        format!("rename-heal lookup for {}", crate::platform::redact_path_for_log(&f.path))
                    })?;
                per_file.push(rows);
            }
        } // writer lock released before any filesystem IO
        let mut gone: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        for (_, old) in per_file.iter().flatten() {
            if !gone.contains_key(old) {
                gone.insert(old.clone(), heal_candidate_moved(old));
            }
        }
        for candidates in &mut per_file {
            candidates.retain(|(_, old)| gone[old]);
        }
        Ok(per_file)
    }

    fn flush(
        &self,
        buffer: &mut Vec<TaggedFile>,
        total: &mut u64,
        heavy: &mut u64,
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

        // Face-crop ids orphaned by faces_evaluated re-processing are pruned AFTER commit, never inside the tx, so a rollback (which restores the old face_prints) can't leave them crop-less.
        let mut crop_ids_to_prune: Vec<i64> = Vec::new();
        // Crop extraction by move happens after commit: JPEG encode + fs::write stay outside the transaction and writer lock.
        let mut deferred_crops: Vec<DeferredFaceCrop> = Vec::new();
        let mut crop_copies: Vec<(i64, i64)> = Vec::new();
        let mut reused_twins = 0u32;

        // Heal candidates are resolved BEFORE the writer lock: `heal_candidate_moved()` stats the old path, which on a dead/slow mount (unmounted NAS, pulled drive) can block for the full fs/SMB timeout and would stall the single writer for the whole batch. Look up under a brief lock, stat with the lock released, keep (per file, file_ref matches first) only candidates whose old path is gone.
        // Legacy BLAKE3 digests come from the decoder's own read (`TaggedFile::legacy_hashes`), so the writer never re-reads a file.
        let heal_candidates = if self.rename_heal_enabled {
            self.moved_heal_candidates(buffer)?
        } else {
            vec![Vec::new(); buffer.len()]
        };
        // A failure for a file deleted or renamed after the walk listed it: a new
        // row would name a missing file and, stamped with this scan's time, survive
        // the orphan sweep. Checked off the writer lock for the same reason as above.
        let vanished: Vec<bool> = buffer
            .iter()
            .map(|f| f.failed && !f.heal_only && matches!(f.path.try_exists(), Ok(false)))
            .collect();

        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction().context("opening tx")?;
        {
            // INSERT ... RETURNING id (SQLite 3.35+) replaces a per-row SELECT and yields the id on both insert and ON CONFLICT DO UPDATE.
            let mut file_stmt = tx
                .prepare_cached(INSERT_FILE_RETURNING_ID_SQL)
                .context("preparing file insert (RETURNING)")?;
            let mut heal_recheck_stmt = tx
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 AND path_text = ?2)")
                .context("preparing rename-heal recheck")?;
            let mut row_at_path_stmt = tx
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM files WHERE path_text = ?1)")
                .context("preparing row-at-path check")?;
            let mut heal_only_check_stmt = tx
                .prepare_cached(HEAL_ONLY_CHECK_SQL)
                .context("preparing heal-only check")?;
            let mut heal_only_touch_stmt = tx
                .prepare_cached("UPDATE files SET scanned_at = ?2 WHERE id = ?1")
                .context("preparing heal-only touch")?;
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
            // Records that a doc/pdf's text stage ran so the BGE backfill stops re-walking a text-less doc; a separate idempotent UPDATE keeps it off the big positional upsert.
            let mut text_stage_stmt = tx
                .prepare_cached(
                    "UPDATE files SET text_stage_done = 1 WHERE id = ?1 AND text_stage_done = 0",
                )
                .context("preparing text_stage_done update")?;
            let mut face_delete = tx
                .prepare_cached("DELETE FROM face_prints WHERE file_id = ?1 RETURNING id")
                .context("preparing face delete")?;
            let mut face_stmt = tx
                .prepare_cached(INSERT_FACE_SQL)
                .context("preparing face insert")?;
            let mut vision_face_stmt = tx
                .prepare_cached("INSERT INTO face_prints (file_id, print_data, bbox, face_quality, excluded) VALUES (?1, X'', ?2, ?3, ?4)")
                .context("preparing Vision face insert")?;
            let mut tag_delete = tx
                .prepare_cached("DELETE FROM tags WHERE file_id = ?1 AND source = 'auto'")
                .context("preparing tag delete")?;
            let mut tag_insert = tx
                .prepare_cached("INSERT OR REPLACE INTO tags (file_id, tag, source, score) VALUES (?1, ?2, 'auto', ?3)")
                .context("preparing tag insert")?;
            // ocr_fts/doc_fts are owned by the v15 sync triggers, so only content tables are touched. Explicit DELETE + INSERT, not INSERT OR REPLACE: REPLACE's implicit delete fires the AFTER DELETE trigger only with recursive triggers enabled, stranding the old text's FTS postings.
            let mut ocr_text_stmt = tx
                .prepare_cached("INSERT INTO ocr_text (file_id, text, handwritten) SELECT ?1, ?2, ?3 WHERE NOT EXISTS (SELECT 1 FROM ocr_text WHERE file_id = ?1)")
                .context("preparing ocr_text insert")?;
            let mut ocr_text_delete = tx
                .prepare_cached("DELETE FROM ocr_text WHERE file_id = ?1 AND source = 'ocr'")
                .context("preparing ocr_text delete")?;
            let mut doc_text_stmt = tx
                .prepare_cached("INSERT INTO doc_text (file_id, text) VALUES (?1, ?2)")
                .context("preparing doc_text insert")?;
            let mut doc_text_delete = tx
                .prepare_cached("DELETE FROM doc_text WHERE file_id = ?1")
                .context("preparing doc_text delete")?;
            let mut twin_lookup = tx
                .prepare_cached(TWIN_LOOKUP_SQL)
                .context("preparing twin lookup")?;
            let mut twin_children = tx
                .prepare_cached(
                    "SELECT EXISTS (SELECT 1 FROM tags WHERE file_id = ?1 AND source = 'auto')                      OR EXISTS (SELECT 1 FROM clip_embeddings WHERE file_id = ?1)",
                )
                .context("preparing twin children check")?;
            let mut own_clip = tx.prepare_cached(OWN_CLIP_SQL).context("preparing own clip read")?;
            let mut own_complete_stmt = tx
                .prepare_cached(&format!(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE path_text = ?1 AND content_hash = ?2 AND {} \
                     AND NOT EXISTS (SELECT 1 FROM scan_catchup c WHERE c.file_id = files.id))",
                    crate::pipeline::twin_reuse::COMPLETE_IMAGE_SQL
                ))
                .context("preparing own row check")?;
            let mut has_clip = tx
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM clip_embeddings WHERE file_id = ?1)")
                .context("preparing twin vector check")?;
            let mut own_clip_restore = tx.prepare_cached(OWN_CLIP_RESTORE_SQL).context("preparing own clip restore")?;
            let mut twin_files = tx.prepare_cached(TWIN_FILES_SQL).context("preparing twin files copy")?;
            let mut twin_tags = tx.prepare_cached(TWIN_TAGS_SQL).context("preparing twin tags copy")?;
            let mut twin_clip = tx.prepare_cached(TWIN_CLIP_SQL).context("preparing twin clip copy")?;
            let mut twin_ocr = tx.prepare_cached(TWIN_OCR_SQL).context("preparing twin ocr copy")?;
            let mut twin_faces = tx.prepare_cached(TWIN_FACES_SQL).context("preparing twin faces read")?;
            let mut twin_face_insert =
                tx.prepare_cached(TWIN_FACE_INSERT_SQL).context("preparing twin face copy")?;
            let mut twin_unavailable = tx
                .prepare_cached("UPDATE files SET failed = 1, error_message = ?2 WHERE id = ?1")
                .context("preparing twin unavailable update")?;
            let mut catalog_revision = tx
                .prepare_cached(CATALOG_REVISION_SQL)
                .context("preparing catalog revision")?;
            let mut failure_record = tx
                .prepare_cached(crate::pipeline::catch_up::RECORD_FAILURE_SQL)
                .context("preparing failure memory")?;
            let mut failure_clear = tx
                .prepare_cached(crate::pipeline::catch_up::CLEAR_FAILURE_SQL)
                .context("preparing failure memory clear")?;
            let mut owed_write = tx.prepare_cached(OWED_WRITE_SQL).context("preparing catch-up marker")?;
            let mut owed_clear = tx
                .prepare_cached("DELETE FROM scan_catchup WHERE file_id = ?1")
                .context("preparing catch-up marker clear")?;
            let mut owed_copy = tx.prepare_cached(OWED_COPY_SQL).context("preparing catch-up marker copy")?;
            for (i, f) in buffer.iter().enumerate() {
                let insert_started = Instant::now();
                let path_text = f.path.to_string_lossy();
                // Path redaction is computed lazily inside the error-context closures (which almost never run): redact_path_for_log allocates and looks up paths::root(), wasted on every row of a 140 files/s flush. The redacted path is what lands in the log and IPC on a real error.
                let path_hash = crate::util::path_safety::stable_path_hash(&path_text);
                // NFC-normalized search shadow of the path (v16 contract): macOS writers store `precomposedStringWithCanonicalMapping`, so an NFD filename (Mac/NAS/Dropbox) must be found by the app's NFC query. ASCII is the no-alloc identity case.
                let path_search = nfc_path_search(&path_text);
                let extension = f
                    .path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();

                // Rename/move heal: move an identity-matching row at a DIFFERENT path to the new path BEFORE the INSERT, so ON CONFLICT(path_text) updates the relocated row and keeps its id and every FK-linked row. Pre-pass candidates are re-checked: an earlier file in this batch may already have healed the same row (two copies of one moved file).
                let mut healable = None;
                for (id, old) in &heal_candidates[i] {
                    let still_there: bool = heal_recheck_stmt
                        .query_row(params![id, old], |r| r.get(0))
                        .context("rename-heal recheck")?;
                    // A file nobody analysed may only take over a row that
                    // still describes it: scanned cleanly, same size and date.
                    let trusted = !f.heal_only
                        || heal_only_check_stmt
                            .query_row(params![id, f.size_bytes as i64, f.modified_unix], |r| r.get(0))
                            .context("heal-only check")?;
                    if still_there && trusted {
                        healable = Some(*id);
                        break;
                    }
                }
                let mut healed = None;
                if let Some(id) = healable {
                    match heal_update_stmt.execute(params![path_text, path_hash, id, path_search, crate::db::name_sort::name_sort_key(&path_text)]) {
                        Ok(_) => {
                            healed = Some(id);
                            tracing::info!(
                                id,
                                new_path = %crate::platform::redact_path_for_log(&f.path),
                                "[RENAME-HEAL] re-bound existing row to new path"
                            );
                        }
                        // A content-identical COPY already occupies the new path: skip the heal rather than OR REPLACE-delete that row, which would orphan its FTS5 external-content index (ocr_fts/doc_fts have no delete triggers). The moved-away orphan keeps its old path for orphan-pruning; the INSERT ON CONFLICT updates the existing row.
                        Err(rusqlite::Error::SqliteFailure(sf, _))
                            if sf.code == rusqlite::ErrorCode::ConstraintViolation =>
                        {
                            tracing::debug!(
                                id,
                                "[RENAME-HEAL] new path already occupied by a copy; skipping heal"
                            );
                        }
                        Err(e) => {
                            Err::<(), _>(e).with_context(|| {
                                format!(
                                    "rename-heal update for {}",
                                    crate::platform::redact_path_for_log(&f.path)
                                )
                            })?;
                        }
                    }
                }

                if f.heal_only {
                    match healed {
                        Some(id) => {
                            heal_only_touch_stmt
                                .execute(params![id, f.scanned_unix])
                                .context("heal-only touch")?;
                        }
                        // The old path came back or the new one was taken since the walk: no row is made from an unanalysed file; the next scan sees it as new.
                        None => tracing::info!(
                            path = %crate::platform::redact_path_for_log(&f.path),
                            "[RENAME-HEAL] move could not be re-bound; the next scan analyses the file"
                        ),
                    }
                    *total += 1;
                    if matches!(
                        f.kind,
                        crate::pipeline::discovery::FileKind::Image
                            | crate::pipeline::discovery::FileKind::Video
                            | crate::pipeline::discovery::FileKind::Model
                    ) {
                        *heavy += 1;
                    }
                    continue;
                }

                if vanished[i]
                    && healed.is_none()
                    && !row_at_path_stmt
                        .query_row(params![path_text], |r| r.get::<_, bool>(0))
                        .context("row-at-path check")?
                {
                    *failed += 1;
                    *total += 1;
                    continue;
                }

                // Same bytes, changed date: the catalog drops a row's vector when size or date changes, so read it before the upsert and restore it below unless another row has these bytes.
                let kept_clip: Option<(Vec<u8>, Option<String>)> = match f.content_hash.as_ref() {
                    Some(hash) if f.reuse_twin => own_clip
                        .query_row(params![path_text, hash.as_slice()], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()
                        .with_context(|| format!("own clip read for {}", crate::platform::redact_path_for_log(&f.path)))?,
                    _ => None,
                };
                // The file's own row already holds an analysis of these bytes (a
                // touch or a restore changed only the date): it stays as it is, with
                // the people the user named, unless only another copy has a vector.
                let own_complete = match f.content_hash.as_ref() {
                    Some(hash) if f.reuse_twin => own_complete_stmt
                        .query_row(params![path_text, hash.as_slice()], |r| r.get::<_, bool>(0))
                        .with_context(|| format!("own row check for {}", crate::platform::redact_path_for_log(&f.path)))?,
                    _ => false,
                };

                let created_unix = f.created_unix.and_then(|t| crate::util::not_in_the_future(t, f.scanned_unix));
                let captured_from_exif = f.captured_from_exif && created_unix.is_some();
                let (file_id, stored_modified): (i64, Option<f64>) = file_stmt
                    .query_row(
                        params![
                            path_text,
                            path_hash,
                            f.size_bytes as i64,
                            created_unix,
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
                            path_search,
                            f.faces_evaluated,
                            f.ocr_stage_ran || f.doc_stage_ran,
                            captured_from_exif,
                            crate::db::name_sort::name_sort_key(&path_text),
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .with_context(|| format!("insert+id for {}", crate::platform::redact_path_for_log(&f.path)))?;
                let modified_unix = stored_modified.unwrap_or(f.modified_unix);

                if f.text_stage_done {
                    text_stage_stmt
                        .execute(params![file_id])
                        .with_context(|| format!("text_stage_done for {}", crate::platform::redact_path_for_log(&f.path)))?;
                }

                if let Some(emb) = &f.clip_embedding {
                    anyhow::ensure!(crate::models::clip_space::valid(emb), "Invalid CLIP embedding cannot be persisted");
                    let bytes = floats_to_le_bytes(emb);
                    clip_stmt
                        .execute(params![file_id, bytes, crate::models::clip_space::model_id()])
                        .with_context(|| format!("clip insert for {}", crate::platform::redact_path_for_log(&f.path)))?;
                }

                // BGE-small embeddings go to `text_embeddings` keyed by model so future embedders can coexist without table churn.
                if let Some(emb) = &f.text_embedding {
                    let bytes = floats_to_le_bytes(emb);
                    text_embed_stmt
                        .execute(params![file_id, bytes, "bge_small_en_v1_5"])
                        .with_context(|| format!("text_embeddings insert for {}", crate::platform::redact_path_for_log(&f.path)))?;
                }

                // Key the stale-face DELETE on whether the face stage ran, NOT `faces.is_empty()`: a zero-face re-process must clear orphaned face_prints (they pollute clusters), while a face-disabled / GPU-dead session leaves valid rows intact.
                if f.faces_evaluated {
                    // Capture the replaced face ids (DELETE ... RETURNING, one statement under the writer lock) so their orphaned crop JPEGs can be pruned: re-inserted faces get fresh AUTOINCREMENT ids, so otherwise face_crops/ grows unbounded across re-scans.
                    let stale_face_ids: Vec<i64> = face_delete
                        .query_map(params![file_id], |r| r.get::<_, i64>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .with_context(|| format!("face delete for {}", crate::platform::redact_path_for_log(&f.path)))?;
                    let source_revision = format!("{}:{}", f.size_bytes, modified_unix.to_bits());
                    // The Mac face pipeline records its own file revision when it
                    // extracts Vision faces; a portable one here would mark them stale.
                    if f.vision_faces.is_none() {
                        catalog_revision.execute(params![file_id, source_revision, f.scanned_unix])?;
                    }
                    for (face_index, face) in f.faces.iter().enumerate() {
                        let mut bbox = serde_json::json!({
                            "x": face.bbox[0],
                            "y": face.bbox[1],
                            "w": face.bbox[2],
                            "h": face.bbox[3],
                            "roll": face.roll,
                            "yaw": face.yaw,
                            "pitch": face.pitch,
                        });
                        if f.image_width > 0 && f.image_height > 0 {
                            bbox["coordinateSpace"] = serde_json::json!("pixel-top-left");
                            bbox["sourceWidth"] = serde_json::json!(f.image_width);
                            bbox["sourceHeight"] = serde_json::json!(f.image_height);
                        }
                        let bbox_json = bbox.to_string();
                        let arcface_bytes = floats_to_le_bytes(&face.embedding);
                        let norm = face.embedding.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
                        let model = face.embedding_model.as_deref().filter(|value| !value.is_empty() && value.len() <= 150 && face.embedding.len() == 128 && face.embedding.iter().all(|v| v.is_finite()) && (0.95..=1.05).contains(&norm));
                        // print_data legacy: same bytes as arcface_embedding so old code keeps working.
                        face_stmt
                            .execute(params![
                                file_id,
                                arcface_bytes.as_slice(),
                                bbox_json,
                                arcface_bytes.as_slice(),
                                face.quality as f64,
                                face.excluded as i64,
                                model,
                                model.map(|_| crate::commands::face_refresh::PORTABLE_PROCESSING),
                                model.map(|_| source_revision.as_str()),
                            ])
                            .with_context(|| format!("face insert for {}", crate::platform::redact_path_for_log(&f.path)))?;

                        if face.crop_rgb_112.is_some() {
                            deferred_crops.push(DeferredFaceCrop {
                                face_id: tx.last_insert_rowid(),
                                file_index: i,
                                face_index,
                            });
                        }
                    }
                    // Vision faces (Mac adapters scan): scan-time rows with no vector or namespace until the Mac face pipeline extracts them.
                    for face in f.vision_faces.iter().flatten() {
                        vision_face_stmt
                            .execute(params![file_id, face.bbox, face.quality, face.excluded])
                            .with_context(|| format!("Vision face insert for {}", crate::platform::redact_path_for_log(&f.path)))?;
                    }
                    // New AUTOINCREMENT ids never collide with the deleted ones, so every captured id is orphaned; the file delete waits until after commit so a rollback can't orphan crops.
                    crop_ids_to_prune.extend(stale_face_ids);
                }

                // Delete-then-conditional-insert, only when the OCR stage ran this session, never on the ambiguous skip path: it clears stale ocr_text (and via the v15 triggers ocr_fts) when a re-process yields empty text, and leaves prior text on skipped sessions. A Deep Analyze transcription (source 'vlm') outlives rescans of an unchanged file; the v25 trigger drops it when the file changes.
                if f.ocr_stage_ran {
                    ocr_text_delete
                        .execute(params![file_id])
                        .with_context(|| format!("ocr_text delete for {}", crate::platform::redact_path_for_log(&f.path)))?;
                    if let Some(text) = &f.ocr_text
                        && !text.trim().is_empty() {
                            ocr_text_stmt
                                .execute(params![file_id, text, f.ocr_handwritten])
                                .with_context(|| format!("ocr_text insert for {}", crate::platform::redact_path_for_log(&f.path)))?;
                        }
                }

                // Document text: same stage-ran-gated delete-then-insert as ocr_text.
                if f.doc_stage_ran {
                    doc_text_delete
                        .execute(params![file_id])
                        .with_context(|| format!("doc_text delete for {}", crate::platform::redact_path_for_log(&f.path)))?;
                    if let Some(text) = &f.doc_text
                        && !text.trim().is_empty() {
                            doc_text_stmt
                                .execute(params![file_id, text])
                                .with_context(|| format!("doc_text insert for {}", crate::platform::redact_path_for_log(&f.path)))?;
                        }
                }

                // Auto-tags: delete-then-reinsert only when the tagging stage ran, like faces_evaluated / ocr_stage_ran / doc_stage_ran. A timeout row or GPU-dead short-circuit emits an EMPTY `tags` vec, and an unconditional DELETE would wipe previously-persisted auto-tags (data loss). `source='user'` tags are untouched either way.
                if f.tags_evaluated {
                    tag_delete
                        .execute(params![file_id])
                        .with_context(|| format!("tag delete for {}", crate::platform::redact_path_for_log(&f.path)))?;
                    for (tag, score) in &f.tags {
                        let trimmed = tag.trim();
                        if trimmed.is_empty() { continue; }
                        tag_insert
                            .execute(params![file_id, trimmed, score.map(|s| s as f64)])
                            .with_context(|| format!("tag insert for {}", crate::platform::redact_path_for_log(&f.path)))?;
                    }
                }

                // A byte-identical image analyzed before: copy the twin row's tags, embedding, text and faces, keeping this file's own year and camera chips. With no twin row, a row with its own results keeps them; a bare one is retried next scan.
                if f.reuse_twin {
                    let twin: Option<i64> = match f.content_hash.as_ref() {
                        Some(hash) => twin_lookup
                            .query_row(params![hash.as_slice(), file_id], |r| r.get(0))
                            .optional()
                            .with_context(|| format!("twin lookup for {}", crate::platform::redact_path_for_log(&f.path)))?,
                        None => None,
                    };
                    let twin = match twin {
                        Some(_) if own_complete && kept_clip.is_some() => None,
                        Some(twin_id) if own_complete => has_clip
                            .query_row(params![twin_id], |r| r.get::<_, bool>(0))
                            .context("twin vector check")?
                            .then_some(twin_id),
                        twin => twin,
                    };
                    let redacted = || crate::platform::redact_path_for_log(&f.path);
                    if let Some(twin_id) = twin {
                        let stale_face_ids: Vec<i64> = face_delete
                            .query_map(params![file_id], |r| r.get::<_, i64>(0))?
                            .collect::<rusqlite::Result<Vec<_>>>()
                            .with_context(|| format!("twin face delete for {}", redacted()))?;
                        crop_ids_to_prune.extend(stale_face_ids);
                        tag_delete.execute(params![file_id]).with_context(|| format!("twin tag delete for {}", redacted()))?;
                        ocr_text_delete.execute(params![file_id]).with_context(|| format!("twin ocr delete for {}", redacted()))?;
                        twin_files.execute(params![file_id, twin_id]).with_context(|| format!("twin files copy for {}", redacted()))?;
                        twin_tags.execute(params![file_id, twin_id]).with_context(|| format!("twin tags copy for {}", redacted()))?;
                        for (tag, score) in &f.tags {
                            let trimmed = tag.trim();
                            if trimmed.is_empty() { continue; }
                            tag_insert
                                .execute(params![file_id, trimmed, score.map(|s| s as f64)])
                                .with_context(|| format!("twin extra tag for {}", redacted()))?;
                        }
                        twin_clip.execute(params![file_id, twin_id]).with_context(|| format!("twin clip copy for {}", redacted()))?;
                        twin_ocr.execute(params![file_id, twin_id]).with_context(|| format!("twin ocr copy for {}", redacted()))?;
                        let source_revision = format!("{}:{}", f.size_bytes, modified_unix.to_bits());
                        // The Mac scan (`vision_faces`) stores faces without vectors and leaves the file revision to the Mac face pipeline; its copies are stored the same way.
                        let mac_scan = f.vision_faces.is_some();
                        if !mac_scan {
                            catalog_revision.execute(params![file_id, source_revision, f.scanned_unix])?;
                        }
                        let faces: Vec<TwinFace> = twin_faces
                            .query_map(params![twin_id], |r| {
                                Ok(TwinFace {
                                    id: r.get(0)?,
                                    print_data: r.get(1)?,
                                    bbox: r.get(2)?,
                                    embedding: r.get(3)?,
                                    quality: r.get(4)?,
                                    excluded: r.get(5)?,
                                    model: r.get(6)?,
                                    processing: r.get(7)?,
                                })
                            })?
                            .collect::<rusqlite::Result<Vec<_>>>()
                            .with_context(|| format!("twin faces read for {}", redacted()))?;
                        for face in faces {
                            let face = if mac_scan { face.without_vector() } else { face };
                            let new_id: i64 = twin_face_insert
                                .query_row(
                                    params![
                                        file_id,
                                        face.print_data,
                                        face.bbox,
                                        face.embedding,
                                        face.quality,
                                        face.excluded,
                                        face.model,
                                        face.processing,
                                        face.model.as_ref().map(|_| source_revision.as_str()),
                                    ],
                                    |r| r.get(0),
                                )
                                .with_context(|| format!("twin face copy for {}", redacted()))?;
                            crop_copies.push((face.id, new_id));
                        }
                        // The copy owes what its twin owes (`Tagging.twinCopy`), so catch-up tags both; the twin's Year and camera tags are its own, so the copy's are recomputed.
                        let owes = owed_copy
                            .execute(params![file_id, twin_id, f.size_bytes as i64, modified_unix, f.scanned_unix])
                            .with_context(|| format!("twin catch-up marker for {}", redacted()))?;
                        if owes == 0 {
                            owed_clear.execute(params![file_id]).with_context(|| format!("twin catch-up clear for {}", redacted()))?;
                        }
                        reused_twins += 1;
                    } else {
                        if let Some((embedding, model)) = &kept_clip {
                            own_clip_restore
                                .execute(params![file_id, embedding, model])
                                .with_context(|| format!("own clip restore for {}", redacted()))?;
                        }
                        let has_results: bool = twin_children
                            .query_row(params![file_id], |r| r.get(0))
                            .with_context(|| format!("twin children check for {}", redacted()))?;
                        if !has_results {
                            twin_unavailable
                                .execute(params![file_id, "the identical file's analysis was not available; it will be analyzed on the next scan"])
                                .with_context(|| format!("twin unavailable for {}", redacted()))?;
                            *failed += 1;
                        }
                    }
                }

                // Remember a failure that is about the file so an unreadable file stops costing a full attempt every scan.
                if f.failed {
                    if !f.failure_is_transient {
                        failure_record
                            .execute(params![
                                file_id,
                                f.size_bytes as i64,
                                modified_unix,
                                crate::pipeline::catch_up::failure_context(),
                                f.scanned_unix,
                            ])
                            .with_context(|| format!("failure memory for {}", crate::platform::redact_path_for_log(&f.path)))?;
                        owed_clear
                            .execute(params![file_id])
                            .with_context(|| format!("catch-up marker clear for {}", crate::platform::redact_path_for_log(&f.path)))?;
                    }
                } else if let Some(owed) = f
                    .catch_up
                    .as_ref()
                    .filter(|owed| owed.stages & crate::pipeline::catch_up::OWED_MASK != 0 && (f.tags_evaluated || f.ocr_stage_ran))
                {
                    // What the scan still owes this file, in the same transaction as the tags it describes.
                    owed_write
                        .execute(params![
                            file_id,
                            owed.stages,
                            f.size_bytes as i64,
                            modified_unix,
                            owed.vision,
                            owed.face_count,
                            f.scanned_unix,
                        ])
                        .with_context(|| format!("catch-up marker for {}", crate::platform::redact_path_for_log(&f.path)))?;
                } else if f.tags_evaluated || f.ocr_stage_ran {
                    owed_clear
                        .execute(params![file_id])
                        .with_context(|| format!("catch-up marker clear for {}", crate::platform::redact_path_for_log(&f.path)))?;
                }
                if !f.failed {
                    failure_clear
                        .execute(params![file_id])
                        .with_context(|| format!("failure memory clear for {}", crate::platform::redact_path_for_log(&f.path)))?;
                }

                if f.failed {
                    *failed += 1;
                } else if matches!(
                    f.kind,
                    crate::pipeline::discovery::FileKind::Image | crate::pipeline::discovery::FileKind::Video
                ) {
                    self.coordinator.note_media_analyzed(1);
                }
                *total += 1;
                if matches!(
                    f.kind,
                    crate::pipeline::discovery::FileKind::Image
                        | crate::pipeline::discovery::FileKind::Video
                        | crate::pipeline::discovery::FileKind::Model
                ) {
                    *heavy += 1;
                }
                vision.push(f.vision_ms);
                clip.push(f.clip_ms);
                let insert_ms = insert_started.elapsed().as_secs_f64() * 1000.0;
                store.push(insert_ms);
                if crate::pipeline::tagging::perf_trace_enabled() {
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

        // The batch is durable: move the crop allocations to the post-commit list; before commit the originals stay untouched, so a rollback never writes crops for nonexistent rows.
        let crops_to_write = take_deferred_face_crops(buffer, deferred_crops);

        // Prune replaced face-crop JPEGs after commit so a rolled-back batch never deletes a live crop.
        for old_id in crop_ids_to_prune {
            remove_face_crop(old_id);
        }

            // Periodic PASSIVE WAL checkpoint: a -wal that never goes through TRUNCATE keeps growing on long scans. PASSIVE does not block readers.
        const WAL_CHECKPOINT_BATCHES: u32 = 32;
        if batch_index > 0 && batch_index % WAL_CHECKPOINT_BATCHES == 0 {
            // Invariant: no transaction is open here (tx.commit closed it and the conn mutex enforces a single writer); is_autocommit() catches a regression that adds a BEGIN before this block.
            debug_assert!(
                conn.is_autocommit(),
                "WAL checkpoint must not run inside an open transaction"
            );
            // Best-effort: failure only leaves the WAL a little larger; SQLITE_BUSY is normal if a reader is mid-query.
            if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)") {
                tracing::debug!(?e, batch_index, "periodic WAL checkpoint failed (transient, continuing)");
            }
        }

        drop(conn);

        // Crop JPEG encode + write happen AFTER commit and writer-lock release so they never block the engine's only writer.
        for (face_id, crop) in crops_to_write {
            if let Err(err) = save_face_crop(face_id, crop) {
                tracing::warn!(?err, face_id, "face crop write failed");
            }
        }
        // After the twins' own crops exist: a copy's twin may sit in this batch.
        for (twin_face_id, face_id) in crop_copies {
            copy_face_crop(twin_face_id, face_id);
        }
        if reused_twins > 0 {
            tracing::info!(reused_twins, files_in_batch, "[TWIN] copied analysis from byte-identical images");
        }

        let wall = started.elapsed().as_secs_f64();
        buffer.clear();

        Ok(BatchStats {
            batch_index,
            files_in_batch,
            processed_total: *total,
            processed_heavy: *heavy,
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

/// Hot-path INSERT. Returns `id` whether the row was freshly inserted or updated via ON CONFLICT (SQLite RETURNING fires on both).
const INSERT_FILE_RETURNING_ID_SQL: &str = r#"
    INSERT INTO files (
        path_text, path_hash, size_bytes,
        created_at, modified_at, scanned_at,
        kind, extension,
        phash, aesthetic,
        has_faces, has_text,
        camera_model, location_lat, location_lon,
        failed, error_message,
        content_hash, file_ref, path_search, name_sort
    )
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?24)
    ON CONFLICT(path_text) DO UPDATE SET
        path_hash    = excluded.path_hash,
        path_search  = excluded.path_search,
        name_sort    = excluded.name_sort,
        size_bytes   = excluded.size_bytes,
        -- A time within a microsecond of the stored one is the same time, stored by an
        -- engine that rounded its last bit differently: keep the stored number so the
        -- row's revision strings and the stale-on-change trigger see no change.
        modified_at  = CASE WHEN abs(modified_at - excluded.modified_at) < 0.000001
                            THEN modified_at ELSE excluded.modified_at END,
        scanned_at   = excluded.scanned_at,
        kind         = excluded.kind,
        extension    = excluded.extension,
        -- Preserve previously-computed vision/EXIF metadata when this re-scan
        -- produced no value (decode failed, file lock, mid-scan GPU TDR, or an
        -- online-only/dehydrated placeholder leaves these NULL). A genuine
        -- content change bumps modified_at and the successful re-decode supplies
        -- a fresh Some(_) that COALESCE picks instead. Mirrors content_hash.
        phash        = COALESCE(excluded.phash, phash),
        aesthetic    = COALESCE(excluded.aesthetic, aesthetic),
        camera_model = COALESCE(excluded.camera_model, camera_model),
        location_lat = COALESCE(excluded.location_lat, location_lat),
        location_lon = COALESCE(excluded.location_lon, location_lon),
        -- has_faces/has_text are NOT NULL 0/1, so COALESCE can't protect them.
        -- Only overwrite when the producing stage actually ran this session
        -- (?21 = faces_evaluated, ?22 = ocr/doc text stage ran) — keeps the
        -- files row consistent with its still-present face_prints/ocr_text
        -- children on a models-missing / GPU-dead / online-only re-scan.
        has_faces    = CASE WHEN ?21 THEN excluded.has_faces ELSE has_faces END,
        has_text     = CASE WHEN ?22 THEN excluded.has_text  ELSE has_text  END,
        failed       = excluded.failed,
        error_message= excluded.error_message,
        content_hash = COALESCE(excluded.content_hash, content_hash),
        file_ref     = COALESCE(excluded.file_ref, file_ref),
        -- ?23: created_at came from the photo's EXIF capture time; a file's
        -- birth time never replaces the recorded one.
        created_at   = CASE WHEN ?23 THEN excluded.created_at ELSE created_at END
    RETURNING id, modified_at
"#;

// Rename/move heal lookup (v8 identity): an existing row whose `file_ref` (volume-local) or `content_hash` (cross-volume) matches at a DIFFERENT path; NULL identity never matches. Returns `path_text` and a `by_ref` flag distinguishing a true move from a coexisting byte-identical copy (see the call site).
// ?4/?6 are the legacy BLAKE3 digests of rows stamped by pre-SHA-256 builds (v0.0.1): ?4 the v0.0.1 recipe (full-file at or under the cap, head ‖ interior samples ‖ tail ‖ size over it), ?6 the pre-interior-sample over-cap composite (NULL at or under the cap). Without them the recipe change would orphan every legacy row on its first move; the upsert re-stamps the current recipe.
const HEAL_LOOKUP_SQL: &str = r#"
    SELECT id, path_text,
           (?1 IS NOT NULL AND file_ref IS NOT NULL AND file_ref = ?1 AND size_bytes = ?5) AS by_ref
    FROM files
    WHERE path_text != ?3
      AND (
          (file_ref IS NOT NULL AND file_ref = ?1 AND size_bytes = ?5)
          OR (content_hash IS NOT NULL AND content_hash IN (?2, ?4, ?6))
      )
    ORDER BY by_ref DESC
    LIMIT 32
"#;

// Heal: `UPDATE OR ABORT` is load-bearing. path_text is UNIQUE ON CONFLICT REPLACE, so a plain UPDATE would silently delete a content-identical copy at the new path along with its user tags. OR ABORT turns that collision into a skipped heal.
// ?4 is the NFC-normalized search shadow (v16 contract) — bound separately
// from ?1 (verbatim path_text) so a healed NFD path is still found by an NFC
// query, mirroring the INSERT path.
const HEAL_ONLY_CHECK_SQL: &str = "SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 AND failed = 0 \
     AND size_bytes = ?2 AND modified_at IS NOT NULL AND abs(modified_at - ?3) < 0.000001)";

const HEAL_UPDATE_SQL: &str = r#"
    UPDATE OR ABORT files
       SET path_text = ?1, path_hash = ?2, path_search = ?4, name_sort = ?5
     WHERE id = ?3
"#;

/// Whether a heal candidate genuinely MOVED: heal ONLY when its previous path no longer exists, for BOTH match kinds. A content_hash-only match also fires for a coexisting byte-identical copy, and a `file_ref` (NTFS MFT id) is only volume-local, so files on different volumes or two hardlinks can collide. Healing a coexisting file steals its row and, via `UPDATE OR REPLACE`, FK-cascades its tags/faces away (silent data loss). `symlink_metadata` so a dangling symlink still counts as present.
fn heal_candidate_moved(old_path: &str) -> bool {
    let path = std::path::Path::new(old_path);
    let gone = matches!(
        std::fs::symlink_metadata(crate::util::path_safety::to_extended_length(path)),
        Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory)
    );
    gone && removable_volume_root(path).is_none_or(|root| std::fs::read_dir(root).is_ok_and(|mut entries| entries.next().is_some()))
}

/// The mount point of a drive the path lives on, when that drive can be unplugged. A path under an unmounted volume fails to stat exactly like a deleted file, so an offline library's rows must not look "moved".
fn removable_volume_root(path: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::path::Component;
    let mut components = path.components();
    match components.next()? {
        Component::Prefix(prefix) => {
            let mut root = std::path::PathBuf::from(prefix.as_os_str());
            root.push(std::path::MAIN_SEPARATOR_STR);
            Some(root)
        }
        Component::RootDir => {
            let names: Vec<&std::ffi::OsStr> = components
                .filter_map(|c| if let Component::Normal(n) = c { Some(n) } else { None })
                .take(4)
                .collect();
            let depth = match names.first()?.to_str()? {
                "Volumes" | "mnt" => 2,
                "media" => 3,
                "run" if names.get(1).and_then(|n| n.to_str()) == Some("media") => 4,
                _ => return None,
            };
            (names.len() >= depth).then(|| names[..depth].iter().fold(std::path::PathBuf::from("/"), |root, name| root.join(name)))
        }
        _ => None,
    }
}

const INSERT_CLIP_SQL: &str = r#"
    INSERT INTO clip_embeddings (file_id, embedding, model)
    VALUES (?1, ?2, ?3)
    ON CONFLICT(file_id) DO UPDATE SET
        embedding = excluded.embedding,
        model     = excluded.model
"#;

const INSERT_FACE_SQL: &str = r#"
    INSERT INTO face_prints (file_id, print_data, bbox, arcface_embedding, face_quality, excluded, embedding_model, processing_version, source_revision)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
"#;

// Twin reuse (see pipeline/twin_reuse.rs): ?1 is the new row, ?2 the twin.
const TWIN_LOOKUP_SQL: &str = const_format_twin_lookup();

const fn const_format_twin_lookup() -> &'static str {
    concat!(
        "SELECT id FROM files WHERE content_hash = ?1 AND id != ?2 AND ",
        "kind = 'image' AND failed = 0 AND content_hash IS NOT NULL AND length(content_hash) = 32 ",
        "AND (EXISTS (SELECT 1 FROM tags t WHERE t.file_id = files.id AND t.source = 'auto') ",
        "OR EXISTS (SELECT 1 FROM clip_embeddings c WHERE c.file_id = files.id)) ",
        "ORDER BY EXISTS (SELECT 1 FROM clip_embeddings c WHERE c.file_id = files.id) DESC, id LIMIT 1"
    )
}

const OWN_CLIP_SQL: &str = r#"
    SELECT c.embedding, c.model FROM clip_embeddings c JOIN files f ON f.id = c.file_id
    WHERE f.path_text = ?1 AND f.content_hash = ?2
"#;

const OWN_CLIP_RESTORE_SQL: &str = r#"
    INSERT INTO clip_embeddings (file_id, embedding, model) VALUES (?1, ?2, ?3)
    ON CONFLICT(file_id) DO NOTHING
"#;

const TWIN_FILES_SQL: &str = r#"
    UPDATE files SET phash = t.phash, aesthetic = t.aesthetic, has_faces = t.has_faces, has_text = t.has_text,
        camera_model = COALESCE(t.camera_model, files.camera_model),
        location_lat = COALESCE(t.location_lat, files.location_lat),
        location_lon = COALESCE(t.location_lon, files.location_lon)
    FROM (SELECT phash, aesthetic, has_faces, has_text, camera_model, location_lat, location_lon
          FROM files WHERE id = ?2) AS t
    WHERE files.id = ?1
"#;

const TWIN_TAGS_SQL: &str = r#"
    INSERT OR REPLACE INTO tags (file_id, tag, source, score)
    SELECT ?1, tag, 'auto', score FROM tags
    WHERE file_id = ?2 AND source = 'auto' AND tag NOT LIKE 'Year\_%' ESCAPE '\'
"#;

const TWIN_CLIP_SQL: &str = r#"
    INSERT INTO clip_embeddings (file_id, embedding, model)
    SELECT ?1, embedding, model FROM clip_embeddings WHERE file_id = ?2
    ON CONFLICT(file_id) DO UPDATE SET embedding = excluded.embedding, model = excluded.model
"#;

const TWIN_OCR_SQL: &str = r#"
    INSERT INTO ocr_text (file_id, text, source, handwritten)
    SELECT ?1, text, source, handwritten FROM ocr_text WHERE file_id = ?2 AND source = 'ocr'
"#;

const TWIN_FACES_SQL: &str = r#"
    SELECT id, print_data, bbox, arcface_embedding, face_quality, excluded, embedding_model, processing_version
    FROM face_prints WHERE file_id = ?1 ORDER BY id
"#;

const TWIN_FACE_INSERT_SQL: &str = r#"
    INSERT INTO face_prints (file_id, print_data, bbox, arcface_embedding, face_quality, excluded, embedding_model, processing_version, source_revision)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
    RETURNING id
"#;

struct TwinFace {
    id: i64,
    print_data: Vec<u8>,
    bbox: String,
    embedding: Option<Vec<u8>>,
    quality: Option<f64>,
    excluded: Option<i64>,
    model: Option<String>,
    processing: Option<String>,
}

impl TwinFace {
    fn without_vector(self) -> Self {
        Self { print_data: Vec::new(), embedding: None, model: None, processing: None, ..self }
    }
}

const OWED_WRITE_SQL: &str = "INSERT OR REPLACE INTO scan_catchup \
     (file_id, stages, size_bytes, modified_at, vision, face_count, attempts, created_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)";

// ?1 the copy, ?2 its twin, ?3/?4 the copy's size and date, ?5 now.
const OWED_COPY_SQL: &str = "INSERT OR REPLACE INTO scan_catchup \
     (file_id, stages, size_bytes, modified_at, vision, face_count, attempts, created_at) \
     SELECT ?1, stages, ?3, ?4, CASE WHEN json_valid(vision) THEN json_remove(vision, '$.x') ELSE vision END, \
            face_count, 0, ?5 \
     FROM scan_catchup WHERE file_id = ?2 AND (stages & 3) != 0";

const CATALOG_REVISION_SQL: &str = "INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) \
     VALUES(?1,?2,'file-stat-v1',?3) ON CONFLICT(file_id) DO UPDATE SET revision=excluded.revision,updated_at=excluded.updated_at \
     WHERE catalog_revisions.revision!=excluded.revision";

/// f32 → little-endian bytes for BLOB storage, matching the macOS GRDB layout so a DB opens on either OS regardless of host endianness.
fn floats_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// NFC-normalize a path for the `path_search` shadow column (v16 contract), dependency-free (no NFC crate in the locked set): composes the canonical Latin base+combining pairs, the real NFD filename surface (HFS+/APFS, NAS, Dropbox). ASCII and composed paths return unchanged without allocating; stacked multi-mark sequences are left as-is.
/// `pub(crate)` so other path-mutating writers (restructure-apply, bulk rename, trash-restore) share one normalizer.
pub fn nfc_path_search(path: &str) -> String {
    if path.is_ascii() {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();
    while let Some(base) = chars.next() {
        if let Some(&mark) = chars.peek()
            && let Some(composed) = compose_canonical(base, mark) {
                out.push(composed);
                chars.next();
                continue;
            }
        out.push(base);
    }
    out
}

/// Canonical composition of `base` + one combining `mark`; the table is sorted by (base, mark) for binary search.
fn compose_canonical(base: char, mark: char) -> Option<char> {
    let key = (base as u32, mark as u32);
    NFC_LATIN_COMPOSE
        .binary_search_by(|&(b, m, _)| (b, m).cmp(&key))
        .ok()
        .and_then(|i| char::from_u32(NFC_LATIN_COMPOSE[i].2))
}

/// (base, combining-mark, precomposed) triples for Latin-1 Supplement + Latin Extended-A, sorted by (base, mark).
#[rustfmt::skip]
pub(crate) const NFC_LATIN_COMPOSE: &[(u32, u32, u32)] = &[
    (0x0041, 0x0300, 0x00C0), (0x0041, 0x0301, 0x00C1), (0x0041, 0x0302, 0x00C2), (0x0041, 0x0303, 0x00C3),
    (0x0041, 0x0304, 0x0100), (0x0041, 0x0306, 0x0102), (0x0041, 0x0307, 0x0226), (0x0041, 0x0308, 0x00C4),
    (0x0041, 0x030A, 0x00C5), (0x0041, 0x030C, 0x01CD), (0x0041, 0x030F, 0x0200), (0x0041, 0x0311, 0x0202),
    (0x0041, 0x0328, 0x0104), (0x0043, 0x0301, 0x0106), (0x0043, 0x0302, 0x0108), (0x0043, 0x0307, 0x010A),
    (0x0043, 0x030C, 0x010C), (0x0043, 0x0327, 0x00C7), (0x0044, 0x030C, 0x010E), (0x0045, 0x0300, 0x00C8),
    (0x0045, 0x0301, 0x00C9), (0x0045, 0x0302, 0x00CA), (0x0045, 0x0304, 0x0112), (0x0045, 0x0306, 0x0114),
    (0x0045, 0x0307, 0x0116), (0x0045, 0x0308, 0x00CB), (0x0045, 0x030C, 0x011A), (0x0045, 0x030F, 0x0204),
    (0x0045, 0x0311, 0x0206), (0x0045, 0x0327, 0x0228), (0x0045, 0x0328, 0x0118), (0x0047, 0x0301, 0x01F4),
    (0x0047, 0x0302, 0x011C), (0x0047, 0x0306, 0x011E), (0x0047, 0x0307, 0x0120), (0x0047, 0x030C, 0x01E6),
    (0x0047, 0x0327, 0x0122), (0x0048, 0x0302, 0x0124), (0x0048, 0x030C, 0x021E), (0x0049, 0x0300, 0x00CC),
    (0x0049, 0x0301, 0x00CD), (0x0049, 0x0302, 0x00CE), (0x0049, 0x0303, 0x0128), (0x0049, 0x0304, 0x012A),
    (0x0049, 0x0306, 0x012C), (0x0049, 0x0307, 0x0130), (0x0049, 0x0308, 0x00CF), (0x0049, 0x030C, 0x01CF),
    (0x0049, 0x030F, 0x0208), (0x0049, 0x0311, 0x020A), (0x0049, 0x0328, 0x012E), (0x004A, 0x0302, 0x0134),
    (0x004B, 0x030C, 0x01E8), (0x004B, 0x0327, 0x0136), (0x004C, 0x0301, 0x0139), (0x004C, 0x030C, 0x013D),
    (0x004C, 0x0327, 0x013B), (0x004E, 0x0300, 0x01F8), (0x004E, 0x0301, 0x0143), (0x004E, 0x0303, 0x00D1),
    (0x004E, 0x030C, 0x0147), (0x004E, 0x0327, 0x0145), (0x004F, 0x0300, 0x00D2), (0x004F, 0x0301, 0x00D3),
    (0x004F, 0x0302, 0x00D4), (0x004F, 0x0303, 0x00D5), (0x004F, 0x0304, 0x014C), (0x004F, 0x0306, 0x014E),
    (0x004F, 0x0307, 0x022E), (0x004F, 0x0308, 0x00D6), (0x004F, 0x030B, 0x0150), (0x004F, 0x030C, 0x01D1),
    (0x004F, 0x030F, 0x020C), (0x004F, 0x0311, 0x020E), (0x004F, 0x031B, 0x01A0), (0x004F, 0x0328, 0x01EA),
    (0x0052, 0x0301, 0x0154), (0x0052, 0x030C, 0x0158), (0x0052, 0x030F, 0x0210), (0x0052, 0x0311, 0x0212),
    (0x0052, 0x0327, 0x0156), (0x0053, 0x0301, 0x015A), (0x0053, 0x0302, 0x015C), (0x0053, 0x030C, 0x0160),
    (0x0053, 0x0326, 0x0218), (0x0053, 0x0327, 0x015E), (0x0054, 0x030C, 0x0164), (0x0054, 0x0326, 0x021A),
    (0x0054, 0x0327, 0x0162), (0x0055, 0x0300, 0x00D9), (0x0055, 0x0301, 0x00DA), (0x0055, 0x0302, 0x00DB),
    (0x0055, 0x0303, 0x0168), (0x0055, 0x0304, 0x016A), (0x0055, 0x0306, 0x016C), (0x0055, 0x0308, 0x00DC),
    (0x0055, 0x030A, 0x016E), (0x0055, 0x030B, 0x0170), (0x0055, 0x030C, 0x01D3), (0x0055, 0x030F, 0x0214),
    (0x0055, 0x0311, 0x0216), (0x0055, 0x031B, 0x01AF), (0x0055, 0x0328, 0x0172), (0x0057, 0x0302, 0x0174),
    (0x0059, 0x0301, 0x00DD), (0x0059, 0x0302, 0x0176), (0x0059, 0x0304, 0x0232), (0x0059, 0x0308, 0x0178),
    (0x005A, 0x0301, 0x0179), (0x005A, 0x0307, 0x017B), (0x005A, 0x030C, 0x017D), (0x0061, 0x0300, 0x00E0),
    (0x0061, 0x0301, 0x00E1), (0x0061, 0x0302, 0x00E2), (0x0061, 0x0303, 0x00E3), (0x0061, 0x0304, 0x0101),
    (0x0061, 0x0306, 0x0103), (0x0061, 0x0307, 0x0227), (0x0061, 0x0308, 0x00E4), (0x0061, 0x030A, 0x00E5),
    (0x0061, 0x030C, 0x01CE), (0x0061, 0x030F, 0x0201), (0x0061, 0x0311, 0x0203), (0x0061, 0x0328, 0x0105),
    (0x0063, 0x0301, 0x0107), (0x0063, 0x0302, 0x0109), (0x0063, 0x0307, 0x010B), (0x0063, 0x030C, 0x010D),
    (0x0063, 0x0327, 0x00E7), (0x0064, 0x030C, 0x010F), (0x0065, 0x0300, 0x00E8), (0x0065, 0x0301, 0x00E9),
    (0x0065, 0x0302, 0x00EA), (0x0065, 0x0304, 0x0113), (0x0065, 0x0306, 0x0115), (0x0065, 0x0307, 0x0117),
    (0x0065, 0x0308, 0x00EB), (0x0065, 0x030C, 0x011B), (0x0065, 0x030F, 0x0205), (0x0065, 0x0311, 0x0207),
    (0x0065, 0x0327, 0x0229), (0x0065, 0x0328, 0x0119), (0x0067, 0x0301, 0x01F5), (0x0067, 0x0302, 0x011D),
    (0x0067, 0x0306, 0x011F), (0x0067, 0x0307, 0x0121), (0x0067, 0x030C, 0x01E7), (0x0067, 0x0327, 0x0123),
    (0x0068, 0x0302, 0x0125), (0x0068, 0x030C, 0x021F), (0x0069, 0x0300, 0x00EC), (0x0069, 0x0301, 0x00ED),
    (0x0069, 0x0302, 0x00EE), (0x0069, 0x0303, 0x0129), (0x0069, 0x0304, 0x012B), (0x0069, 0x0306, 0x012D),
    (0x0069, 0x0308, 0x00EF), (0x0069, 0x030C, 0x01D0), (0x0069, 0x030F, 0x0209), (0x0069, 0x0311, 0x020B),
    (0x0069, 0x0328, 0x012F), (0x006A, 0x0302, 0x0135), (0x006A, 0x030C, 0x01F0), (0x006B, 0x030C, 0x01E9),
    (0x006B, 0x0327, 0x0137), (0x006C, 0x0301, 0x013A), (0x006C, 0x030C, 0x013E), (0x006C, 0x0327, 0x013C),
    (0x006E, 0x0300, 0x01F9), (0x006E, 0x0301, 0x0144), (0x006E, 0x0303, 0x00F1), (0x006E, 0x030C, 0x0148),
    (0x006E, 0x0327, 0x0146), (0x006F, 0x0300, 0x00F2), (0x006F, 0x0301, 0x00F3), (0x006F, 0x0302, 0x00F4),
    (0x006F, 0x0303, 0x00F5), (0x006F, 0x0304, 0x014D), (0x006F, 0x0306, 0x014F), (0x006F, 0x0307, 0x022F),
    (0x006F, 0x0308, 0x00F6), (0x006F, 0x030B, 0x0151), (0x006F, 0x030C, 0x01D2), (0x006F, 0x030F, 0x020D),
    (0x006F, 0x0311, 0x020F), (0x006F, 0x031B, 0x01A1), (0x006F, 0x0328, 0x01EB), (0x0072, 0x0301, 0x0155),
    (0x0072, 0x030C, 0x0159), (0x0072, 0x030F, 0x0211), (0x0072, 0x0311, 0x0213), (0x0072, 0x0327, 0x0157),
    (0x0073, 0x0301, 0x015B), (0x0073, 0x0302, 0x015D), (0x0073, 0x030C, 0x0161), (0x0073, 0x0326, 0x0219),
    (0x0073, 0x0327, 0x015F), (0x0074, 0x030C, 0x0165), (0x0074, 0x0326, 0x021B), (0x0074, 0x0327, 0x0163),
    (0x0075, 0x0300, 0x00F9), (0x0075, 0x0301, 0x00FA), (0x0075, 0x0302, 0x00FB), (0x0075, 0x0303, 0x0169),
    (0x0075, 0x0304, 0x016B), (0x0075, 0x0306, 0x016D), (0x0075, 0x0308, 0x00FC), (0x0075, 0x030A, 0x016F),
    (0x0075, 0x030B, 0x0171), (0x0075, 0x030C, 0x01D4), (0x0075, 0x030F, 0x0215), (0x0075, 0x0311, 0x0217),
    (0x0075, 0x031B, 0x01B0), (0x0075, 0x0328, 0x0173), (0x0077, 0x0302, 0x0175), (0x0079, 0x0301, 0x00FD),
    (0x0079, 0x0302, 0x0177), (0x0079, 0x0304, 0x0233), (0x0079, 0x0308, 0x00FF), (0x007A, 0x0301, 0x017A),
    (0x007A, 0x0307, 0x017C), (0x007A, 0x030C, 0x017E), (0x00C6, 0x0301, 0x01FC), (0x00C6, 0x0304, 0x01E2),
    (0x00D8, 0x0301, 0x01FE), (0x00E6, 0x0301, 0x01FD), (0x00E6, 0x0304, 0x01E3), (0x00F8, 0x0301, 0x01FF),
    (0x01B7, 0x030C, 0x01EE), (0x0292, 0x030C, 0x01EF),
];

/// Encodes a 112×112 RGB crop as JPEG at face_crops/<face_id>.jpg (37 KB raw → ~5 KB at q85).
fn save_face_crop(face_id: i64, crop_rgb_112: Vec<u8>) -> anyhow::Result<()> {
    use anyhow::Context;
    let dir = crate::paths::faces_dir().context("resolving faces dir")?;
    std::fs::create_dir_all(&dir).ok();
    let dest = dir.join(format!("{face_id}.jpg"));
    let img: image::ImageBuffer<image::Rgb<u8>, _> =
        image::ImageBuffer::from_raw(112, 112, crop_rgb_112)
            .context("face crop bytes don't match 112x112")?;
    let dyn_img = image::DynamicImage::ImageRgb8(img);
    let mut bytes = Vec::with_capacity(8 * 1024);
    dyn_img
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .context("encode face crop JPEG")?;
    std::fs::write(&dest, &bytes).with_context(|| format!("write {}", dest.display()))?;
    Ok(())
}

fn copy_face_crop(twin_face_id: i64, face_id: i64) {
    if let Ok(dir) = crate::paths::faces_dir() {
        let _ = std::fs::copy(dir.join(format!("{twin_face_id}.jpg")), dir.join(format!("{face_id}.jpg")));
    }
}

/// Best-effort removal of a face crop orphaned by a faces_evaluated re-process; a leftover crop is cosmetic disk use, never a correctness issue.
pub(crate) fn remove_face_crop(face_id: i64) {
    if let Ok(dir) = crate::paths::faces_dir() {
        let _ = std::fs::remove_file(dir.join(format!("{face_id}.jpg")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::name_sort::name_sort_key;
    use crate::pipeline::discovery::FileKind;
    use crate::pipeline::tagging::DetectedFace;
    use rusqlite::OptionalExtension;
    use std::path::PathBuf;

    #[test]
    fn fresh_catalog_skips_rename_heal_for_the_entire_scan() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn, ScanCoordinator::new());
        assert!(!writer.rename_heal_enabled);
    }

    #[test]
    fn existing_catalog_keeps_rename_heal_enabled() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        conn.lock()
            .execute(
                "INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension, failed) VALUES ('C:/a.jpg', 1, 1, 0, 'image', 'jpg', 0)",
                [],
            )
            .unwrap();
        let writer = DbWriter::new(conn, ScanCoordinator::new());
        assert!(writer.rename_heal_enabled);
    }

    /// The files-table upsert alone via the real INSERT_FILE_RETURNING_ID_SQL, so drift in its ON CONFLICT clause shows up.
    fn insert_one(conn: &Connection, f: &TaggedFile) -> Result<()> {
        let path_text = f.path.to_string_lossy();
        conn.query_row(
            INSERT_FILE_RETURNING_ID_SQL,
            params![
                path_text,
                crate::util::path_safety::stable_path_hash(&path_text),
                f.size_bytes as i64,
                None::<f64>,
                f.modified_unix,
                f.scanned_unix,
                f.kind.as_str(),
                f.path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase(),
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
                nfc_path_search(&path_text),
                f.faces_evaluated,
                f.ocr_stage_ran || f.doc_stage_ran,
                f.captured_from_exif,
                crate::db::name_sort::name_sort_key(&path_text),
            ],
            |row| row.get::<_, i64>(0),
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
            captured_from_exif: false,
            scanned_unix: 1_700_000_100.0,
            has_faces: false,
            faces: vec![],
            vision_faces: None,
            has_text: false,
            ocr_text: None,
            ocr_handwritten: false,
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
            legacy_hashes: None,
            text_embedding: None,
            doc_text: None,
            tags: vec![],
            faces_evaluated: false,
            ocr_stage_ran: false,
            doc_stage_ran: false,
            text_stage_done: false,
            tags_evaluated: true,
            reuse_twin: false,
            heal_only: false,
            failure_is_transient: false,
            catch_up: None,
        }
    }

    #[test]
    fn deferred_face_crop_transfer_moves_the_original_allocation() {
        let mut file = fixture("face.jpg");
        file.faces.push(DetectedFace {
            bbox: [0.0; 4],
            embedding: vec![1.0],
            embedding_model: None,
            roll: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            quality: 1.0,
            excluded: false,
            crop_rgb_112: Some(vec![7; 112 * 112 * 3]),
        });
        let original = file.faces[0].crop_rgb_112.as_ref().unwrap().as_ptr();
        let mut buffer = vec![file];
        let moved = take_deferred_face_crops(
            &mut buffer,
            vec![DeferredFaceCrop {
                face_id: 42,
                file_index: 0,
                face_index: 0,
            }],
        );
        assert!(buffer[0].faces[0].crop_rgb_112.is_none());
        assert_eq!(moved[0].0, 42);
        assert_eq!(moved[0].1.as_ptr(), original);
    }

    /// Re-ingesting the same path twice yields one row (guards against INSERT OR IGNORE).
    #[test]
    fn duplicate_path_resolves_to_single_row() {
        let conn = crate::db::test_support::memory_db();
        let f = fixture(r"C:\Users\adam\Pictures\IMG_0001.jpg");
        insert_one(&conn, &f).unwrap();
        insert_one(&conn, &f).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn reappearing_file_clears_soft_missing_state() {
        let conn = crate::db::test_support::memory_db();
        let f = fixture(r"C:\library\returned.jpg");
        insert_one(&conn, &f).unwrap();
        conn.execute(
            "UPDATE files SET failed = 1, error_message = 'File is no longer present under the completed scan root.'",
            [],
        )
        .unwrap();

        insert_one(&conn, &f).unwrap();
        let state: (i64, Option<String>) = conn
            .query_row("SELECT failed, error_message FROM files", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(state, (0, None));
    }

    /// INSERT_FILE_RETURNING_ID_SQL must yield the row id on BOTH the insert and ON CONFLICT DO UPDATE branches, or every repeat-scan row errors with QueryReturnedNoRows.
    #[test]
    fn insert_returning_id_yields_same_id_on_conflict() {
        let conn = crate::db::test_support::memory_db();
        let f = fixture(r"C:\Users\adam\Pictures\IMG_RETURNING.jpg");
        let path_text = f.path.to_string_lossy();
        let path_hash = crate::util::path_safety::stable_path_hash(&path_text);
        let extension = f.path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
        let bind = |f: &TaggedFile| {
            let path_text = f.path.to_string_lossy().to_string();
            (path_text, path_hash, f.size_bytes as i64, None::<f64>,
             f.modified_unix, f.scanned_unix, f.kind.as_str().to_string(),
             extension.clone(), f.phash, f.aesthetic,
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
                    row.17, row.18, row.0, false, false, false, name_sort_key(&row.0)],
            |r| r.get(0),
        ).expect("first insert returns id");
        let id2: i64 = conn.query_row(
            INSERT_FILE_RETURNING_ID_SQL,
            params![row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7,
                    row.8, row.9, row.10, row.11, row.12, row.13, row.14, row.15, row.16,
                    row.17, row.18, row.0, false, false, false, name_sort_key(&row.0)],
            |r| r.get(0),
        ).expect("ON CONFLICT branch must also return id");
        assert_eq!(id1, id2, "RETURNING must yield stable id across insert + update");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn exif_capture_time_replaces_created_at_but_birth_time_never_does() {
        let conn = crate::db::test_support::memory_db();
        let path = "/lib/IMG_DATE.jpg";
        let upsert = |created: f64, from_exif: bool| {
            conn.query_row(
                INSERT_FILE_RETURNING_ID_SQL,
                params![path, 7_i64, 1_i64, created, 1.0, 1.0, "image", "jpg", None::<i64>, None::<f64>,
                        0_i64, 0_i64, None::<String>, None::<f64>, None::<f64>, 0_i64, None::<String>,
                        None::<Vec<u8>>, None::<i64>, path, false, false, from_exif, name_sort_key(path)],
                |r| r.get::<_, i64>(0),
            ).unwrap();
            conn.query_row("SELECT CAST(created_at AS INTEGER) FROM files", [], |r| r.get::<_, i64>(0)).unwrap()
        };
        assert_eq!(upsert(1_800_000_000.0, false), 1_800_000_000);
        assert_eq!(upsert(1_900_000_000.0, false), 1_800_000_000, "a copy's birth time must not replace the recorded date");
        assert_eq!(upsert(1_200_000_000.0, true), 1_200_000_000, "EXIF capture time corrects a copy date");
    }

    #[test]
    fn a_capture_date_in_the_future_is_not_stored() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let mut future = fixture(r"C:\lib\IMG_FUTURE.jpg");
        future.created_unix = Some(2_212_000_000.0);
        future.captured_from_exif = true;
        let mut real = fixture(r"C:\lib\IMG_PAST.jpg");
        real.created_unix = Some(1_600_000_000.0);
        real.captured_from_exif = true;
        let mut buffer = vec![future, real];
        DbWriter::new(conn.clone(), ScanCoordinator::new()).flush(&mut buffer, &mut 0, &mut 0, &mut 0, 0).unwrap();
        let stored = |path: &str| -> Option<f64> {
            conn.lock().query_row("SELECT created_at FROM files WHERE path_text = ?1", params![path], |r| r.get(0)).unwrap()
        };
        assert_eq!(stored(r"C:\lib\IMG_FUTURE.jpg"), None, "a 2040 date must not become the photo's date");
        assert_eq!(stored(r"C:\lib\IMG_PAST.jpg"), Some(1_600_000_000.0));
    }

    /// A re-scan whose vision/EXIF stage produced nothing (decode failed, placeholder, GPU dead) must not null out phash/camera/GPS or flip has_faces/has_text off while child rows exist: COALESCE guards the nullable scalars, the ?21/?22 stage-ran gates the NOT NULL booleans.
    #[test]
    fn stage_skipped_rescan_preserves_prior_metadata() {
        let conn = crate::db::test_support::memory_db();
        let mut a = fixture(r"C:\lib\IMG_META.jpg");
        a.phash = Some(0x1234_5678);
        a.camera_model = Some("Canon".into());
        a.location_lat = Some(40.0);
        a.location_lon = Some(-73.0);
        a.has_faces = true;
        a.has_text = true;
        a.faces_evaluated = true;
        a.ocr_stage_ran = true;
        insert_one(&conn, &a).unwrap();

        let mut b = fixture(r"C:\lib\IMG_META.jpg");
        b.phash = None;
        b.camera_model = None;
        b.location_lat = None;
        b.location_lon = None;
        b.has_faces = false;
        b.has_text = false;
        b.faces_evaluated = false;
        b.ocr_stage_ran = false;
        b.doc_stage_ran = false;
        insert_one(&conn, &b).unwrap();

        let (phash, cam, lat, lon, hf, ht): (Option<i64>, Option<String>, Option<f64>, Option<f64>, i64, i64) =
            conn.query_row(
                "SELECT phash, camera_model, location_lat, location_lon, has_faces, has_text \
                 FROM files WHERE path_text = ?1",
                params![r"C:\lib\IMG_META.jpg"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(phash, Some(0x1234_5678), "phash preserved when re-scan produced none");
        assert_eq!(cam.as_deref(), Some("Canon"), "camera_model preserved");
        assert_eq!(lat, Some(40.0), "location_lat preserved");
        assert_eq!(lon, Some(-73.0), "location_lon preserved");
        assert_eq!(hf, 1, "has_faces not cleared when the faces stage didn't run");
        assert_eq!(ht, 1, "has_text not cleared when the text stage didn't run");

        let mut c = fixture(r"C:\lib\IMG_META.jpg");
        c.has_faces = false;
        c.faces_evaluated = true; // stage ran and found none → must clear
        insert_one(&conn, &c).unwrap();
        let hf2: i64 = conn
            .query_row("SELECT has_faces FROM files WHERE path_text = ?1",
                       params![r"C:\lib\IMG_META.jpg"], |r| r.get(0))
            .unwrap();
        assert_eq!(hf2, 0, "has_faces cleared when the faces stage actually ran and found none");
    }

    #[test]
    fn successful_rescan_restores_aesthetic_on_existing_row() {
        let conn = crate::db::test_support::memory_db();
        let path = r"C:\lib\restored.jpg";
        let dormant = fixture(path);
        insert_one(&conn, &dormant).unwrap();
        let mut restored = fixture(path);
        restored.aesthetic = Some(0.91);
        insert_one(&conn, &restored).unwrap();
        let aesthetic: Option<f64> = conn
            .query_row(
                "SELECT aesthetic FROM files WHERE path_text = ?1",
                params![path],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(aesthetic, Some(0.91));
    }

    /// ON CONFLICT must UPDATE (not skip) so a rescan writes the new size/modified_at.
    #[test]
    fn duplicate_path_updates_size_and_modified() {
        let conn = crate::db::test_support::memory_db();
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

    /// A time within a microsecond of the stored one is the same instant written by an
    /// engine that rounded its last bit differently: the stored number stays, so the
    /// stale-on-change trigger leaves reviewed rows alone; a real change still updates it.
    #[test]
    fn a_time_a_bit_apart_keeps_the_stored_number_and_a_real_change_updates_it() {
        let conn = crate::db::test_support::memory_db();
        let mut f = fixture(r"C:.jpg");
        let stored = f64::from_bits(0x41d9_b450_2f75_52cf);
        f.modified_unix = stored;
        insert_one(&conn, &f).unwrap();
        let file_id: i64 = conn.query_row("SELECT id FROM files", [], |r| r.get(0)).unwrap();
        conn.execute(
            "INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited,stale) \
             VALUES('c',?1,0,1,'t','','r','user',1,1,0)",
            params![file_id],
        )
        .unwrap();
        let read = |conn: &Connection| -> (f64, i64) {
            conn.query_row(
                "SELECT f.modified_at, c.stale FROM files f JOIN catalog_chapters c ON c.file_id = f.id",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        f.modified_unix = f64::from_bits(stored.to_bits() + 1);
        insert_one(&conn, &f).unwrap();
        assert_eq!(read(&conn), (stored, 0));
        f.modified_unix = stored + 60.0;
        insert_one(&conn, &f).unwrap();
        assert_eq!(read(&conn), (stored + 60.0, 1));
    }

    proptest::proptest! {
        /// Arbitrary inserts with duplicates: row count equals distinct paths (the resume cursor and People-tab dedup rely on path_text uniqueness).
        #[test]
        fn row_count_equals_distinct_paths(
            paths in proptest::collection::vec(r"C:\\test\\[a-z0-9]{1,8}\\f\.jpg", 1..6),
            order in proptest::collection::vec(0usize..6, 1..50),
        ) {
            let conn = crate::db::test_support::memory_db();
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

        /// Embedding BLOB round-trip must be byte-for-byte lossless and little-endian on every host (to_ne_bytes would corrupt DBs moved across architectures). Generated via f32::from_bits so NaN payloads are in scope; compared on bit patterns.
        #[test]
        fn embedding_le_bytes_round_trip(
            bits in proptest::collection::vec(proptest::num::u32::ANY, 1..520),
        ) {
            let values: Vec<f32> = bits.iter().copied().map(f32::from_bits).collect();
            let bytes = floats_to_le_bytes(&values);
            proptest::prop_assert_eq!(bytes.len(), values.len() * 4);
            let decoded: Vec<f32> = bytes
                .as_chunks::<4>().0.iter()
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

    /// A 512-d zero vector → 2048 zero bytes, guarding a Vec::with_capacity bug that allocates but doesn't write.
    #[test]
    fn embedding_le_bytes_zero_vector() {
        let v = vec![0.0_f32; 512];
        let bytes = floats_to_le_bytes(&v);
        assert_eq!(bytes.len(), 2048);
        assert!(bytes.iter().all(|&b| b == 0));
    }


    #[test]
    fn only_new_or_changed_images_and_videos_count_as_analysed_media() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let coordinator = ScanCoordinator::new();
        let writer = DbWriter::new(conn, coordinator.clone());
        let mut document = fixture("/lib/notes.pdf");
        document.kind = FileKind::Pdf;
        let mut broken = fixture("/lib/broken.jpg");
        broken.failed = true;
        let mut buffer = vec![document, broken];
        writer.flush(&mut buffer, &mut 0, &mut 0, &mut 0, 0).unwrap();
        assert_eq!(coordinator.media_analyzed(), 0);
        let mut video = fixture("/lib/clip.mov");
        video.kind = FileKind::Video;
        let mut buffer = vec![fixture("/lib/a.jpg"), video];
        writer.flush(&mut buffer, &mut 0, &mut 0, &mut 0, 1).unwrap();
        assert_eq!(coordinator.media_analyzed(), 2);
    }

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

    /// Mirrors the heal+insert per-file body of `flush` over the real SQL constants and `heal_candidate_moved`, so the old-path-gone guard is tested end to end.
    fn ingest_with_heal(conn: &Connection, f: &TaggedFile) -> i64 {
        let path_text = f.path.to_string_lossy();
        let path_hash = crate::util::path_safety::stable_path_hash(&path_text);
        let path_search = nfc_path_search(&path_text);
        let extension = f
            .path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if f.file_ref.is_some() || f.content_hash.is_some() {
            let ch_bytes = f.content_hash.as_ref().map(|h| h.as_slice());
            let legacy_hash = if f.content_hash.is_some() {
                crate::util::content_hash::legacy_content_hashes(&f.path, f.size_bytes).ok()
            } else {
                None
            };
            let legacy_hash = legacy_hash.as_ref();
            let healed: Option<(i64, String, bool)> = conn
                .query_row(
                    HEAL_LOOKUP_SQL,
                    params![
                        f.file_ref.map(|r| r as i64),
                        ch_bytes,
                        path_text.as_ref(),
                        legacy_hash.map(|h| h.v2.as_slice()),
                        f.size_bytes as i64,
                        legacy_hash.and_then(|h| h.v1.as_ref()).map(|h| h.as_slice())
                    ],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)),
                )
                .optional()
                .unwrap();
            if let Some((id, old_path, _by_ref)) = healed
                && heal_candidate_moved(&old_path) {
                    match conn.execute(
                        HEAL_UPDATE_SQL,
                        params![path_text, path_hash, id, path_search, crate::db::name_sort::name_sort_key(&path_text)],
                    ) {
                        Ok(_) => {}
                        Err(rusqlite::Error::SqliteFailure(e, _))
                            if e.code == rusqlite::ErrorCode::ConstraintViolation => {}
                        Err(e) => panic!("unexpected heal error: {e}"),
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
                path_search,
                f.faces_evaluated,
                f.ocr_stage_ran || f.doc_stage_ran,
                f.captured_from_exif,
                crate::db::name_sort::name_sort_key(&path_text),
            ],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_path_under_an_unmounted_volume_is_not_a_moved_file() {
        for old in [
            "/Volumes/fileid-test-offline-drive/Photos/a.jpg",
            "/mnt/fileid-test-offline-drive/Photos/a.jpg",
            "/media/someone/fileid-test-offline-drive/a.jpg",
            "/run/media/someone/fileid-test-offline-drive/a.jpg",
        ] {
            assert!(!heal_candidate_moved(old), "{old}");
        }
        let dir = unique_tmp_dir("heal_moved");
        let gone = dir.join("deleted.jpg");
        assert!(heal_candidate_moved(&gone.to_string_lossy()), "a deleted file on a mounted disk is a move");
        assert!(!heal_candidate_moved(&dir.to_string_lossy()), "a present path is not moved");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn removable_volume_roots_are_found_for_mount_conventions_only() {
        let root = |p: &str| removable_volume_root(std::path::Path::new(p)).map(|r| r.to_string_lossy().into_owned());
        assert_eq!(root("/Volumes/Adlon/a/b.jpg").as_deref(), Some("/Volumes/Adlon"));
        assert_eq!(root("/media/u/Disk/x").as_deref(), Some("/media/u/Disk"));
        assert_eq!(root("/run/media/u/Disk/x").as_deref(), Some("/run/media/u/Disk"));
        assert_eq!(root("/Users/a/Pictures/x.jpg"), None);
        assert_eq!(root("/Volumes"), None);
    }

    /// `by_ref` is 1 only for a `file_ref` match and 0 (never NULL) for a content_hash-only match, even when the incoming file_ref is NULL but the matched row has one.
    #[test]
    fn heal_lookup_flags_ref_match_but_not_hash_only() {
        let conn = crate::db::test_support::memory_db();
        let mut a = fixture(r"C:\lib\old\IMG.jpg");
        a.content_hash = Some([7u8; 32]);
        a.file_ref = Some(0xABCD);
        ingest_with_heal(&conn, &a);

        let by_ref: bool = conn
            .query_row(
                HEAL_LOOKUP_SQL,
                params![Some(0xABCDu64), Some([7u8; 32].as_slice()), r"C:\lib\new\IMG.jpg", None::<&[u8]>, 1234i64, None::<&[u8]>],
                |r| Ok(r.get::<_, i64>(2)? != 0),
            )
            .unwrap();
        assert!(by_ref, "file_ref match must set by_ref");

        let by_ref_none: bool = conn
            .query_row(
                HEAL_LOOKUP_SQL,
                params![None::<u64>, Some([7u8; 32].as_slice()), r"C:\lib\new\IMG.jpg", None::<&[u8]>, 1234i64, None::<&[u8]>],
                |r| Ok(r.get::<_, i64>(2)? != 0),
            )
            .unwrap();
        assert!(!by_ref_none, "content_hash-only match must clear by_ref");
    }

    /// A row stamped by a pre-SHA-256 build carries a BLAKE3 digest; the lookup must match it via the legacy ?4 / ?6 fallbacks even though the current-recipe digest (?2) differs.
    #[test]
    fn heal_lookup_matches_legacy_recipe_digest_via_fallback() {
        let conn = crate::db::test_support::memory_db();
        let mut old = fixture(r"C:\lib\old\HUGE.tif");
        old.content_hash = Some([0x22; 32]); // legacy BLAKE3 digest stamped by main
        ingest_with_heal(&conn, &old);

        let probe = |v2: [u8; 32], v1: Option<[u8; 32]>| -> Option<(i64, bool)> {
            conn.query_row(
                HEAL_LOOKUP_SQL,
                params![
                    None::<u64>,
                    Some([0x33u8; 32].as_slice()), // current-recipe digest: no row has it
                    r"C:\lib\new\HUGE.tif",
                    Some(v2.as_slice()),
                    1234i64,
                    v1.as_ref().map(|h| h.as_slice())
                ],
                |r| Ok((r.get(0)?, r.get::<_, i64>(2)? != 0)),
            )
            .optional()
            .unwrap()
        };

        let (_, by_ref) = probe([0x22; 32], None).expect("legacy digest must match via ?4");
        assert!(!by_ref, "legacy-hash match is a content match, not a ref match");

        let (_, by_ref) = probe([0x44; 32], Some([0x22; 32]))
            .expect("pre-interior-sample digest must match via ?6");
        assert!(!by_ref, "legacy-hash match is a content match, not a ref match");

        assert!(
            probe([0x44; 32], Some([0x55; 32])).is_none(),
            "non-matching legacy digests must not heal"
        );
    }

    /// Two byte-identical files that COEXIST (a copy, not a move) must each keep their own row; the content_hash heal must not steal the first's.
    #[test]
    fn coexisting_byte_identical_copies_stay_distinct_rows() {
        let dir = unique_tmp_dir("b1_copy");
        let orig = dir.join("IMG_1558.HEIC");
        std::fs::write(&orig, b"same-bytes").unwrap();
        let copy = dir.join("IMG_1558(1).HEIC");
        std::fs::write(&copy, b"same-bytes").unwrap();

        let conn = crate::db::test_support::memory_db();
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

    /// A genuine MOVE (content_hash match, old path gone from disk) heals
    /// to a single row, preserving the row id and its FK-linked tags.
    #[test]
    fn genuine_move_heals_and_preserves_fks() {
        let dir = unique_tmp_dir("b1_move");
        let new_path = dir.join("moved.jpg");
        std::fs::write(&new_path, b"payload").unwrap();
        let old_path = dir.join("gone").join("orig.jpg");

        let conn = crate::db::test_support::memory_db();
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

    /// Two copies of one moved file in the same batch: the pre-pass offers
    /// both the same orphan row; only the first may heal it.
    #[test]
    fn one_batch_heals_a_moved_row_once() {
        let dir = unique_tmp_dir("batch_move");
        let old_path = dir.join("gone").join("orig.jpg");
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let mut seed = fixture(old_path.to_str().unwrap());
        seed.content_hash = Some([0x33; 32]);
        let seed_id = ingest_with_heal(&conn.lock(), &seed);
        let mut buffer: Vec<TaggedFile> = ["a.jpg", "b.jpg"]
            .iter()
            .map(|name| {
                let path = dir.join(name);
                std::fs::write(&path, b"payload").unwrap();
                let mut f = fixture(path.to_str().unwrap());
                f.content_hash = Some([0x33; 32]);
                f
            })
            .collect();
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        writer.flush(&mut buffer, &mut 0, &mut 0, &mut 0, 0).unwrap();
        let rows: Vec<(i64, String)> = conn
            .lock()
            .prepare("SELECT id, path_text FROM files ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (seed_id, dir.join("a.jpg").to_string_lossy().into_owned()));
        assert_eq!(rows[1].1, dir.join("b.jpg").to_string_lossy());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: when a content-identical COPY occupies the new path with a LIVE row carrying user metadata, healing a moved-away orphan onto that path must NOT clobber it. `path_text UNIQUE ON CONFLICT REPLACE` made a plain UPDATE silently delete the copy's row and FK-cascade its user tags/person names; `UPDATE OR ABORT` prevents it.
    #[test]
    fn heal_does_not_clobber_a_live_copy_at_the_new_path() {
        let dir = unique_tmp_dir("heal_no_clobber");
        let p1 = dir.join("orig.jpg");
        let p2 = dir.join("copy.jpg");
        std::fs::write(&p1, b"payload").unwrap();
        std::fs::write(&p2, b"payload").unwrap();

        let conn = crate::db::test_support::memory_db();
        let mut a = fixture(p1.to_str().unwrap());
        a.content_hash = Some([0x33; 32]);
        a.file_ref = None;
        let id_a = ingest_with_heal(&conn, &a);

        let mut b = fixture(p2.to_str().unwrap());
        b.content_hash = Some([0x33; 32]);
        b.file_ref = None;
        let id_b = ingest_with_heal(&conn, &b);
        assert_ne!(id_a, id_b, "the copy at P2 must get its OWN row while P1 still exists");
        std::fs::remove_file(&p1).unwrap();
        conn.execute(
            "INSERT INTO tags (file_id, tag, source, score) VALUES (?1, 'Grandma', 'user', 1.0)",
            params![id_b],
        )
        .unwrap();

        // Reprocess P2: the heal finds orphan A (P1 gone, hash matches) but P2 is occupied by live row B, so the heal must be skipped.
        let id_reproc = ingest_with_heal(&conn, &b);
        assert_eq!(id_reproc, id_b, "reprocessing P2 must keep B's row id");

        let user_tag: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tags WHERE file_id = ?1 AND tag = 'Grandma' AND source = 'user'",
                params![id_b],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(user_tag, 1, "B's user-authored tag must survive; the heal must not REPLACE-delete B");
        let b_still_at_p2: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE id = ?1 AND path_text = ?2",
                params![id_b, p2.to_string_lossy()], |r| r.get(0))
            .unwrap();
        assert_eq!(b_still_at_p2, 1, "row B must still own P2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_ref_rename_with_old_path_gone_heals() {
        let dir = unique_tmp_dir("b1_ref_rename");
        let new_path = dir.join("after.png");
        std::fs::write(&new_path, b"x").unwrap();
        let old_path = dir.join("gone").join("before.png");

        let conn = crate::db::test_support::memory_db();
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

    /// Cross-volume/hardlink safety: a file_ref match while the OLD path still exists is NOT a rename (MFT refs are volume-local), and healing would FK-cascade the loser's tags/faces away. The old-path-gone gate keeps two distinct rows.
    #[test]
    fn file_ref_collision_with_both_paths_present_stays_distinct() {
        let dir = unique_tmp_dir("b1_ref_collision");
        let old_path = dir.join("before.png");
        std::fs::write(&old_path, b"x").unwrap(); // old path STILL present
        let new_path = dir.join("after.png");
        std::fs::write(&new_path, b"y").unwrap(); // a DISTINCT coexisting file

        let conn = crate::db::test_support::memory_db();
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

    /// A file_ref match with the old path GONE but a different row size is not a genuine move (volume-local MFT ref collision or reuse); the size_bytes corroboration blocks it, while a true rename still heals.
    #[test]
    fn file_ref_heal_requires_matching_size() {
        let dir = unique_tmp_dir("r3_17_size");
        let new_path = dir.join("after.png");
        std::fs::write(&new_path, b"x").unwrap();
        let old_path = dir.join("gone").join("before.png");

        let conn = crate::db::test_support::memory_db();
        let mut a = fixture(old_path.to_str().unwrap());
        a.file_ref = Some(0xC0FF_EE00);
        a.size_bytes = 1000; // original file's size
        let id_a = ingest_with_heal(&conn, &a);

        let mut b = fixture(new_path.to_str().unwrap());
        b.file_ref = Some(0xC0FF_EE00); // same ref, old path gone …
        b.size_bytes = 2000; // … but a different size → a different file
        let id_b = ingest_with_heal(&conn, &b);

        assert_ne!(id_a, id_b, "same ref + old path gone but different size must not heal");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "the size mismatch keeps them as two distinct rows");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An NTFS `file_ref` with the high bit set exceeds `i64::MAX`, which rusqlite's `ToSql for u64` rejects, aborting the whole flush batch. Values are bitcast u64 → i64 at every bind site; the insert must succeed and still round-trip through the heal lookup.
    #[test]
    fn high_bit_file_ref_does_not_abort_insert() {
        let dir = unique_tmp_dir("eng18_ref");
        let hi: u64 = 0xFFFF_0000_0000_0001; // > i64::MAX
        assert!(hi > i64::MAX as u64, "fixture must exercise the high-bit path");

        let conn = crate::db::test_support::memory_db();
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

    /// The v15 sync triggers own ocr_fts/doc_fts; the writer touches only content tables (explicit DELETE + INSERT). FTS must follow insert → re-process → update → delete with no stale postings. A manual FTS statement once double-fired against a macOS trigger set → SQLITE_CORRUPT on every re-process.
    #[test]
    fn fts_follows_content_tables_via_triggers() {
        let conn = crate::db::test_support::memory_db();
        conn.execute(
            "INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension) \
             VALUES ('C:\\t\\a.png', 1, 1, 1.0, 'image', 'png')",
            [],
        )
        .unwrap();
        let id: i64 = conn
            .query_row("SELECT id FROM files WHERE path_text = 'C:\\t\\a.png'", [], |r| r.get(0))
            .unwrap();

        let hits = |term: &str, fts: &str| -> i64 {
            conn.query_row(
                &format!("SELECT COUNT(*) FROM {fts} WHERE {fts} MATCH ?1"),
                params![term],
                |r| r.get(0),
            )
            .unwrap()
        };
        let integrity_ok = |fts: &str| {
            conn.execute(
                &format!("INSERT INTO {fts}({fts}, rank) VALUES('integrity-check', 1)"),
                [],
            )
            .map(|_| ())
        };

        for (content, fts) in [("ocr_text", "ocr_fts"), ("doc_text", "doc_fts")] {
            conn.execute(
                &format!("INSERT INTO {content} (file_id, text) VALUES (?1, 'alpha bravo')"),
                params![id],
            )
            .unwrap();
            assert_eq!(hits("alpha", fts), 1, "{fts}: insert must be indexed by the ai trigger");

            conn.execute(&format!("DELETE FROM {content} WHERE file_id = ?1"), params![id])
                .unwrap();
            conn.execute(
                &format!("INSERT INTO {content} (file_id, text) VALUES (?1, 'charlie delta')"),
                params![id],
            )
            .unwrap();
            assert_eq!(hits("alpha", fts), 0, "{fts}: re-process must drop stale postings");
            assert_eq!(hits("charlie", fts), 1, "{fts}: re-process must index fresh text");

            conn.execute(
                &format!("UPDATE {content} SET text = 'echo foxtrot' WHERE file_id = ?1"),
                params![id],
            )
            .unwrap();
            assert_eq!(hits("charlie", fts), 0, "{fts}: au trigger must drop old postings");
            assert_eq!(hits("echo", fts), 1, "{fts}: au trigger must index new text");

            conn.execute(&format!("DELETE FROM {content} WHERE file_id = ?1"), params![id])
                .unwrap();
            assert_eq!(hits("echo", fts), 0, "{fts}: ad trigger must clear postings");

            integrity_ok(fts).unwrap_or_else(|e| panic!("{fts} integrity-check failed: {e}"));
        }
    }

    /// Every writer path stamps `path_search` so the app's normalization-insensitive LIKE never misses engine-written rows.
    #[test]
    fn writers_populate_path_search() {
        let conn = crate::db::test_support::memory_db();
        let f = fixture(r"C:\lib\café.jpg");
        insert_one(&conn, &f).unwrap();
        let stored: String = conn
            .query_row(
                "SELECT path_search FROM files WHERE path_text = ?1",
                params![r"C:\lib\café.jpg"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, r"C:\lib\café.jpg");

        let id: i64 = conn
            .query_row("SELECT id FROM files WHERE path_text = ?1", params![r"C:\lib\café.jpg"], |r| r.get(0))
            .unwrap();
        let new_path = r"C:\lib\renamed.jpg";
        conn.execute(
            HEAL_UPDATE_SQL,
            params![new_path, 42i64, id, nfc_path_search(new_path), crate::db::name_sort::name_sort_key(new_path)],
        )
        .unwrap();
        let healed: String = conn
            .query_row("SELECT path_search FROM files WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(healed, r"C:\lib\renamed.jpg", "heal must re-stamp path_search");
    }

    #[test]
    fn writers_stamp_the_name_sort_key_on_insert_and_on_a_healed_rename() {
        let conn = crate::db::test_support::memory_db();
        let path = "/lib/Zoë 10.jpg";
        let id: i64 = conn
            .query_row(
                INSERT_FILE_RETURNING_ID_SQL,
                params![path, 7_i64, 1_i64, None::<f64>, 1.0, 1.0, "image", "jpg", None::<i64>, None::<f64>,
                        0_i64, 0_i64, None::<String>, None::<f64>, None::<f64>, 0_i64, None::<String>,
                        None::<Vec<u8>>, None::<i64>, path, false, false, false, name_sort_key(path)],
                |r| r.get(0),
            )
            .unwrap();
        let key = |conn: &Connection| -> String {
            conn.query_row("SELECT name_sort FROM files WHERE id = ?1", params![id], |r| r.get(0)).unwrap()
        };
        assert_eq!(key(&conn), name_sort_key("/x/zoe 10.jpg"));
        let renamed = "/lib/Apple 2.jpg";
        conn.execute(HEAL_UPDATE_SQL, params![renamed, 9_i64, id, renamed, name_sort_key(renamed)]).unwrap();
        assert_eq!(key(&conn), name_sort_key("apple 2.jpg"));
    }

    // An NFD filename (Mac/NAS/Dropbox-synced) must be stored as NFC in path_search so the app's NFC query finds it.
    #[test]
    fn nfd_filename_is_searchable_by_nfc_query() {
        let nfd_path = "C:\\lib\\cafe\u{0301}.jpg";
        let nfc_query = "%caf\u{00E9}.jpg";

        assert_ne!(nfd_path, "C:\\lib\\caf\u{00E9}.jpg",
            "test vector must actually be NFD, not NFC");

        let conn = crate::db::test_support::memory_db();
        let f = fixture(nfd_path);
        insert_one(&conn, &f).unwrap();

        let (stored_text, stored_search): (String, String) = conn
            .query_row(
                "SELECT path_text, path_search FROM files LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored_text, nfd_path, "path_text stays verbatim");
        assert_eq!(stored_search, "C:\\lib\\caf\u{00E9}.jpg",
            "path_search must be NFC-composed");

        let found_via_search: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path_search LIKE ?1",
                params![nfc_query],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found_via_search, 1,
            "NFC query must match the NFC-normalized path_search");

        let found_via_text: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path_text LIKE ?1",
                params![nfc_query],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found_via_text, 0,
            "NFC query cannot match verbatim NFD path_text — why path_search exists");
    }

    #[test]
    fn nfc_path_search_is_identity_on_ascii() {
        let p = r"C:\Users\me\Pictures\vacation_2024.jpg";
        assert_eq!(nfc_path_search(p), p, "ASCII paths must not be altered");
    }

    #[test]
    fn vision_faces_and_text_reads_are_stored_as_the_swift_scan_stores_them() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let mut file = fixture("/internal/vision-faces.jpg");
        file.faces_evaluated = true;
        file.has_faces = true;
        file.vision_faces = Some(vec![
            crate::pipeline::tagging::VisionFace { bbox: "0.1000,0.2000,0.3000,0.4000".into(), quality: Some(0.5), excluded: false },
            crate::pipeline::tagging::VisionFace { bbox: "0.6000,0.6000,0.0100,0.0100".into(), quality: None, excluded: true },
        ]);
        file.ocr_stage_ran = true;
        file.has_text = true;
        file.ocr_text = Some("Meeting notes".into());
        file.ocr_handwritten = true;
        let mut buffer = vec![file];
        writer.flush(&mut buffer, &mut 0, &mut 0, &mut 0, 0).unwrap();
        let db = conn.lock();
        let faces: Vec<(Vec<u8>, String, Option<f64>, i64, Option<String>, Option<String>)> = db
            .prepare("SELECT print_data,bbox,face_quality,excluded,embedding_model,processing_version FROM face_prints ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            faces,
            vec![
                (vec![], "0.1000,0.2000,0.3000,0.4000".to_owned(), Some(0.5), 0, None, None),
                (vec![], "0.6000,0.6000,0.0100,0.0100".to_owned(), None, 1, None, None),
            ]
        );
        let revisions: i64 = db.query_row("SELECT COUNT(*) FROM catalog_revisions", [], |row| row.get(0)).unwrap();
        assert_eq!(revisions, 0, "the Mac face pipeline records its own file revision");
        let text: (String, String, i64) = db.query_row("SELECT text,source,handwritten FROM ocr_text", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        assert_eq!(text, ("Meeting notes".to_owned(), "ocr".to_owned(), 1));
    }

    #[test]
    fn face_flush_records_verified_model_and_source_revision() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let mut file = fixture("/internal/face-cache.png");
        file.faces_evaluated = true;
        let mut embedding = vec![0.0; 128];
        embedding[0] = 1.0;
        file.faces.push(crate::pipeline::tagging::DetectedFace {
            bbox: [1.0, 2.0, 10.0, 10.0],
            embedding,
            embedding_model: Some("fixture-weight-hash".to_owned()),
            roll: 0.0, yaw: 0.0, pitch: 0.0, quality: 0.9,
            excluded: false, crop_rgb_112: None,
        });
        let expected_revision = format!("{}:{}", file.size_bytes, file.modified_unix.to_bits());
        let mut buffer = vec![file];
        writer.flush(&mut buffer, &mut 0, &mut 0, &mut 0, 0).unwrap();
        let db = conn.lock();
        let recorded: (String,String,String) = db.query_row("SELECT embedding_model,processing_version,source_revision FROM face_prints", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(recorded, ("fixture-weight-hash".to_owned(),"rust-align112-v1".to_owned(),expected_revision));
        let evidence: (i64,f64,i64) = db.query_row("SELECT e.dimension,o.confidence,o.stale FROM catalog_embeddings e JOIN catalog_observations o ON o.id=e.entity_id", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(evidence, (128,0.0,0));
        db.execute("UPDATE face_prints SET excluded=1", []).unwrap();
        db.execute("UPDATE files SET modified_at=modified_at+1", []).unwrap();
        db.execute("UPDATE face_prints SET excluded=0", []).unwrap();
        let stale: i64 = db.query_row("SELECT stale FROM catalog_observations", [], |row| row.get(0)).unwrap();
        assert_eq!(stale, 1);
        db.execute("UPDATE catalog_observations SET user_edited=1,region_json='manual marker'", []).unwrap();
        db.execute("UPDATE face_prints SET bbox='changed region'", []).unwrap();
        let empty: i64 = db.query_row("SELECT COUNT(*) FROM face_prints WHERE arcface_embedding IS NULL", [], |row| row.get(0)).unwrap();
        assert_eq!(empty, 1);
        db.execute("DELETE FROM face_prints", []).unwrap();
        let preserved: String = db.query_row("SELECT region_json FROM catalog_observations", [], |row| row.get(0)).unwrap();
        assert_eq!(preserved, "manual marker");
    }

    // The legacy content-hash re-read for an over-cap (>16 MB) file is a ~2 MB blocking disk read and must happen BEFORE the writer lock (into `legacy_hashes`), never inside the single-writer transaction. Drives the real `flush` with a row stamped with the legacy digest at a now-gone path; the heal fires only if `flush` read the digest off disk and matched via `?6`.
    #[test]
    fn a_full_database_fails_the_batch_whole_and_the_writer_recovers() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        {
            let c = conn.lock();
            let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
            c.execute_batch(&format!("PRAGMA max_page_count = {pages}")).unwrap();
        }
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let batch = || -> Vec<TaggedFile> {
            (0..200)
                .map(|i| {
                    let mut file = fixture(&format!("/lib/full/{i}.txt"));
                    file.kind = FileKind::Doc;
                    file.doc_text = Some("words ".repeat(400));
                    file
                })
                .collect()
        };
        let mut buffer = batch();
        let (mut total, mut failed) = (0u64, 0u64);
        assert!(writer.flush(&mut buffer, &mut total, &mut 0u64, &mut failed, 0).is_err(), "SQLITE_FULL must surface");
        {
            let c = conn.lock();
            assert!(c.is_autocommit(), "the failed batch must not leave a transaction open");
            let rows: i64 = c.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
            assert_eq!(rows, 0, "a failed batch commits nothing");
            c.execute_batch("PRAGMA max_page_count = 1073741823").unwrap();
        }
        let mut buffer = batch();
        writer.flush(&mut buffer, &mut total, &mut 0u64, &mut failed, 1).expect("writer works once space frees");
        let rows: i64 = conn.lock().query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 200);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_writer_commits_every_row_across_more_than_two_batches_and_the_tail() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let n = current_batch_size() * 2 + 7;
        let (tx, rx) = mpsc::channel(n + 1);
        for i in 0..n {
            tx.send(fixture(&format!("/lib/many/{i}.jpg"))).await.unwrap();
        }
        drop(tx);
        let mut batches = 0u32;
        let (total, failed) = writer.run(rx, |_| batches += 1).await.unwrap();
        assert_eq!((total, failed), (n as u64, 0));
        assert!(batches >= 3, "two full batches and the tail are three commits, got {batches}");
        let rows: i64 = conn.lock().query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(rows as usize, n, "no row is lost between batches or in the final partial batch");
        let distinct: i64 = conn.lock().query_row("SELECT COUNT(DISTINCT path_hash) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(distinct as usize, n);
    }

    #[test]
    fn over_cap_legacy_hash_heal_runs_via_prelock_read() {
        let dir = unique_tmp_dir("overcap_legacy");
        let over_cap_bytes = crate::util::content_hash::FULL_HASH_MAX_BYTES + 4096;
        let new_path = dir.join("moved_here.bin");
        let mut content = vec![0u8; over_cap_bytes as usize];
        for (i, b) in content.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        std::fs::write(&new_path, &content).unwrap();

        // The pre-interior-sample digest the decoder thread would NOT have stamped; `flush` reproduces it by re-reading the file.
        let legacy_digest =
            crate::util::content_hash::legacy_content_hashes(&new_path, over_cap_bytes)
                .unwrap()
                .v1
                .unwrap();

        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));

        let old_path = dir.join("was_here.bin"); // never written → gone from disk
        {
            let c = conn.lock();
            let mut seed = fixture(old_path.to_str().unwrap());
            seed.size_bytes = over_cap_bytes;
            seed.kind = FileKind::Other;
            seed.content_hash = Some(legacy_digest);
            insert_one(&c, &seed).unwrap();
        }

        let mut incoming = fixture(new_path.to_str().unwrap());
        incoming.size_bytes = over_cap_bytes;
        incoming.kind = FileKind::Other;
        incoming.content_hash = Some([0xABu8; 32]);
        incoming.legacy_hashes =
            Some(crate::util::content_hash::legacy_content_hashes(&new_path, over_cap_bytes).unwrap());

        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let mut buffer = vec![incoming];
        let mut total = 0u64;
        let mut failed = 0u64;
        writer.flush(&mut buffer, &mut total, &mut 0u64, &mut failed, 0).unwrap();

        let c = conn.lock();
        let row_count: i64 = c
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row_count, 1, "heal must re-bind the prior row, not add a second");
        let healed_path: String = c
            .query_row("SELECT path_text FROM files LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            healed_path,
            new_path.to_string_lossy(),
            "over-cap row must heal to the new path via the legacy-digest fallback"
        );

        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Pre-SHA-256 builds (v0.0.1) stamped full-file BLAKE3 for under-cap files. On a cross-volume move file_ref is useless, so the heal must reproduce that digest; gating the legacy probe on over-cap orphaned every ≤16 MB legacy row.
    #[test]
    fn under_cap_legacy_blake3_row_heals_instead_of_orphaning() {
        let dir = unique_tmp_dir("undercap_legacy");
        let new_path = dir.join("moved_here.bin");
        let content: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
        let size = content.len() as u64;
        std::fs::write(&new_path, &content).unwrap();

        let v001_digest = *blake3::hash(&content).as_bytes();

        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let old_path = dir.join("was_here.bin"); // never written → gone from disk
        let seed_id: i64;
        {
            let c = conn.lock();
            let mut seed = fixture(old_path.to_str().unwrap());
            seed.size_bytes = size;
            seed.kind = FileKind::Other;
            seed.content_hash = Some(v001_digest);
            insert_one(&c, &seed).unwrap();
            seed_id = c.query_row("SELECT id FROM files", [], |r| r.get(0)).unwrap();
        }

        let mut incoming = fixture(new_path.to_str().unwrap());
        incoming.size_bytes = size;
        incoming.kind = FileKind::Other;
        incoming.content_hash =
            Some(crate::util::content_hash::content_hash(&new_path, size).unwrap());
        incoming.legacy_hashes =
            Some(crate::util::content_hash::legacy_content_hashes(&new_path, size).unwrap());

        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let mut buffer = vec![incoming];
        let mut total = 0u64;
        let mut failed = 0u64;
        writer.flush(&mut buffer, &mut total, &mut 0u64, &mut failed, 0).unwrap();

        let c = conn.lock();
        let row_count: i64 = c
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row_count, 1, "under-cap legacy row must heal, not be orphaned");
        let (id, path): (i64, String) = c
            .query_row("SELECT id, path_text FROM files", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(id, seed_id, "heal must preserve the row id (its tags/faces/embeddings)");
        assert_eq!(path, new_path.to_string_lossy());

        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // v0.0.1 stamped over-cap files with blake3(head ‖ interior samples ‖ tail ‖ size_le); the seed digest here can only match via the v0.0.1-recipe probe (?4).
    #[test]
    fn over_cap_v001_interior_sample_row_heals() {
        let dir = unique_tmp_dir("overcap_v001");
        let size = crate::util::content_hash::FULL_HASH_MAX_BYTES + 4096;
        let new_path = dir.join("moved_here.bin");
        let mut content = vec![0u8; size as usize];
        for (i, b) in content.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        std::fs::write(&new_path, &content).unwrap();

        let legacy = crate::util::content_hash::legacy_content_hashes(&new_path, size).unwrap();
        let v001_digest = legacy.v2;
        assert_ne!(
            Some(v001_digest),
            legacy.v1,
            "fixture must make the interior samples fire (v2 != v1)"
        );

        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let old_path = dir.join("was_here.bin"); // never written → gone from disk
        let seed_id: i64;
        {
            let c = conn.lock();
            let mut seed = fixture(old_path.to_str().unwrap());
            seed.size_bytes = size;
            seed.kind = FileKind::Other;
            seed.content_hash = Some(v001_digest);
            insert_one(&c, &seed).unwrap();
            seed_id = c.query_row("SELECT id FROM files", [], |r| r.get(0)).unwrap();
        }

        let mut incoming = fixture(new_path.to_str().unwrap());
        incoming.size_bytes = size;
        incoming.kind = FileKind::Other;
        incoming.content_hash =
            Some(crate::util::content_hash::content_hash(&new_path, size).unwrap());
        incoming.legacy_hashes =
            Some(crate::util::content_hash::legacy_content_hashes(&new_path, size).unwrap());

        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let mut buffer = vec![incoming];
        let mut total = 0u64;
        let mut failed = 0u64;
        writer.flush(&mut buffer, &mut total, &mut 0u64, &mut failed, 0).unwrap();

        let c = conn.lock();
        let row_count: i64 = c
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row_count, 1, "v0.0.1-recipe over-cap row must heal, not be orphaned");
        let (id, path): (i64, String) = c
            .query_row("SELECT id, path_text FROM files", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(id, seed_id, "heal must preserve the row id (its tags/faces/embeddings)");
        assert_eq!(path, new_path.to_string_lossy());

        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

}

#[cfg(test)]
mod twin_reuse_tests {
    use super::*;
    use crate::pipeline::discovery::FileKind;
    use crate::pipeline::tagging::DetectedFace;
    use std::path::PathBuf;

    fn image(path: &str, hash: [u8; 32]) -> TaggedFile {
        TaggedFile {
            path: PathBuf::from(path),
            kind: FileKind::Image,
            size_bytes: 1234,
            modified_unix: 1_700_000_000.0,
            created_unix: Some(1_600_000_000.0),
            captured_from_exif: false,
            scanned_unix: 1_700_000_100.0,
            has_faces: false,
            faces: vec![],
            vision_faces: None,
            has_text: false,
            ocr_text: None,
            ocr_handwritten: false,
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
            content_hash: Some(hash),
            legacy_hashes: None,
            text_embedding: None,
            doc_text: None,
            tags: vec![],
            faces_evaluated: false,
            ocr_stage_ran: false,
            doc_stage_ran: false,
            text_stage_done: false,
            tags_evaluated: false,
            reuse_twin: false,
            heal_only: false,
            failure_is_transient: false,
            catch_up: None,
        }
    }

    fn analyzed(path: &str, hash: [u8; 32]) -> TaggedFile {
        let mut file = image(path, hash);
        file.phash = Some(42);
        file.aesthetic = Some(0.5);
        file.has_faces = true;
        file.has_text = true;
        file.ocr_text = Some("FileID OCR 12345".to_owned());
        file.image_width = 640;
        file.image_height = 480;
        let mut clip = vec![0.0f32; 512];
        clip[0] = 1.0;
        file.clip_embedding = Some(clip);
        let mut embedding = vec![0.0f32; 128];
        embedding[3] = 1.0;
        file.faces.push(DetectedFace {
            bbox: [10.0, 20.0, 30.0, 40.0],
            embedding,
            embedding_model: Some("fixture-weight-hash".to_owned()),
            roll: 0.0,
            yaw: 0.1,
            pitch: 0.2,
            quality: 0.9,
            excluded: false,
            crop_rgb_112: None,
        });
        file.tags = vec![("dog".to_owned(), Some(0.9)), ("park".to_owned(), Some(0.7)), ("Year_2020".to_owned(), None)];
        file.faces_evaluated = true;
        file.ocr_stage_ran = true;
        file.tags_evaluated = true;
        file
    }

    fn reused(path: &str, hash: [u8; 32], year: &str) -> TaggedFile {
        let mut file = image(path, hash);
        file.reuse_twin = true;
        file.tags = vec![(year.to_owned(), None)];
        file
    }

    #[test]
    fn a_copy_takes_over_its_twins_rows_and_keeps_its_own_year() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let hash = [9u8; 32];
        let mut buffer = vec![analyzed("/lib/a.jpg", hash), reused("/lib/backup/a.jpg", hash, "Year_2021")];
        writer.flush(&mut buffer, &mut 0, &mut 0, &mut 0, 0).unwrap();

        let db = conn.lock();
        let copy_id: i64 = db.query_row("SELECT id FROM files WHERE path_text = '/lib/backup/a.jpg'", [], |r| r.get(0)).unwrap();
        let (phash, has_faces, has_text, failed): (i64, i64, i64, i64) = db
            .query_row("SELECT phash, has_faces, has_text, failed FROM files WHERE id = ?1", params![copy_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        assert_eq!((phash, has_faces, has_text, failed), (42, 1, 1, 0));
        let mut tags: Vec<String> = db
            .prepare("SELECT tag FROM tags WHERE file_id = ?1 ORDER BY tag").unwrap()
            .query_map(params![copy_id], |r| r.get(0)).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap();
        tags.sort();
        assert_eq!(tags, vec!["Year_2021", "dog", "park"]);
        let clip: Vec<u8> = db.query_row("SELECT embedding FROM clip_embeddings WHERE file_id = ?1", params![copy_id], |r| r.get(0)).unwrap();
        assert_eq!(clip.len(), 512 * 4);
        let text: String = db.query_row("SELECT text FROM ocr_text WHERE file_id = ?1", params![copy_id], |r| r.get(0)).unwrap();
        assert_eq!(text, "FileID OCR 12345");
        let (faces, model): (i64, String) = db
            .query_row("SELECT count(*), max(embedding_model) FROM face_prints WHERE file_id = ?1", params![copy_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((faces, model.as_str()), (1, "fixture-weight-hash"));
        let total_faces: i64 = db.query_row("SELECT count(*) FROM face_prints", [], |r| r.get(0)).unwrap();
        assert_eq!(total_faces, 2, "the twin keeps its own face row");
    }

    #[test]
    fn a_copy_takes_the_camera_and_location_its_bytes_share_with_the_twin() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let hash = [11u8; 32];
        let mut twin = analyzed("/lib/a.jpg", hash);
        twin.camera_model = Some("iPhone 15 Pro".to_owned());
        twin.location_lat = Some(48.2);
        twin.location_lon = Some(16.4);
        writer.flush(&mut vec![twin, reused("/lib/b.jpg", hash, "Year_2021")], &mut 0, &mut 0, &mut 0, 0).unwrap();
        let db = conn.lock();
        let copy: (String, f64, f64) = db
            .query_row("SELECT camera_model, location_lat, location_lon FROM files WHERE path_text = '/lib/b.jpg'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!(copy, ("iPhone 15 Pro".to_owned(), 48.2, 16.4));
        let revisions: i64 = db.query_row("SELECT count(*) FROM catalog_revisions r JOIN files f ON f.id = r.file_id WHERE f.path_text = '/lib/b.jpg'", [], |r| r.get(0)).unwrap();
        assert_eq!(revisions, 1);
    }

    #[test]
    fn a_mac_scan_copy_stores_its_faces_without_vectors_or_a_file_revision() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let hash = [12u8; 32];
        let mut copy = reused("/lib/b.jpg", hash, "Year_2021");
        copy.vision_faces = Some(Vec::new());
        writer.flush(&mut vec![analyzed("/lib/a.jpg", hash), copy], &mut 0, &mut 0, &mut 0, 0).unwrap();
        let db = conn.lock();
        let face: (Vec<u8>, Option<Vec<u8>>, Option<String>, Option<String>, Option<String>, String) = db
            .query_row(
                "SELECT fp.print_data, fp.arcface_embedding, fp.embedding_model, fp.processing_version, fp.source_revision, fp.bbox \
                 FROM face_prints fp JOIN files f ON f.id = fp.file_id WHERE f.path_text = '/lib/b.jpg'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert!(face.0.is_empty() && face.1.is_none() && face.2.is_none() && face.3.is_none() && face.4.is_none());
        assert!(!face.5.is_empty(), "the face box is copied");
        let revisions: i64 = db.query_row("SELECT count(*) FROM catalog_revisions r JOIN files f ON f.id = r.file_id WHERE f.path_text = '/lib/b.jpg'", [], |r| r.get(0)).unwrap();
        assert_eq!(revisions, 0, "the Mac face pipeline records the file revision itself");
        let tags: i64 = db.query_row("SELECT count(*) FROM tags t JOIN files f ON f.id = t.file_id WHERE f.path_text = '/lib/b.jpg'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags, 3);
    }

    fn owed(conn: &Arc<Mutex<Connection>>, path: &str) -> Option<(i64, i64, f64, Option<String>, i64)> {
        conn.lock()
            .query_row(
                "SELECT c.stages, c.size_bytes, c.modified_at, c.vision, c.face_count FROM scan_catchup c \
                 JOIN files f ON f.id = c.file_id WHERE f.path_text = ?1",
                [path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .unwrap()
    }

    #[test]
    fn work_owed_is_stored_with_the_row_and_replaced_by_a_full_analysis() {
        use crate::pipeline::catch_up::{Marker, CAREFUL_TEXT_OWED, TAGS_OWED, TEXT_READ_AT_SCAN};
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let mut deferred = analyzed("/lib/a.jpg", [21u8; 32]);
        deferred.catch_up = Some(Marker { stages: TAGS_OWED | TEXT_READ_AT_SCAN, vision: Some("{\"k\":[],\"c\":[],\"u\":false}".into()), face_count: 2 });
        writer.flush(&mut vec![deferred.clone()], &mut 0, &mut 0, &mut 0, 0).unwrap();
        assert_eq!(
            owed(&conn, "/lib/a.jpg"),
            Some((TAGS_OWED | TEXT_READ_AT_SCAN, 1234, 1_700_000_000.0, Some("{\"k\":[],\"c\":[],\"u\":false}".to_string()), 2))
        );

        // A marker that owes nothing is not stored, and clears an earlier one.
        let mut settled = deferred.clone();
        settled.catch_up = Some(Marker { stages: TEXT_READ_AT_SCAN, vision: None, face_count: 0 });
        writer.flush(&mut vec![settled], &mut 0, &mut 0, &mut 0, 1).unwrap();
        assert_eq!(owed(&conn, "/lib/a.jpg"), None);

        // A row whose stages did not run (a timeout) leaves an earlier marker alone.
        writer.flush(&mut vec![deferred], &mut 0, &mut 0, &mut 0, 2).unwrap();
        let mut timed_out = image("/lib/a.jpg", [21u8; 32]);
        timed_out.catch_up = Some(Marker { stages: CAREFUL_TEXT_OWED, vision: None, face_count: 0 });
        writer.flush(&mut vec![timed_out], &mut 0, &mut 0, &mut 0, 3).unwrap();
        assert_eq!(owed(&conn, "/lib/a.jpg").map(|m| m.0), Some(TAGS_OWED | TEXT_READ_AT_SCAN));

        // An inline analysis (no marker) replaces it.
        writer.flush(&mut vec![analyzed("/lib/a.jpg", [21u8; 32])], &mut 0, &mut 0, &mut 0, 4).unwrap();
        assert_eq!(owed(&conn, "/lib/a.jpg"), None);
    }

    #[test]
    fn a_copy_owes_what_its_twin_owes_without_the_twins_own_year_and_camera_tags() {
        use crate::pipeline::catch_up::{Context, Marker, TAGS_OWED};
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let hash = [22u8; 32];
        let mut twin = analyzed("/lib/a.jpg", hash);
        let context = Context { k: vec!["park".into()], f: Some(true), x: Some(vec!["Year_2020".into()]), ..Context::default() };
        twin.catch_up = Some(Marker { stages: TAGS_OWED, vision: context.encoded(), face_count: 1 });
        let mut copy = reused("/lib/b.jpg", hash, "Year_2021");
        copy.size_bytes = 1234;
        copy.modified_unix = 1_711_111_111.0;
        writer.flush(&mut vec![twin, copy], &mut 0, &mut 0, &mut 0, 0).unwrap();

        let (stages, size, modified, vision, faces) = owed(&conn, "/lib/b.jpg").expect("the copy owes the tags too");
        assert_eq!((stages, size, modified, faces), (TAGS_OWED, 1234, 1_711_111_111.0, 1));
        let copied = Context::decode(vision.as_deref()).unwrap();
        assert_eq!(copied, Context { x: None, ..context });

        // Once the twin is settled, a later copy takes its final tags and owes nothing.
        conn.lock().execute("DELETE FROM scan_catchup WHERE file_id = (SELECT id FROM files WHERE path_text = '/lib/a.jpg')", []).unwrap();
        let mut again = reused("/lib/b.jpg", hash, "Year_2021");
        again.modified_unix = 1_711_111_111.0;
        writer.flush(&mut vec![again], &mut 0, &mut 0, &mut 0, 1).unwrap();
        assert_eq!(owed(&conn, "/lib/b.jpg"), None);
    }

    fn copy_after_spoiling_the_twin(spoil: &str, switch_off: bool) -> (i64, i64, i64) {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let hash = [31u8; 32];
        writer.flush(&mut vec![analyzed("/lib/a.jpg", hash)], &mut 0, &mut 0, &mut 0, 0).unwrap();
        conn.lock().execute_batch(spoil).unwrap();
        let mut copy = reused("/lib/b.jpg", hash, "Year_2021");
        copy.reuse_twin = !switch_off;
        writer.flush(&mut vec![copy], &mut 0, &mut 0, &mut 0, 1).unwrap();
        let db = conn.lock();
        db.query_row(
            "SELECT f.failed, (SELECT count(*) FROM tags WHERE file_id = f.id), (SELECT count(*) FROM clip_embeddings WHERE file_id = f.id) \
             FROM files f WHERE path_text = '/lib/b.jpg'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    #[test]
    fn a_copy_never_takes_over_a_failed_row_a_non_image_a_sampled_hash_or_a_row_with_reuse_switched_off() {
        let none = (1, 0, 0);
        assert_eq!(copy_after_spoiling_the_twin("UPDATE files SET failed = 1", false), none, "failed twin");
        assert_eq!(copy_after_spoiling_the_twin("UPDATE files SET kind = 'video'", false), none, "non-image twin");
        assert_eq!(copy_after_spoiling_the_twin("UPDATE files SET content_hash = substr(content_hash, 1, 16)", false), none, "sampled or short hash");
        assert_eq!(
            copy_after_spoiling_the_twin("DELETE FROM tags; DELETE FROM clip_embeddings", false),
            none,
            "a twin that kept no tag and no vector"
        );
        let (failed, tags, clips) = copy_after_spoiling_the_twin("SELECT 1", true);
        assert_eq!((failed, tags, clips), (0, 0, 0), "reuse off: the copy keeps only its own row and gets nothing of the twin's");
        let (failed, tags, clips) = copy_after_spoiling_the_twin("SELECT 1", false);
        assert_eq!((failed, tags, clips), (0, 3, 1), "positive control: the intact twin is taken over");
    }

    #[test]
    fn a_copy_of_an_earlier_scan_is_served_from_the_catalog() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let hash = [5u8; 32];
        writer.flush(&mut vec![analyzed("/lib/a.jpg", hash)], &mut 0, &mut 0, &mut 0, 0).unwrap();
        let known = crate::pipeline::twin_reuse::known_image_hashes(&conn.lock(), true).unwrap();
        assert_eq!(known, vec![hash]);

        writer.flush(&mut vec![reused("/lib/b.jpg", hash, "Year_2020")], &mut 0, &mut 0, &mut 0, 1).unwrap();
        let db = conn.lock();
        let tags: i64 = db.query_row("SELECT count(*) FROM tags t JOIN files f ON f.id = t.file_id WHERE f.path_text = '/lib/b.jpg'", [], |r| r.get(0)).unwrap();
        assert_eq!(tags, 3);
    }

    #[test]
    fn a_copy_without_a_twin_row_is_retried_next_scan_unless_it_has_results() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let hash = [7u8; 32];
        writer.flush(&mut vec![reused("/lib/lonely.jpg", hash, "Year_2020")], &mut 0, &mut 0, &mut 0, 0).unwrap();
        let failed: i64 = conn.lock().query_row("SELECT failed FROM files WHERE path_text = '/lib/lonely.jpg'", [], |r| r.get(0)).unwrap();
        assert_eq!(failed, 1);

        // The same path touched (new mtime, same bytes) keeps the results it already has.
        writer.flush(&mut vec![analyzed("/lib/own.jpg", hash)], &mut 0, &mut 0, &mut 0, 1).unwrap();
        let mut touched = reused("/lib/own.jpg", hash, "Year_2020");
        touched.modified_unix += 10.0;
        writer.flush(&mut vec![touched], &mut 0, &mut 0, &mut 0, 2).unwrap();
        let db = conn.lock();
        let (failed, tags): (i64, i64) = db
            .query_row("SELECT f.failed, (SELECT count(*) FROM tags WHERE file_id = f.id) FROM files f WHERE path_text = '/lib/own.jpg'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((failed, tags), (0, 3));
        let vectors: i64 = db
            .query_row("SELECT count(*) FROM clip_embeddings c JOIN files f ON f.id = c.file_id WHERE f.path_text = '/lib/own.jpg'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors, 1, "a date change with the same bytes must not cost the file its vector");
    }

    fn face_rows(conn: &Arc<Mutex<Connection>>, path: &str) -> Vec<String> {
        conn.lock()
            .prepare(
                "SELECT fp.id || '|' || COALESCE(fp.person_id, 'none') || '|' || fp.bbox FROM face_prints fp \
                 JOIN files f ON f.id = fp.file_id WHERE f.path_text = ?1 ORDER BY fp.id",
            )
            .unwrap()
            .query_map(params![path], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn touched_file_keeps(other_copy: bool) {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let hash = [11u8; 32];
        let mut first = vec![analyzed("/lib/own.jpg", hash)];
        if other_copy {
            first.push(analyzed("/lib/backup/own.jpg", hash));
        }
        writer.flush(&mut first, &mut 0, &mut 0, &mut 0, 0).unwrap();
        let file_id: i64 = {
            let db = conn.lock();
            let file_id: i64 = db.query_row("SELECT id FROM files WHERE path_text = '/lib/own.jpg'", [], |r| r.get(0)).unwrap();
            db.execute("INSERT INTO persons (name, file_count, created_at) VALUES ('Ada', 1, 0)", []).unwrap();
            db.execute("UPDATE face_prints SET person_id = ?1 WHERE file_id = ?2", params![db.last_insert_rowid(), file_id]).unwrap();
            db.execute("INSERT INTO tags (file_id, tag, source) VALUES (?1, 'holiday', 'user')", params![file_id]).unwrap();
            file_id
        };
        let faces_before = face_rows(&conn, "/lib/own.jpg");
        assert!(faces_before.len() == 1 && !faces_before[0].contains("none"));

        let mut touched = reused("/lib/own.jpg", hash, "Year_2020");
        touched.modified_unix += 3_600.0;
        writer.flush(&mut vec![touched], &mut 0, &mut 0, &mut 0, 1).unwrap();

        let db = conn.lock();
        let (id, failed, user_tags, vectors): (i64, i64, i64, i64) = db
            .query_row(
                "SELECT f.id, f.failed, (SELECT count(*) FROM tags WHERE file_id = f.id AND source = 'user'), \
                 (SELECT count(*) FROM clip_embeddings WHERE file_id = f.id) FROM files f WHERE path_text = '/lib/own.jpg'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        drop(db);
        assert_eq!((id, failed, user_tags, vectors), (file_id, 0, 1, 1), "other copy: {other_copy}");
        assert_eq!(face_rows(&conn, "/lib/own.jpg"), faces_before, "face rows and their people survive; other copy: {other_copy}");
    }

    #[test]
    fn a_file_whose_date_changed_but_bytes_did_not_keeps_its_row_faces_and_people() {
        touched_file_keeps(false);
    }

    #[test]
    fn a_touched_file_with_a_copy_elsewhere_keeps_its_own_faces_and_people() {
        touched_file_keeps(true);
    }

    fn tags_only(path: &str, hash: [u8; 32]) -> TaggedFile {
        let mut file = image(path, hash);
        file.tags = vec![("Year_2020".to_owned(), None)];
        file.tags_evaluated = true;
        file
    }

    #[test]
    fn a_row_without_a_vector_is_not_reusable_when_the_scan_can_embed() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        writer
            .flush(&mut vec![tags_only("/lib/tags-only.jpg", [9u8; 32]), analyzed("/lib/full.jpg", [10u8; 32])], &mut 0, &mut 0, &mut 0, 0)
            .unwrap();
        let db = conn.lock();
        assert_eq!(crate::pipeline::twin_reuse::known_image_hashes(&db, true).unwrap(), vec![[10u8; 32]]);
        let mut any = crate::pipeline::twin_reuse::known_image_hashes(&db, false).unwrap();
        any.sort();
        assert_eq!(any, vec![[9u8; 32], [10u8; 32]]);
    }

    #[test]
    fn a_copy_prefers_the_twin_that_has_a_vector() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let hash = [11u8; 32];
        writer.flush(&mut vec![tags_only("/lib/older.jpg", hash)], &mut 0, &mut 0, &mut 0, 0).unwrap();
        writer.flush(&mut vec![analyzed("/lib/newer.jpg", hash)], &mut 0, &mut 0, &mut 0, 1).unwrap();
        writer.flush(&mut vec![reused("/lib/copy.jpg", hash, "Year_2020")], &mut 0, &mut 0, &mut 0, 2).unwrap();
        let vectors: i64 = conn
            .lock()
            .query_row("SELECT count(*) FROM clip_embeddings c JOIN files f ON f.id = c.file_id WHERE f.path_text = '/lib/copy.jpg'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vectors, 1);
    }

    #[test]
    fn a_path_whose_bytes_changed_does_not_keep_the_old_vector() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        writer.flush(&mut vec![analyzed("/lib/own.jpg", [7u8; 32])], &mut 0, &mut 0, &mut 0, 0).unwrap();
        let mut replaced = reused("/lib/own.jpg", [8u8; 32], "Year_2020");
        replaced.modified_unix += 10.0;
        writer.flush(&mut vec![replaced], &mut 0, &mut 0, &mut 0, 1).unwrap();
        let vectors: i64 = conn.lock().query_row("SELECT count(*) FROM clip_embeddings", [], |r| r.get(0)).unwrap();
        assert_eq!(vectors, 0, "the vector of the old bytes must not be attached to new bytes");
    }
}

#[cfg(test)]
mod heal_only_tests {
    use super::*;
    use crate::pipeline::discovery::{DiscoveredFile, FileKind};
    use std::path::PathBuf;

    fn discovered(path: &str, file_ref: u64) -> DiscoveredFile {
        DiscoveredFile {
            path: PathBuf::from(path),
            kind: FileKind::Image,
            size_bytes: 1234,
            modified_unix: 1_700_000_000.0,
            created_unix: None,
            online_only: false,
            file_ref: Some(file_ref),
            moved_from_known_row: false,
        }
    }

    fn analyzed(path: &str, file_ref: u64) -> TaggedFile {
        let mut file = TaggedFile::bare(&discovered(path, file_ref));
        file.scanned_unix = 1_700_000_100.0;
        file.content_hash = Some([3u8; 32]);
        file.tags = vec![("dog".to_owned(), Some(0.9))];
        file.tags_evaluated = true;
        file
    }

    fn moved(path: &str, file_ref: u64) -> TaggedFile {
        let mut file = TaggedFile::bare(&discovered(path, file_ref));
        file.scanned_unix = 1_700_009_000.0;
        file.heal_only = true;
        file
    }

    fn seeded() -> (Arc<Mutex<Connection>>, DbWriter) {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        conn.lock().execute("INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES ('/seed', 0, 1, 0, 'other', '')", []).unwrap();
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        assert!(writer.rename_heal_enabled());
        (conn, writer)
    }

    fn rows(conn: &Arc<Mutex<Connection>>) -> Vec<(i64, String, f64, i64)> {
        let db = conn.lock();
        let mut stmt = db
            .prepare("SELECT f.id, f.path_text, f.scanned_at, (SELECT count(*) FROM tags WHERE file_id = f.id) FROM files f WHERE f.path_text != '/seed' ORDER BY f.id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    #[test]
    fn a_moved_file_rebinds_its_row_without_any_analysis() {
        let (conn, writer) = seeded();
        writer.flush(&mut vec![analyzed("/fileid-missing/old.jpg", 42)], &mut 0, &mut 0, &mut 0, 0).unwrap();
        let before = rows(&conn);
        let (mut total, mut heavy, mut failed) = (0, 0, 0);
        writer.flush(&mut vec![moved("/fileid-missing/new.jpg", 42)], &mut total, &mut heavy, &mut failed, 1).unwrap();
        let after = rows(&conn);
        assert_eq!(after, vec![(before[0].0, "/fileid-missing/new.jpg".to_string(), 1_700_009_000.0, 1)]);
        assert_eq!((total, heavy, failed), (1, 1, 0));
        let hash: Vec<u8> = conn.lock().query_row("SELECT content_hash FROM files WHERE id = ?1", [before[0].0], |r| r.get(0)).unwrap();
        assert_eq!(hash, vec![3u8; 32], "the stored analysis is untouched");
    }

    #[test]
    fn a_move_that_can_no_longer_be_rebound_writes_nothing() {
        // The row changed since the walk: it no longer describes this file.
        let (conn, writer) = seeded();
        writer.flush(&mut vec![analyzed("/fileid-missing/old.jpg", 42)], &mut 0, &mut 0, &mut 0, 0).unwrap();
        conn.lock().execute("UPDATE files SET modified_at = modified_at + 5 WHERE file_ref = 42", []).unwrap();
        let before = rows(&conn);
        let mut total = 0;
        writer.flush(&mut vec![moved("/fileid-missing/new.jpg", 42)], &mut total, &mut 0, &mut 0, 1).unwrap();
        assert_eq!(rows(&conn), before);
        assert_eq!(total, 1, "the file still counts toward the scan's progress");

        // The new path was taken by another row in the meantime.
        let (conn, writer) = seeded();
        writer.flush(&mut vec![analyzed("/fileid-missing/old.jpg", 42), analyzed("/fileid-missing/new.jpg", 43)], &mut 0, &mut 0, &mut 0, 0).unwrap();
        let before = rows(&conn);
        writer.flush(&mut vec![moved("/fileid-missing/new.jpg", 42)], &mut 0, &mut 0, &mut 0, 1).unwrap();
        assert_eq!(rows(&conn), before);

        // The old path is back on disk (a hard link or a copy with the same id).
        let dir = std::env::temp_dir().join(format!("fileid-heal-only-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("old.jpg");
        std::fs::write(&old, b"x").unwrap();
        let (conn, writer) = seeded();
        writer.flush(&mut vec![analyzed(&old.to_string_lossy(), 42)], &mut 0, &mut 0, &mut 0, 0).unwrap();
        let before = rows(&conn);
        writer.flush(&mut vec![moved(&dir.join("new.jpg").to_string_lossy(), 42)], &mut 0, &mut 0, &mut 0, 1).unwrap();
        assert_eq!(rows(&conn), before);
        let _ = std::fs::remove_dir_all(&dir);

        // No candidate at all.
        let (conn, writer) = seeded();
        writer.flush(&mut vec![moved("/fileid-missing/new.jpg", 99)], &mut 0, &mut 0, &mut 0, 0).unwrap();
        assert!(rows(&conn).is_empty());
    }
}

#[cfg(test)]
mod failure_memory_tests {
    use super::*;
    use crate::pipeline::catch_up;
    use crate::pipeline::discovery::{DiscoveredFile, FileKind};
    use std::path::PathBuf;

    fn file(path: &str, failed: bool) -> TaggedFile {
        let mut file = TaggedFile::bare(&DiscoveredFile {
            path: PathBuf::from(path),
            kind: FileKind::Image,
            size_bytes: 1234,
            modified_unix: 1_700_000_000.0,
            created_unix: None,
            online_only: false,
            file_ref: None,
            moved_from_known_row: false,
        });
        file.failed = failed;
        file.error_message = failed.then(|| "image decode: broken".to_string());
        file.tags_evaluated = !failed;
        file
    }

    fn memory(conn: &Arc<Mutex<Connection>>, path: &str) -> Option<(i64, i64, String)> {
        conn.lock()
            .query_row(
                "SELECT 4, c.attempts, c.engine_build FROM scan_failures c JOIN files f ON f.id = c.file_id WHERE f.path_text = ?1",
                [path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .unwrap()
    }

    /// A failure is only recorded for a file that is still on disk.
    fn on_disk(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("fileid-failure-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, b"broken").unwrap();
        path.to_string_lossy().into_owned()
    }

    fn given_up(conn: &Arc<Mutex<Connection>>) -> Vec<String> {
        let db = conn.lock();
        let mut stmt = db.prepare(catch_up::GIVEN_UP_SQL).unwrap();
        stmt.query_map([catch_up::failure_context()], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    #[test]
    fn three_failures_of_the_same_file_are_remembered_and_a_success_forgets_them() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let broken = on_disk("broken.jpg");
        for attempt in 1..=3 {
            writer.flush(&mut vec![file(&broken, true)], &mut 0, &mut 0, &mut 0, attempt).unwrap();
            assert_eq!(memory(&conn, &broken), Some((4, i64::from(attempt), catch_up::failure_context().to_string())));
            assert_eq!(given_up(&conn).len(), usize::from(attempt == 3));
        }
        let (failed, message): (i64, String) = conn
            .lock()
            .query_row("SELECT failed, error_message FROM files WHERE path_text = ?1", [&broken], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((failed, message.as_str()), (1, "image decode: broken"), "the row the Library lists as unreadable is kept");

        writer.flush(&mut vec![file(&broken, false)], &mut 0, &mut 0, &mut 0, 4).unwrap();
        assert_eq!(memory(&conn, &broken), None);
        assert!(given_up(&conn).is_empty());
    }

    #[test]
    fn a_changed_file_or_engine_starts_counting_again_and_transient_failures_never_count() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let (a, offline) = (on_disk("a.jpg"), on_disk("offline.jpg"));
        for attempt in 0..3 {
            writer.flush(&mut vec![file(&a, true)], &mut 0, &mut 0, &mut 0, attempt).unwrap();
        }
        assert_eq!(given_up(&conn), vec![a.clone()]);

        // Edited on disk: the next failure is the first for the new content.
        let mut edited = file(&a, true);
        edited.modified_unix += 60.0;
        writer.flush(&mut vec![edited], &mut 0, &mut 0, &mut 0, 3).unwrap();
        assert_eq!(memory(&conn, &a).map(|m| m.1), Some(1));
        assert!(given_up(&conn).is_empty());

        // Failures recorded by another engine build do not count for this one.
        conn.lock().execute("UPDATE scan_failures SET attempts = 3, engine_build = 'fileid-engine 0.0.1 portable'", []).unwrap();
        assert!(given_up(&conn).is_empty());
        let mut again = file(&a, true);
        again.modified_unix += 60.0;
        writer.flush(&mut vec![again], &mut 0, &mut 0, &mut 0, 4).unwrap();
        assert_eq!(memory(&conn, &a).map(|m| m.1), Some(1));

        // The stored row changed without a scan (size differs): not given up.
        conn.lock().execute("UPDATE scan_failures SET attempts = 3", []).unwrap();
        assert_eq!(given_up(&conn).len(), 1);
        conn.lock().execute("UPDATE files SET size_bytes = size_bytes + 1", []).unwrap();
        assert!(given_up(&conn).is_empty());

        let mut transient = file(&offline, true);
        transient.failure_is_transient = true;
        for attempt in 5..9 {
            writer.flush(&mut vec![transient.clone()], &mut 0, &mut 0, &mut 0, attempt).unwrap();
        }
        assert_eq!(memory(&conn, &offline), None);
    }

    #[test]
    fn a_failure_replaces_work_owed_from_an_earlier_scan() {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let a = on_disk("a.jpg");
        writer.flush(&mut vec![file(&a, false)], &mut 0, &mut 0, &mut 0, 0).unwrap();
        conn.lock()
            .execute(
                "INSERT INTO scan_catchup (file_id, stages, size_bytes, modified_at, vision, face_count, attempts, created_at) \
                 SELECT id, 1, size_bytes, modified_at, '{}', 2, 2, 0 FROM files",
                [],
            )
            .unwrap();
        writer.flush(&mut vec![file(&a, true)], &mut 0, &mut 0, &mut 0, 1).unwrap();
        assert_eq!(memory(&conn, &a).map(|m| (m.0, m.1)), Some((4, 1)));
    }
}


#[cfg(test)]
mod rescan_preservation_tests {
    use super::*;
    use crate::pipeline::discovery::{DiscoveredFile, FileKind};
    use std::path::PathBuf;

    const PATH: &str = "/lib/IMG_0001.jpg";

    fn scan(modified_unix: f64) -> TaggedFile {
        TaggedFile::bare(&DiscoveredFile {
            path: PathBuf::from(PATH),
            kind: FileKind::Image,
            size_bytes: 4,
            modified_unix,
            created_unix: None,
            online_only: false,
            file_ref: None,
            moved_from_known_row: false,
        })
    }

    fn analysed(modified_unix: f64, tags: &[&str], ocr: &str) -> TaggedFile {
        let mut file = scan(modified_unix);
        file.tags = tags.iter().map(|tag| ((*tag).to_string(), None)).collect();
        file.tags_evaluated = true;
        file.faces_evaluated = true;
        file.ocr_stage_ran = true;
        file.ocr_text = Some(ocr.to_string());
        file
    }

    fn flush(writer: &DbWriter, file: TaggedFile, batch: u32) {
        writer.flush(&mut vec![file], &mut 0, &mut 0, &mut 0, batch).unwrap();
    }

    struct Seeded {
        conn: Arc<Mutex<Connection>>,
        writer: DbWriter,
        file_id: i64,
        face_id: i64,
        person_id: i64,
    }

    /// A scanned file with an automatic tag, OCR text, one face the user has
    /// named, and a tag the user typed.
    fn seeded() -> Seeded {
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        flush(&writer, analysed(1_700_000_000.0, &["beach"], "alpha bravo"), 0);
        let (file_id, face_id, person_id) = {
            let db = conn.lock();
            let file_id: i64 = db.query_row("SELECT id FROM files WHERE path_text = ?1", [PATH], |r| r.get(0)).unwrap();
            db.execute("INSERT INTO persons (name, file_count, created_at) VALUES ('Mom', 1, 0)", []).unwrap();
            let person_id = db.last_insert_rowid();
            db.execute(
                "INSERT INTO face_prints (file_id, person_id, print_data, bbox) VALUES (?1, ?2, X'01', '{}')",
                params![file_id, person_id],
            )
            .unwrap();
            let face_id = db.last_insert_rowid();
            db.execute("INSERT INTO tags (file_id, tag, source) VALUES (?1, 'keepme', 'user')", [file_id]).unwrap();
            (file_id, face_id, person_id)
        };
        Seeded { conn, writer, file_id, face_id, person_id }
    }

    fn state(seed: &Seeded) -> (Option<i64>, Vec<String>, Vec<Option<i64>>, Vec<String>, i64) {
        let db = seed.conn.lock();
        let id = db.query_row("SELECT id FROM files WHERE path_text = ?1", [PATH], |r| r.get(0)).ok();
        let tags = db
            .prepare("SELECT tag || ':' || source FROM tags ORDER BY tag")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let people = db
            .prepare("SELECT person_id FROM face_prints ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let text = db
            .prepare("SELECT text FROM ocr_text ORDER BY file_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let failed = db.query_row("SELECT failed FROM files WHERE path_text = ?1", [PATH], |r| r.get(0)).unwrap();
        (id, tags, people, text, failed)
    }

    #[test]
    fn a_rewrite_keeps_the_row_id_and_the_people_the_user_named() {
        let seed = seeded();
        let before = state(&seed);
        assert_eq!(before.2, vec![Some(seed.person_id)]);

        // The same file again with the face stage skipped (the usual state of
        // a scan that reads only what changed).
        let mut again = analysed(1_700_000_000.0, &["beach"], "alpha bravo");
        again.faces_evaluated = false;
        flush(&seed.writer, again, 1);

        let after = state(&seed);
        assert_eq!(after.0, Some(seed.file_id), "files.id survives a rewrite");
        assert_eq!(after.2, vec![Some(seed.person_id)], "the named face stays named");
        assert_eq!(after.1, vec!["beach:auto".to_string(), "keepme:user".to_string()]);
        assert_eq!(seed.conn.lock().query_row("SELECT id FROM face_prints", [], |r| r.get::<_, i64>(0)).unwrap(), seed.face_id);
    }

    #[test]
    fn a_changed_file_whose_stages_did_not_run_keeps_its_tags_people_and_text() {
        let seed = seeded();
        // A read that timed out, or models that were not ready: the file
        // changed, nothing was produced, no stage ran.
        let changed = scan(1_700_000_060.0);
        flush(&seed.writer, changed, 1);

        let after = state(&seed);
        assert_eq!(after.0, Some(seed.file_id));
        assert_eq!(after.1, vec!["beach:auto".to_string(), "keepme:user".to_string()], "tags survive");
        assert_eq!(after.2, vec![Some(seed.person_id)], "the person assignment survives");
        assert_eq!(after.3, vec!["alpha bravo".to_string()], "the text survives");
        assert_eq!(after.4, 0);
    }

    #[test]
    fn a_failed_read_of_a_changed_file_records_the_failure_and_keeps_what_was_found_before() {
        let seed = seeded();
        let mut failed = scan(1_700_000_060.0);
        failed.failed = true;
        failed.error_message = Some("image decode: broken".to_string());
        flush(&seed.writer, failed, 1);

        let after = state(&seed);
        assert_eq!(after.0, Some(seed.file_id), "the row is not replaced");
        assert_eq!(after.4, 1, "the failure is recorded");
        assert_eq!(after.1, vec!["beach:auto".to_string(), "keepme:user".to_string()]);
        assert_eq!(after.2, vec![Some(seed.person_id)]);
        assert_eq!(after.3, vec!["alpha bravo".to_string()]);

        // The next good read clears the failure and replaces the old results.
        flush(&seed.writer, analysed(1_700_000_060.0, &["sunset"], "charlie"), 2);
        let healed = state(&seed);
        assert_eq!(healed.0, Some(seed.file_id));
        assert_eq!(healed.4, 0);
        assert_eq!(healed.1, vec!["keepme:user".to_string(), "sunset:auto".to_string()], "automatic tags are replaced, the user's stay");
        assert_eq!(healed.3, vec!["charlie".to_string()]);
    }

    #[test]
    fn a_file_deleted_after_discovery_gets_no_failed_row_but_a_broken_one_does() {
        let dir = std::env::temp_dir().join(format!("fileid-vanish-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (gone, broken, known) = (dir.join("deleted.jpg"), dir.join("broken.jpg"), dir.join("known.jpg"));
        std::fs::write(&broken, b"not a jpeg").unwrap();
        std::fs::write(&known, b"jpeg").unwrap();
        let conn = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let writer = DbWriter::new(conn.clone(), ScanCoordinator::new());
        let mut first = analysed(1_700_000_000.0, &["beach"], "alpha");
        first.path = known.clone();
        writer.flush(&mut vec![first], &mut 0, &mut 0, &mut 0, 0).unwrap();
        std::fs::remove_file(&known).unwrap();
        let failure = |path: &PathBuf| {
            let mut file = scan(1_700_000_000.0);
            file.path = path.clone();
            file.failed = true;
            file.error_message = Some("Could not decode image".to_string());
            file
        };
        let (mut total, mut failed) = (0, 0);
        writer
            .flush(&mut vec![failure(&gone), failure(&broken), failure(&known)], &mut total, &mut 0, &mut failed, 1)
            .unwrap();
        let rows: Vec<(String, bool)> = conn
            .lock()
            .prepare("SELECT path_text, failed FROM files ORDER BY path_text")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let tags: i64 = conn.lock().query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let text = |p: &PathBuf| p.to_string_lossy().into_owned();
        assert_eq!(rows, vec![(text(&broken), true), (text(&known), true)], "a known file that went missing keeps its row for the orphan sweep");
        assert_eq!(tags, 1, "the known file's tags survive its failed read");
        assert_eq!((total, failed), (3, 3), "the vanished file still counts as a failure of this scan");
    }

    fn unit_vector() -> Vec<f32> {
        let mut vector = vec![0.0f32; crate::models::clip_space::DIMENSION];
        vector[0] = 1.0;
        vector
    }

    fn clip_rows(seed: &Seeded) -> Vec<(i64, String)> {
        seed.conn
            .lock()
            .prepare("SELECT file_id, model FROM clip_embeddings")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn rescanning_an_unchanged_image_fills_a_missing_vector_once_and_never_duplicates_it() {
        let seed = seeded();
        assert!(clip_rows(&seed).is_empty(), "the first scan had no CLIP model");
        for batch in 1..=2 {
            let mut again = analysed(1_700_000_000.0, &["beach"], "alpha bravo");
            again.clip_embedding = Some(unit_vector());
            flush(&seed.writer, again, batch);
            assert_eq!(clip_rows(&seed), vec![(seed.file_id, crate::models::clip_space::model_id().to_string())]);
        }
        assert_eq!(state(&seed).1, vec!["beach:auto".to_string(), "keepme:user".to_string()]);
    }

    #[test]
    fn a_vector_from_an_older_model_is_replaced_in_place_and_the_users_tags_stay() {
        let seed = seeded();
        seed.conn
            .lock()
            .execute(
                "INSERT INTO clip_embeddings (file_id, embedding, model) VALUES (?1, X'00', 'clip_legacy')",
                [seed.file_id],
            )
            .unwrap();
        let mut again = analysed(1_700_000_000.0, &["beach"], "alpha bravo");
        again.clip_embedding = Some(unit_vector());
        flush(&seed.writer, again, 1);
        let rows = clip_rows(&seed);
        assert_eq!(rows, vec![(seed.file_id, crate::models::clip_space::model_id().to_string())]);
        let blob: Vec<u8> = seed.conn.lock().query_row("SELECT embedding FROM clip_embeddings", [], |r| r.get(0)).unwrap();
        assert_eq!(blob.len(), crate::models::clip_space::DIMENSION * 4);
        assert!(state(&seed).1.contains(&"keepme:user".to_string()));
    }

    #[test]
    fn a_file_that_timed_out_is_walked_again_even_though_its_date_is_unchanged() {
        let seed = seeded();
        let sql = format!(
            "SELECT path_text FROM files WHERE failed = 0 {}",
            crate::scan_session::SKIP_SET_CONTENT_HASH_GATE
        );
        let skipped = |seed: &Seeded| -> Vec<String> {
            let db = seed.conn.lock();
            db.execute("UPDATE files SET content_hash = X'01'", []).ok();
            db.prepare(&sql).unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect()
        };
        assert_eq!(skipped(&seed), vec![PATH.to_string()], "a cleanly scanned file is skipped");

        let mut timed_out = scan(1_700_000_000.0);
        timed_out.failed = true;
        timed_out.error_message = Some("per-file timeout after 60s".into());
        flush(&seed.writer, timed_out, 1);
        assert!(skipped(&seed).is_empty(), "a timed-out file is marked failed, so the next scan walks it again");
    }
}
