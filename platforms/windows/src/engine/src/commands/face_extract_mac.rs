//! Pending face vectors in a macOS face space, as the Swift engine's
//! `FaceAnalysisCache` and `FaceClustering.extractPendingPrints` keep them: a
//! face needs a vector when it has none, or one from another model, processing
//! or file version; a vector is saved only while the face, its box and its
//! photo's size and time are unchanged; a face that fails backs off (1 min
//! doubling to 1 h). The extractor itself is behind [`Extractor`] (the Mac
//! pipeline in `apple::faces`), so this file stays portable and testable.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};

pub const MAX_PER_BATCH: usize = 5000;
const EXTRACTION_THREADS: usize = 4;

/// A face row whose vector needs computing.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub id: i64,
    pub file_id: i64,
    pub bbox: String,
    pub path: String,
    pub size: i64,
    pub modified: Option<f64>,
}

impl Input {
    /// `FaceCacheProvenance.sourceRevision`.
    pub fn revision(&self) -> String {
        source_revision(self.size, self.modified)
    }
}

pub fn source_revision(size: i64, modified: Option<f64>) -> String {
    match modified {
        Some(modified) => format!("{size}:{}", modified.to_bits()),
        None => format!("{size}:unknown"),
    }
}

fn normalized(blob: Option<&[u8]>) -> bool {
    let Some(blob) = blob.filter(|blob| blob.len() == 512) else { return false };
    let mut sum = 0.0f64;
    for chunk in blob.as_chunks::<4>().0 {
        let value = f32::from_le_bytes(*chunk);
        if !value.is_finite() {
            return false;
        }
        sum += f64::from(value) * f64::from(value);
    }
    (0.95..=1.05).contains(&sum)
}

/// `FaceAnalysisCache.pending`: faces after `after` (by id) without a current
/// vector in `model` / `processing`, skipping those still backing off.
pub fn pending(conn: &Connection, model: &str, processing: &str, limit: usize, after: i64, now: f64) -> rusqlite::Result<Vec<Input>> {
    let mut statement = conn.prepare_cached(
        "SELECT fp.id,fp.file_id,fp.bbox,f.path_text,f.size_bytes,f.modified_at,fp.arcface_embedding,fp.embedding_model,
                fp.processing_version,fp.source_revision,r.revision
         FROM face_prints fp JOIN files f ON f.id=fp.file_id LEFT JOIN catalog_revisions r ON r.file_id=f.id
         WHERE f.failed=0 AND fp.id>?1
           AND NOT EXISTS (SELECT 1 FROM face_refresh_failures q WHERE q.face_id=fp.id
               AND q.embedding_model=?2 AND q.processing_version=?3 AND q.source_path=f.path_text
               AND q.source_size=f.size_bytes AND q.source_modified IS f.modified_at
               AND q.bbox=fp.bbox AND q.retry_after>?4)
         ORDER BY fp.id",
    )?;
    let mut rows = statement.query(params![after, model, processing, now])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let size: i64 = row.get(4)?;
        let modified: Option<f64> = row.get(5)?;
        let embedding: Option<Vec<u8>> = row.get(6)?;
        let stored_model: Option<String> = row.get(7)?;
        let stored_processing: Option<String> = row.get(8)?;
        let stored_revision: Option<String> = row.get(9)?;
        let catalog_revision: Option<String> = row.get(10)?;
        let file_revision = modified.filter(|m| m.is_finite()).map(|m| source_revision(size, Some(m)));
        let current = normalized(embedding.as_deref())
            && stored_model.as_deref() == Some(model)
            && stored_processing.as_deref() == Some(processing)
            && stored_revision.is_some()
            && catalog_revision.is_some()
            && stored_revision == catalog_revision
            && file_revision.is_some()
            && stored_revision == file_revision;
        if current {
            continue;
        }
        out.push(Input { id: row.get(0)?, file_id: row.get(1)?, bbox: row.get(2)?, path: row.get(3)?, size, modified });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// `FaceAnalysisCache.persist`: saves the vector while the photo and the face
/// row are unchanged. True when the row was updated.
pub fn persist(conn: &Connection, input: &Input, embedding: &[f32], model: &str, processing: &str, now: f64) -> rusqlite::Result<bool> {
    let blob: Vec<u8> = embedding.iter().flat_map(|v| v.to_le_bytes()).collect();
    if model.is_empty() || model.chars().count() > 150 || !normalized(Some(&blob)) {
        return Ok(false);
    }
    let file: Option<(String, i64, Option<f64>)> = conn
        .query_row("SELECT path_text,size_bytes,modified_at FROM files WHERE id=?1", [input.file_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    let Some((path, size, modified)) = file else { return Ok(false) };
    if path != input.path || size != input.size || modified != input.modified {
        return Ok(false);
    }
    let present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM face_prints WHERE id=?1 AND file_id=?2 AND bbox=?3",
        params![input.id, input.file_id, input.bbox],
        |r| r.get(0),
    )?;
    if present != 1 {
        return Ok(false);
    }
    let revision = input.revision();
    conn.execute(
        "INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?1,?2,'file-stat-v1',?3)
         ON CONFLICT(file_id) DO UPDATE SET revision=excluded.revision,processing_version=excluded.processing_version,updated_at=excluded.updated_at
         WHERE catalog_revisions.revision!=excluded.revision",
        params![input.file_id, revision, now],
    )?;
    let changed = conn.execute(
        "UPDATE face_prints SET arcface_embedding=?1,embedding_model=?2,processing_version=?3,source_revision=?4 WHERE id=?5 AND file_id=?6 AND bbox=?7",
        params![blob, model, processing, revision, input.id, input.file_id, input.bbox],
    )?;
    if changed == 1 {
        conn.execute("DELETE FROM face_refresh_failures WHERE face_id=?1", [input.id])?;
    }
    Ok(changed == 1)
}

/// `FaceAnalysisCache.recordFailure`: retry after 1 min, doubling per attempt
/// up to 1 h, while the photo and box stay the same.
pub fn record_failure(conn: &Connection, input: &Input, model: &str, processing: &str, reason: &str, now: f64) -> rusqlite::Result<()> {
    if !now.is_finite() || model.is_empty() || model.chars().count() > 150 || !["source_unavailable_or_changed", "no_embedding"].contains(&reason) {
        return Ok(());
    }
    let present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM face_prints fp JOIN files f ON f.id=fp.file_id WHERE fp.id=?1 AND fp.file_id=?2 AND fp.bbox=?3
         AND f.path_text=?4 AND f.size_bytes=?5 AND f.modified_at IS ?6",
        params![input.id, input.file_id, input.bbox, input.path, input.size, input.modified],
        |r| r.get(0),
    )?;
    if present != 1 {
        return Ok(());
    }
    let previous: i64 = conn
        .query_row(
            "SELECT attempts FROM face_refresh_failures WHERE face_id=?1 AND embedding_model=?2 AND processing_version=?3
             AND source_path=?4 AND source_size=?5 AND source_modified IS ?6 AND bbox=?7",
            params![input.id, model, processing, input.path, input.size, input.modified, input.bbox],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let attempts = (previous + 1).min(16);
    let retry_after = now + (60.0 * 2f64.powi(attempts as i32 - 1)).min(3600.0);
    conn.execute(
        "INSERT INTO face_refresh_failures(face_id,embedding_model,processing_version,source_revision,
             source_path,source_size,source_modified,bbox,attempts,retry_after,reason,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT(face_id) DO UPDATE SET
             embedding_model=excluded.embedding_model,processing_version=excluded.processing_version,
             source_revision=excluded.source_revision,source_path=excluded.source_path,
             source_size=excluded.source_size,source_modified=excluded.source_modified,bbox=excluded.bbox,
             attempts=excluded.attempts,retry_after=excluded.retry_after,reason=excluded.reason,updated_at=excluded.updated_at",
        params![input.id, model, processing, input.revision(), input.path, input.size, input.modified, input.bbox, attempts, retry_after, reason, now],
    )?;
    Ok(())
}

/// One face's vector and crop JPEG.
pub struct Embedded {
    pub embedding: Vec<f32>,
    pub jpeg: Option<Vec<u8>>,
}

/// Computes the vectors of one photo's faces (one result per box, in order).
pub trait Extractor: Sync {
    fn model(&self) -> &str;
    fn extract(&self, path: &Path, boxes: &[String]) -> Vec<Option<Embedded>>;
}

/// The photo's size, Foundation modification time and inode, so a file that
/// changes while its faces are read is caught (`currentIdentity`).
fn identity(path: &str) -> Option<(i64, f64, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = super::face_clustering_mac::foundation_mtime(&metadata)?;
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&metadata);
    #[cfg(not(unix))]
    let inode = 0u64;
    Some((i64::try_from(metadata.len()).ok()?, modified, inode))
}

fn current(rows: &[Input]) -> Option<(i64, f64, u64)> {
    let first = rows.first()?;
    let found = identity(&first.path)?;
    rows.iter().all(|row| Some(found.0) == Some(row.size) && row.modified == Some(found.1)).then_some(found)
}

struct FileResult {
    rows: Vec<Input>,
    vectors: Vec<Option<Embedded>>,
    unavailable: bool,
}

fn extract_one(rows: Vec<Input>, extractor: &dyn Extractor) -> FileResult {
    let Some(before) = current(&rows) else { return FileResult { vectors: rows.iter().map(|_| None).collect(), rows, unavailable: true } };
    let boxes: Vec<String> = rows.iter().map(|row| row.bbox.clone()).collect();
    let mut vectors = extractor.extract(Path::new(&rows[0].path), &boxes);
    vectors.resize_with(rows.len(), || None);
    if current(&rows) != Some(before) {
        return FileResult { vectors: rows.iter().map(|_| None).collect(), rows, unavailable: true };
    }
    FileResult { rows, vectors, unavailable: false }
}

/// What one extraction pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub pending: usize,
    pub extracted: usize,
    pub failed: usize,
    pub files: usize,
}

/// `FaceClustering.extractPendingPrints`: batches of [`MAX_PER_BATCH`] pending
/// faces (`skip` left out), photos read on a few threads outside the catalog
/// lock, vectors saved and failures recorded per batch, crop JPEGs written to
/// `crops` for saved faces. Stops between photos once `cancelled` turns true.
pub fn extract_pending<S: std::hash::BuildHasher + Sync>(
    database: &Arc<Mutex<Connection>>,
    extractor: &dyn Extractor,
    processing: &str,
    skip: &HashSet<i64, S>,
    crops: Option<&Path>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> rusqlite::Result<Summary> {
    let model = extractor.model().to_string();
    let mut total = Summary::default();
    let mut cursor = 0i64;
    loop {
        if cancelled() {
            return Ok(total);
        }
        let now = super::face_review::now();
        let batch: Vec<Input> = {
            let conn = database.lock();
            pending(&conn, &model, processing, MAX_PER_BATCH + skip.len(), cursor, now)?
                .into_iter()
                .filter(|input| !skip.contains(&input.id))
                .take(MAX_PER_BATCH)
                .collect()
        };
        let Some(last) = batch.last().map(|input| input.id) else { return Ok(total) };
        let started = Instant::now();
        let mut by_path: BTreeMap<String, Vec<Input>> = BTreeMap::new();
        for input in &batch {
            by_path.entry(input.path.clone()).or_default().push(input.clone());
        }
        let files = by_path.len();
        let queue = Mutex::new(by_path.into_values().collect::<Vec<_>>());
        let results = Mutex::new(Vec::<FileResult>::new());
        std::thread::scope(|scope| {
            for _ in 0..EXTRACTION_THREADS.min(files.max(1)) {
                // As Swift's `extractOneFile` (`DispatchQueue.global(qos: .userInitiated)`):
                // Vision and the HEVC decoder schedule by the caller's QoS.
                scope.spawn(|| {
                    crate::util::qos::interactive(|| {
                        loop {
                            if cancelled() {
                                return;
                            }
                            let Some(rows) = queue.lock().pop() else { return };
                            let result = extract_one(rows, extractor);
                            results.lock().push(result);
                        }
                    });
                });
            }
        });
        let stopped = cancelled();
        let results = results.into_inner();
        let mut saved_crops = Vec::new();
        let (mut extracted, mut failed) = (0usize, 0usize);
        {
            let conn = database.lock();
            let tx = conn.unchecked_transaction()?;
            let mut saved: HashSet<i64> = HashSet::new();
            for file in &results {
                for (row, vector) in file.rows.iter().zip(&file.vectors) {
                    if let Some(vector) = vector
                        && persist(&tx, row, &vector.embedding, &model, processing, now)?
                    {
                        saved.insert(row.id);
                        if let Some(jpeg) = &vector.jpeg {
                            saved_crops.push((row.id, jpeg.clone()));
                        }
                    }
                }
            }
            if !stopped {
                let unavailable: HashSet<i64> = results.iter().filter(|f| f.unavailable).flat_map(|f| f.rows.iter().map(|r| r.id)).collect();
                for input in batch.iter().filter(|input| !saved.contains(&input.id)) {
                    let unreadable = unavailable.contains(&input.id) || identity(&input.path).is_none_or(|(size, modified, _)| size != input.size || Some(modified) != input.modified);
                    let reason = if unreadable { "source_unavailable_or_changed" } else { "no_embedding" };
                    record_failure(&tx, input, &model, processing, reason, now)?;
                    failed += 1;
                }
            }
            tx.commit()?;
            extracted += saved.len();
        }
        if let Some(dir) = crops {
            for (id, jpeg) in saved_crops {
                save_crop(dir, id, &jpeg);
            }
        }
        tracing::info!(pending = batch.len(), extracted, failed, files, seconds = started.elapsed().as_secs_f64(), "face_print_extract_done");
        total.pending += batch.len();
        total.extracted += extracted;
        total.failed += failed;
        total.files += files;
        if stopped || last <= cursor {
            return Ok(total);
        }
        cursor = last;
    }
}

fn save_crop(dir: &Path, id: i64, jpeg: &[u8]) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = dir.join(format!("{id}.jpg"));
    let temporary = dir.join(format!(".{id}.jpg.tmp"));
    if std::fs::write(&temporary, jpeg).is_ok() && std::fs::rename(&temporary, &path).is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

#[cfg(test)]
#[path = "face_extract_mac_tests.rs"]
mod tests;
