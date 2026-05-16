// DBWriter — drains the Tagging → DB channel and writes 100-file or
// 200ms batches into the single SQLite writer connection.
//
// Mirror of macOS engine/Sources/FileIDEngine/Pipeline/DBWriter.swift.
// Single-writer is by design (matches Database.swift's invariant): WAL
// permits concurrent readers but only one writer. Every insert + the
// resume cursor update + the FTS5 OCR row land in the same transaction
// so a crash mid-batch leaves no partial state.
//
// Phase 2.1 cut: writes the `files` row (path, size, kind, scanned_at,
// modified_at, has_faces, has_text). Phase 2.2 extends it with the EXIF
// columns + phash + clip_embeddings; 2.3 adds OCR FTS5 + face rows.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use tokio::sync::mpsc;

use crate::coordinator::ScanCoordinator;
use crate::pipeline::tagging::TaggedFile;

/// Flush trigger thresholds. Mirror of macOS DBWriter.
const BATCH_SIZE: usize = 100;
const FLUSH_INTERVAL: Duration = Duration::from_millis(200);

/// Stats reported per batch — fed into the `batchSummary` IPC event so
/// the app sidebar can show throughput in real time.
#[derive(Debug, Clone, Default)]
pub struct BatchStats {
    pub batch_index: u32,
    pub files_in_batch: u32,
    pub processed_total: u64,
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
        let mut buffer: Vec<TaggedFile> = Vec::with_capacity(BATCH_SIZE);
        let mut deadline: Option<Instant> = None;
        let mut total: u64 = 0;
        let mut failed: u64 = 0;
        let mut batch_index: u32 = 0;

        loop {
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
                    if buffer.len() >= BATCH_SIZE {
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

        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction().context("opening tx")?;
        {
            let mut file_stmt = tx.prepare_cached(INSERT_FILE_SQL).context("preparing file insert")?;
            let mut id_stmt = tx
                .prepare_cached("SELECT id FROM files WHERE path_text = ?1")
                .context("preparing file id lookup")?;
            let mut clip_stmt = tx
                .prepare_cached(INSERT_CLIP_SQL)
                .context("preparing clip insert")?;
            let mut face_delete = tx
                .prepare_cached("DELETE FROM face_prints WHERE file_id = ?1")
                .context("preparing face delete")?;
            let mut face_stmt = tx
                .prepare_cached(INSERT_FACE_SQL)
                .context("preparing face insert")?;
            let mut ocr_text_stmt = tx
                .prepare_cached("INSERT OR REPLACE INTO ocr_text (file_id, text) VALUES (?1, ?2)")
                .context("preparing ocr_text insert")?;
            let mut ocr_fts_delete = tx
                .prepare_cached("DELETE FROM ocr_fts WHERE rowid = ?1")
                .context("preparing ocr_fts delete")?;
            let mut ocr_fts_stmt = tx
                .prepare_cached("INSERT INTO ocr_fts (rowid, text) VALUES (?1, ?2)")
                .context("preparing ocr_fts insert")?;
            for f in buffer.iter() {
                let insert_started = Instant::now();
                let path_text = f.path.to_string_lossy();
                let path_hash = crate::util::path_safety::stable_path_hash(&path_text);
                let extension = f
                    .path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();

                file_stmt.execute(params![
                    path_text,
                    path_hash,
                    f.size_bytes as i64,
                    None::<f64>,
                    f.modified_unix,
                    f.scanned_unix,
                    f.kind.as_str(),
                    extension,
                    f.phash,
                    None::<f64>, // aesthetic
                    f.has_faces as i64,
                    f.has_text as i64,
                    f.camera_model,
                    f.location_lat,
                    f.location_lon,
                    f.failed as i64,
                    f.error_message,
                ])
                .with_context(|| format!("insert {}", path_text))?;

                let file_id: i64 = id_stmt
                    .query_row(params![path_text], |row| row.get(0))
                    .with_context(|| format!("lookup id for {}", path_text))?;

                if let Some(emb) = &f.clip_embedding {
                    let bytes = floats_to_le_bytes(emb);
                    clip_stmt
                        .execute(params![file_id, bytes, "mobileclip_s2"])
                        .with_context(|| format!("clip insert for {}", path_text))?;
                }

                if !f.faces.is_empty() {
                    face_delete
                        .execute(params![file_id])
                        .with_context(|| format!("face delete for {}", path_text))?;
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
                            .with_context(|| format!("face insert for {}", path_text))?;

                        if let Some(crop) = &face.crop_rgb_112 {
                            let face_id = tx.last_insert_rowid();
                            if let Err(err) = save_face_crop(face_id, crop) {
                                tracing::warn!(?err, face_id, "face crop write failed");
                            }
                        }
                    }
                }

                if let Some(text) = &f.ocr_text {
                    if !text.trim().is_empty() {
                        ocr_text_stmt
                            .execute(params![file_id, text])
                            .with_context(|| format!("ocr_text insert for {}", path_text))?;
                        // ocr_fts is a contentless FTS5 view; rebuild row by id.
                        let _ = ocr_fts_delete.execute(params![file_id]);
                        ocr_fts_stmt
                            .execute(params![file_id, text])
                            .with_context(|| format!("ocr_fts insert for {}", path_text))?;
                    }
                }

                if f.failed {
                    *failed += 1;
                }
                *total += 1;
                vision.push(f.vision_ms);
                clip.push(f.clip_ms);
                store.push(insert_started.elapsed().as_secs_f64() * 1000.0);
            }
        }
        tx.commit().context("commit batch")?;

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
            wall_seconds: wall,
            files_per_second: if wall > 0.0 { f64::from(files_in_batch) / wall } else { 0.0 },
            utilization: 0.0, // Phase 2.2: derive from worker idle samples
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

const INSERT_FILE_SQL: &str = r#"
    INSERT INTO files (
        path_text, path_hash, size_bytes,
        created_at, modified_at, scanned_at,
        kind, extension,
        phash, aesthetic,
        has_faces, has_text,
        camera_model, location_lat, location_lon,
        failed, error_message
    )
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
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
        error_message= excluded.error_message
"#;

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
