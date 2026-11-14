//! Durable face refresh backlog: a `faceRefresh` catalog job that re-embeds
//! faces whose stored vector is missing or from another model, processing or
//! file version, one model/processing namespace at a time. Mirror of the macOS
//! `FaceRefreshJob.swift`; keep states, SQL and messages in step.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};

use super::face_review::page;
use crate::ipc::sink::Sink;
use crate::ipc::{CatalogJob, CatalogRequest, CatalogResponse, EventPayload, FaceRefreshStatus, FaceReviewPage, IpcEvent, Wrap};

pub const JOB_ID: &str = "face-refresh";
pub const JOB_KIND: &str = "faceRefresh";
/// What this engine's own face pipeline produces (YuNet landmarks, 112 px alignment).
pub const PORTABLE_PROCESSING: &str = "rust-align112-v1";
/// The macOS namespaces (Vision landmarks and the Swift alignment), as
/// `FaceCacheProvenance.processingVersion` names them.
#[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
pub const MAC_PROCESSING: &str = "macos-landmark-overlap-v3-orientation";
#[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
pub const MAC_BBOX_PROCESSING: &str = "macos-bbox-crop-v3-orientation";
#[cfg_attr(all(target_os = "macos", feature = "apple-adapters"), allow(dead_code))]
const REPORT_ONLY: &str = "Updating faces isn't available on this computer yet. Your People and names are unchanged.";
const BATCH: usize = 64;
const NO_MODEL: &str = "Install the face recognition model in Settings, then update faces again.";
const MODEL_CHANGED: &str = "The face model changed. Start the face update again.";
const INTERRUPTED: &str = "Interrupted. Resume to continue; faces already updated are kept.";

static ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
struct Interrupted;

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Face refresh stopped")
    }
}

impl std::error::Error for Interrupted {}

fn now() -> f64 {
    super::face_review::now()
}

const OUTDATED: &str = "FROM face_prints fp JOIN files f ON f.id=fp.file_id LEFT JOIN catalog_revisions r ON r.file_id=fp.file_id \
     WHERE f.failed=0 AND (length(COALESCE(fp.arcface_embedding,X''))<>512 OR fp.embedding_model IS NULL OR fp.embedding_model<>?1 \
       OR fp.processing_version IS NULL OR fp.processing_version<>?2 OR fp.source_revision IS NULL OR r.revision IS NULL \
       OR fp.source_revision<>r.revision)";

/// The face space this platform keeps People in. The engine the Mac app hosts
/// keeps the Swift engine's namespace, so a Swift catalog shows no refresh
/// backlog; elsewhere it is the portable pipeline's.
pub(crate) fn processing() -> &'static str {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        if std::env::var("FILEID_FACE_ALIGN").as_deref() == Ok("0") { MAC_BBOX_PROCESSING } else { MAC_PROCESSING }
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        PORTABLE_PROCESSING
    }
}

/// Whether this engine can compute vectors in `processing()`: the portable
/// pipeline, or the app-hosted engine with the Vision adapters (`apple::faces`).
pub(crate) fn refresh_supported() -> bool {
    processing() == PORTABLE_PROCESSING || cfg!(all(target_os = "macos", feature = "apple-adapters"))
}

/// The installed SFace weights' identity, cached by path, size and mtime.
pub(crate) fn current_model() -> Option<String> {
    static CACHE: Mutex<Option<(PathBuf, u64, std::time::SystemTime, String)>> = Mutex::new(None);
    let path = crate::models::sface::default_weights_path().ok()?;
    let metadata = std::fs::metadata(&path).ok()?;
    let (size, modified) = (metadata.len(), metadata.modified().ok()?);
    if !(1..=128 * 1024 * 1024).contains(&size) {
        return None;
    }
    let mut cache = CACHE.lock();
    if let Some((cached, len, at, model)) = cache.as_ref()
        && *cached == path
        && *len == size
        && *at == modified
    {
        return Some(model.clone());
    }
    let digest = crate::util::content_hash::exact_file_sha256(&path, size).ok()?;
    let model = format!("sface128-sha256:{}", hex::encode(digest));
    *cache = Some((path, size, modified, model.clone()));
    Some(model)
}

pub(crate) fn outdated(conn: &Connection, model: &str, processing: &str) -> rusqlite::Result<i64> {
    conn.query_row(&format!("SELECT COUNT(*) {OUTDATED}"), params![model, processing], |row| row.get(0))
}

pub(crate) fn status(conn: &Connection, model: Option<&str>, processing: &str) -> rusqlite::Result<FaceRefreshStatus> {
    let job: Option<(String, f64, String)> = conn
        .query_row("SELECT state, progress, recipe_json FROM catalog_jobs WHERE id=?1 AND kind=?2", params![JOB_ID, JOB_KIND], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get::<_, Option<String>>(2)?.unwrap_or_default()))
        })
        .optional()?;
    let (model, processing) = match model {
        Some(model) => (model.to_string(), processing.to_string()),
        None => match job.as_ref().map(|(_, _, recipe)| recipe_space(recipe)) {
            Some(Some((model, processing))) => (model, processing),
            _ => (String::new(), processing.to_string()),
        },
    };
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    {
        let mut statement = conn.prepare(
            "SELECT state, COUNT(*) FROM face_refresh_backlog WHERE embedding_model=?1 AND processing_version=?2 GROUP BY state",
        )?;
        let rows = statement.query_map(params![model, processing], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
        for row in rows {
            let (state, count) = row?;
            counts.insert(state, count);
        }
    }
    let count = |state: &str| counts.get(state).copied().unwrap_or(0);
    Ok(FaceRefreshStatus {
        outdated: if model.is_empty() { 0 } else { outdated(conn, &model, &processing)? },
        total: counts.values().sum(),
        done: count("done"),
        pending: count("pending"),
        missing: count("missing"),
        offline: count("offline"),
        failed: count("failed"),
        job_id: job.as_ref().map(|_| JOB_ID.to_string()),
        state: job.as_ref().map(|(state, _, _)| state.clone()),
        progress: job.as_ref().map(|(_, progress, _)| progress.clamp(0.0, 1.0)),
        model,
        processing,
    })
}

pub(crate) fn status_page(conn: &Connection, request: &str) -> rusqlite::Result<FaceReviewPage> {
    let refresh = status(conn, current_model().as_deref(), processing())?;
    Ok(FaceReviewPage {
        total: refresh.pending,
        queue: Some("refresh".into()),
        refresh: Some(refresh),
        ..page(request, "list", "ok")
    })
}

fn recipe_space(recipe: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(recipe).ok()?;
    Some((value["model"].as_str()?.to_string(), value["processing"].as_str()?.to_string()))
}

pub fn recover(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE catalog_jobs SET state='paused',error=?1,updated_at=?2 WHERE kind=?3 AND state IN ('running','queued')",
        params![INTERRUPTED, now(), JOB_KIND],
    )?;
    Ok(())
}

pub(crate) fn is_job(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT kind FROM catalog_jobs WHERE id=?1", [id], |r| r.get::<_, String>(0))
        .optional()?
        .is_some_and(|kind| kind == JOB_KIND))
}

pub(crate) fn jobs(conn: &Connection) -> Result<Vec<CatalogJob>> {
    Ok(super::catalog::jobs(conn)?.into_iter().filter(|job| job.kind == JOB_KIND).collect())
}

/// Seeds the backlog for one namespace and queues the job.
pub(crate) fn seed(conn: &Connection, model: &str, processing: &str, at: f64) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO face_refresh_backlog(face_id,embedding_model,processing_version,state,attempts,reason,updated_at) \
             SELECT fp.id, ?1, ?2, 'pending', 0, NULL, ?3 {OUTDATED}"
        ),
        params![model, processing, at],
    )?;
    conn.execute(
        "UPDATE face_refresh_backlog SET state='pending',updated_at=?3 WHERE embedding_model=?1 AND processing_version=?2 \
         AND state IN ('missing','offline','failed')",
        params![model, processing, at],
    )?;
    conn.execute(
        &format!(
            "UPDATE face_refresh_backlog SET state='pending',updated_at=?3 WHERE embedding_model=?1 AND processing_version=?2 \
             AND state='done' AND face_id IN (SELECT fp.id {OUTDATED})"
        ),
        params![model, processing, at],
    )?;
    let recipe = serde_json::json!({"model": model, "processing": processing}).to_string();
    conn.execute(
        "INSERT OR IGNORE INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,priority,created_at,updated_at) VALUES(?1,?2,'[]',?3,'queued',0,?4,?4)",
        params![JOB_ID, JOB_KIND, recipe, at],
    )?;
    conn.execute(
        "UPDATE catalog_jobs SET state='queued',progress=0,error=NULL,checkpoint_json='{}',recipe_json=?1,updated_at=?2 \
         WHERE id=?3 AND kind=?4 AND state IN ('completed','cancelled','failed','paused')",
        params![recipe, at, JOB_ID, JOB_KIND],
    )?;
    conn.execute(
        "UPDATE catalog_jobs SET recipe_json=?1,updated_at=?2 WHERE id=?3 AND kind=?4 AND state='queued'",
        params![recipe, at, JOB_ID, JOB_KIND],
    )?;
    Ok(())
}

pub(crate) fn enqueue(conn: &Connection, request: &CatalogRequest, model: Option<&str>) -> Result<CatalogResponse> {
    let Some(model) = model else { bail!(NO_MODEL) };
    if !refresh_supported() {
        bail!(REPORT_ONLY);
    }
    seed(conn, model, processing(), now())?;
    let mut response = super::catalog::response(
        &request.request_id,
        "ok",
        Some("Updating faces for the current face model. People and names don't change.".into()),
    );
    response.jobs = jobs(conn)?;
    Ok(response)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Available,
    Missing,
    Offline,
}

/// Missing: the photo is gone but its library location is there. Offline: the
/// location itself (or the volume or drive it is on) is unreachable.
pub(crate) fn classify(path: &Path, roots: &[PathBuf]) -> Source {
    if path.exists() {
        return Source::Available;
    }
    if let Some(root) = roots.iter().filter(|root| path.starts_with(root)).max_by_key(|root| root.as_os_str().len()) {
        return if root.exists() { Source::Missing } else { Source::Offline };
    }
    let mut components = path.components();
    if let Some(std::path::Component::Prefix(prefix)) = components.next() {
        let drive = PathBuf::from(prefix.as_os_str()).join(std::path::MAIN_SEPARATOR_STR);
        return if drive.exists() { Source::Missing } else { Source::Offline };
    }
    let parts: Vec<_> = path.components().take(3).collect();
    if parts.len() == 3 && parts[1].as_os_str() == "Volumes" {
        let volume: PathBuf = parts.iter().collect();
        if !volume.exists() {
            return Source::Offline;
        }
    }
    Source::Missing
}

pub(crate) fn roots(conn: &Connection) -> rusqlite::Result<Vec<PathBuf>> {
    let mut statement = conn.prepare("SELECT path FROM library_roots")?;
    statement.query_map([], |row| row.get::<_, String>(0).map(PathBuf::from))?.collect()
}

/// Re-embeds the faces of one photo. One result per box, in order.
pub(crate) trait Backend: Send {
    fn model(&self) -> &str;
    fn embed(&mut self, path: &Path, boxes: &[String]) -> Vec<std::result::Result<Vec<f32>, &'static str>>;
    /// Crop JPEGs (`face_crops/<id>.jpg`) for the boxes of the last `embed`, in
    /// order, for backends that make them.
    fn take_crops(&mut self) -> Vec<Option<Vec<u8>>> {
        Vec::new()
    }
}

pub(crate) struct Pending {
    pub face: i64,
    pub file: i64,
    pub bbox: String,
    pub path: String,
    pub size: i64,
    pub modified: Option<f64>,
}

pub(crate) fn next_batch(conn: &Connection, model: &str, processing: &str, limit: usize) -> rusqlite::Result<Vec<Pending>> {
    let mut statement = conn.prepare(
        "SELECT b.face_id, fp.file_id, fp.bbox, f.path_text, f.size_bytes, f.modified_at FROM face_refresh_backlog b \
         JOIN face_prints fp ON fp.id=b.face_id JOIN files f ON f.id=fp.file_id \
         WHERE b.embedding_model=?1 AND b.processing_version=?2 AND b.state='pending' ORDER BY b.face_id LIMIT ?3",
    )?;
    statement
        .query_map(params![model, processing, limit as i64], |row| {
            Ok(Pending {
                face: row.get(0)?,
                file: row.get(1)?,
                bbox: row.get(2)?,
                path: row.get(3)?,
                size: row.get(4)?,
                modified: row.get(5)?,
            })
        })?
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    Embedded(Vec<f32>),
    Missing,
    Offline,
    Failed(&'static str),
}

fn stat_matches(path: &Path, size: i64, modified: Option<f64>) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else { return false };
    let actual = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs_f64());
    i64::try_from(metadata.len()).is_ok_and(|len| len == size)
        && match (actual, modified) {
            (Some(actual), Some(stored)) => (actual - stored).abs() < 0.001,
            _ => false,
        }
}

/// Runs the backend outside the database lock, one photo at a time.
#[cfg(test)]
pub(crate) fn embed_batch(batch: &[Pending], roots: &[PathBuf], backend: &mut dyn Backend) -> Vec<(i64, Outcome)> {
    embed_batch_with_crops(batch, roots, backend).0
}

/// [`embed_batch`] plus the crop JPEGs the backend made, by face.
pub(crate) fn embed_batch_with_crops(batch: &[Pending], roots: &[PathBuf], backend: &mut dyn Backend) -> (Vec<(i64, Outcome)>, Vec<(i64, Vec<u8>)>) {
    let mut crops = Vec::new();
    let mut by_path: BTreeMap<&str, Vec<&Pending>> = BTreeMap::new();
    for item in batch {
        by_path.entry(item.path.as_str()).or_default().push(item);
    }
    let mut out = Vec::with_capacity(batch.len());
    for (path, items) in by_path {
        let path = Path::new(path);
        let outcome = match classify(path, roots) {
            Source::Missing => Some(Outcome::Missing),
            Source::Offline => Some(Outcome::Offline),
            Source::Available if !items.iter().all(|item| stat_matches(path, item.size, item.modified)) => {
                Some(Outcome::Failed("source_changed"))
            }
            Source::Available => None,
        };
        if let Some(outcome) = outcome {
            out.extend(items.iter().map(|item| (item.face, outcome.clone())));
            continue;
        }
        let boxes: Vec<String> = items.iter().map(|item| item.bbox.clone()).collect();
        let results = backend.embed(path, &boxes);
        for (item, jpeg) in items.iter().zip(backend.take_crops()) {
            if let Some(jpeg) = jpeg {
                crops.push((item.face, jpeg));
            }
        }
        for (index, item) in items.iter().enumerate() {
            let outcome = match results.get(index) {
                Some(Ok(vector)) => Outcome::Embedded(vector.clone()),
                Some(Err(reason)) => Outcome::Failed(reason),
                None => Outcome::Failed("no_embedding"),
            };
            out.push((item.face, outcome));
        }
    }
    (out, crops)
}

fn valid(vector: &[f32]) -> bool {
    let norm = vector.iter().map(|value| f64::from(*value).powi(2)).sum::<f64>();
    vector.len() == 128 && vector.iter().all(|value| value.is_finite()) && (0.95..=1.05).contains(&norm)
}

/// Stores one batch's outcomes. A vector is saved only while the face, its box
/// and its photo's catalog size and time are unchanged.
pub(crate) fn persist(conn: &Connection, model: &str, processing: &str, batch: &[Pending], outcomes: &[(i64, Outcome)], at: f64) -> rusqlite::Result<Vec<i64>> {
    let tx = conn.unchecked_transaction()?;
    let mut saved = Vec::new();
    for (face, outcome) in outcomes {
        let Some(item) = batch.iter().find(|item| item.face == *face) else { continue };
        let (state, reason) = match outcome {
            // The Mac face space saves through `FaceAnalysisCache.persist`, as
            // the Swift engine's `FaceRefreshJob` does.
            Outcome::Embedded(vector) if processing.starts_with(super::face_clustering_mac::MAC_PREFIX) => {
                let input = super::face_extract_mac::Input {
                    id: item.face,
                    file_id: item.file,
                    bbox: item.bbox.clone(),
                    path: item.path.clone(),
                    size: item.size,
                    modified: item.modified,
                };
                if processing == self::processing() && super::face_extract_mac::persist(&tx, &input, vector, model, processing, at)? {
                    saved.push(item.face);
                    ("done", None)
                } else {
                    ("failed", Some("source_changed"))
                }
            }
            Outcome::Embedded(vector) if valid(vector) => {
                let revision: Option<String> = tx
                    .query_row(
                        "SELECT r.revision FROM face_prints fp JOIN files f ON f.id=fp.file_id JOIN catalog_revisions r ON r.file_id=fp.file_id \
                         WHERE fp.id=?1 AND fp.file_id=?2 AND fp.bbox=?3 AND f.size_bytes=?4 AND f.modified_at IS ?5",
                        params![item.face, item.file, item.bbox, item.size, item.modified],
                        |row| row.get(0),
                    )
                    .optional()?;
                match revision {
                    Some(revision) => {
                        let blob: Vec<u8> = vector.iter().flat_map(|value| value.to_le_bytes()).collect();
                        tx.execute(
                            "UPDATE face_prints SET arcface_embedding=?1,embedding_model=?2,processing_version=?3,source_revision=?4 WHERE id=?5 AND file_id=?6 AND bbox=?7",
                            params![blob, model, processing, revision, item.face, item.file, item.bbox],
                        )?;
                        saved.push(item.face);
                        ("done", None)
                    }
                    None => ("failed", Some("source_changed")),
                }
            }
            Outcome::Embedded(_) => ("failed", Some("invalid_vector")),
            Outcome::Missing => ("missing", None),
            Outcome::Offline => ("offline", None),
            Outcome::Failed(reason) => ("failed", Some(*reason)),
        };
        tx.execute(
            "UPDATE face_refresh_backlog SET state=?1,reason=?2,attempts=attempts+CASE WHEN ?1='failed' THEN 1 ELSE 0 END,updated_at=?3 \
             WHERE face_id=?4 AND embedding_model=?5 AND processing_version=?6 AND state='pending'",
            params![state, reason, at, item.face, model, processing],
        )?;
    }
    let (total, pending): (i64, i64) = tx.query_row(
        "SELECT COUNT(*), COALESCE(SUM(state='pending'),0) FROM face_refresh_backlog WHERE embedding_model=?1 AND processing_version=?2",
        params![model, processing],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let progress = if total > 0 { (total - pending) as f64 / total as f64 } else { 1.0 };
    tx.execute("UPDATE catalog_jobs SET progress=?1,updated_at=?2 WHERE id=?3 AND state='running'", params![progress, at, JOB_ID])?;
    tx.commit()?;
    Ok(saved)
}

fn require_running(conn: &Connection) -> Result<(String, String)> {
    let row: Option<(String, Option<String>)> = conn
        .query_row("SELECT state, recipe_json FROM catalog_jobs WHERE id=?1", [JOB_ID], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    match row {
        Some((state, recipe)) if state == "running" => {
            recipe_space(&recipe.unwrap_or_default()).ok_or_else(|| anyhow::anyhow!("The face update has no model to refresh to."))
        }
        _ => Err(Interrupted.into()),
    }
}

/// Drains the backlog synchronously with `backend`.
#[cfg(test)]
pub(crate) fn drain(conn: &Connection, backend: &mut dyn Backend) -> Result<()> {
    loop {
        let (model, processing) = require_running(conn)?;
        if backend.model() != model {
            bail!(MODEL_CHANGED);
        }
        let batch = next_batch(conn, &model, &processing, BATCH)?;
        if batch.is_empty() {
            return Ok(());
        }
        let outcomes = embed_batch(&batch, &roots(conn)?, backend);
        persist(conn, &model, &processing, &batch, &outcomes, now())?;
    }
}

pub(crate) fn schedule(sink: Sink, database: Arc<Mutex<Connection>>) {
    if ACTIVE.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async move {
        run(&sink, &database).await;
        ACTIVE.store(false, Ordering::Release);
        let queued = super::timeline::with_db(&database, |conn| {
            Ok(conn.query_row("SELECT state FROM catalog_jobs WHERE id=?1", [JOB_ID], |r| r.get::<_, String>(0)).optional()?)
        })
        .await;
        if matches!(queued, Ok(Some(state)) if state == "queued") {
            schedule(sink, database);
        }
    });
}

async fn publish(sink: &Sink, database: &Arc<Mutex<Connection>>) {
    if let Ok(jobs) = super::timeline::with_db(database, |conn| jobs(conn)).await {
        let mut response = super::catalog::response(JOB_ID, "ok", None);
        response.jobs = jobs;
        sink.send(IpcEvent::now(EventPayload::CatalogResponse(Wrap::new(response)))).await;
    }
}

async fn run(sink: &Sink, database: &Arc<Mutex<Connection>>) {
    let started = super::timeline::with_db(database, |conn| {
        let changed = conn.execute(
            "UPDATE catalog_jobs SET state='running',error=NULL,updated_at=?1 WHERE id=?2 AND kind=?3 AND state='queued'",
            params![now(), JOB_ID, JOB_KIND],
        )?;
        Ok(changed == 1)
    })
    .await;
    if !matches!(started, Ok(true)) {
        return;
    }
    publish(sink, database).await;
    let outcome = async {
        let mut backend: Box<dyn Backend> =
            tokio::task::spawn_blocking(load_backend)
                .await
                .map_err(|error| anyhow::anyhow!(error))??;
        let mut published = Instant::now();
        loop {
            let (batch, model, processing, roots) = super::timeline::with_db(database, |conn| {
                let (model, processing) = require_running(conn)?;
                let batch = next_batch(conn, &model, &processing, BATCH)?;
                Ok((batch, model, processing, roots(conn)?))
            })
            .await?;
            if backend.model() != model {
                bail!(MODEL_CHANGED);
            }
            if batch.is_empty() {
                return Ok(());
            }
            let (outcomes, crops, returned, batch) = tokio::task::spawn_blocking(move || {
                let (outcomes, crops) = embed_batch_with_crops(&batch, &roots, backend.as_mut());
                (outcomes, crops, backend, batch)
            })
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
            backend = returned;
            let saved = super::timeline::with_db(database, move |conn| Ok(persist(conn, &model, &processing, &batch, &outcomes, now())?)).await?;
            save_crops(&crops, &saved);
            if published.elapsed() >= Duration::from_millis(500) {
                publish(sink, database).await;
                published = Instant::now();
            }
        }
    }
    .await;
    let failure = match outcome {
        Ok(()) => None,
        Err(error) if error.is::<Interrupted>() => return publish(sink, database).await,
        Err(error) => Some(format!("{error:#}").chars().take(2_000).collect::<String>()),
    };
    let _ = super::timeline::with_db(database, move |conn| {
        match failure {
            None => conn.execute(
                "UPDATE catalog_jobs SET state='completed',progress=1,error=NULL,updated_at=?1 WHERE id=?2 AND state='running'",
                params![now(), JOB_ID],
            )?,
            Some(error) => conn.execute(
                "UPDATE catalog_jobs SET state='failed',error=?1,updated_at=?2 WHERE id=?3 AND state='running'",
                params![error, now(), JOB_ID],
            )?,
        };
        Ok(())
    })
    .await;
    publish(sink, database).await;
}

/// Writes the crop JPEGs of the faces whose vectors were saved.
fn save_crops(crops: &[(i64, Vec<u8>)], saved: &[i64]) {
    if crops.is_empty() {
        return;
    }
    let Ok(dir) = crate::paths::faces_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (face, jpeg) in crops.iter().filter(|(face, _)| saved.contains(face)) {
        let temporary = dir.join(format!(".{face}.jpg.tmp"));
        if std::fs::write(&temporary, jpeg).is_ok() && std::fs::rename(&temporary, dir.join(format!("{face}.jpg"))).is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
    }
}

/// Pixel box, top-left origin, on the upright decoded image.
#[cfg_attr(all(target_os = "macos", feature = "apple-adapters"), allow(dead_code))]
pub(crate) fn pixel_box(bbox: &str, width: u32, height: u32) -> std::result::Result<[f32; 4], &'static str> {
    let text = bbox.trim();
    if text.starts_with('{') {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|_| "bad_box")?;
        if value["orientation"].as_i64().is_some_and(|orientation| orientation != 1) {
            return Err("rotated_box");
        }
        let source = (value["sourceWidth"].as_u64(), value["sourceHeight"].as_u64());
        if let (Some(w), Some(h)) = source
            && (w != u64::from(width) || h != u64::from(height))
        {
            return Err("dimensions_changed");
        }
        let get = |key: &str| value[key].as_f64().map(|v| v as f32).ok_or("bad_box");
        return Ok([get("x")?, get("y")?, get("w")?, get("h")?]);
    }
    let parts: Vec<f32> = text.split(',').filter_map(|part| part.trim().parse::<f32>().ok()).collect();
    let [x, y, w, h] = parts[..] else { return Err("bad_box") };
    let (width, height) = (width as f32, height as f32);
    Ok([x * width, (1.0 - y - h) * height, w * width, h * height])
}

#[cfg_attr(all(target_os = "macos", feature = "apple-adapters"), allow(dead_code))]
fn overlap(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = (a[0] + a[2]).min(b[0] + b[2]);
    let y2 = (a[1] + a[3]).min(b[1] + b[3]);
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

#[cfg_attr(all(target_os = "macos", feature = "apple-adapters"), allow(dead_code))]
struct OnnxBackend {
    yunet: crate::models::yunet::YuNet,
    sface: crate::models::sface::SFace,
}

/// The app-hosted engine's backend: the Mac pipeline in `apple::faces`.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
struct MacBackend {
    sface: Arc<crate::apple::faces::MacSFace>,
    crops: Vec<Option<Vec<u8>>>,
}

#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
impl Backend for MacBackend {
    fn model(&self) -> &str {
        self.sface.model_version()
    }

    fn embed(&mut self, path: &Path, boxes: &[String]) -> Vec<std::result::Result<Vec<f32>, &'static str>> {
        let (faces, _) = crate::apple::faces::extract_file(path, boxes, &self.sface);
        self.crops = faces.iter().map(|face| face.as_ref().and_then(|face| face.jpeg.clone())).collect();
        faces.into_iter().map(|face| face.map(|face| face.embedding).ok_or("no_embedding")).collect()
    }

    fn take_crops(&mut self) -> Vec<Option<Vec<u8>>> {
        std::mem::take(&mut self.crops)
    }
}

fn load_backend() -> Result<Box<dyn Backend>> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        match crate::apple::faces::shared_sface() {
            None => bail!(NO_MODEL),
            Some(Err(error)) => Err(error.context("The face recognition model is installed but failed to load.")),
            Some(Ok(sface)) => Ok(Box::new(MacBackend { sface, crops: Vec::new() })),
        }
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        OnnxBackend::load().map(|backend| Box::new(backend) as Box<dyn Backend>)
    }
}

impl OnnxBackend {
    #[cfg_attr(all(target_os = "macos", feature = "apple-adapters"), allow(dead_code))]
    fn load() -> Result<Self> {
        if !refresh_supported() {
            if current_model().is_none() {
                bail!(NO_MODEL);
            }
            bail!(REPORT_ONLY);
        }
        let yunet = crate::models::yunet::default_weights_path().and_then(crate::models::yunet::YuNet::load);
        let sface = crate::models::sface::default_weights_path().and_then(crate::models::sface::SFace::load);
        match (yunet, sface) {
            (Ok(yunet), Ok(sface)) => Ok(Self { yunet, sface }),
            _ => bail!(NO_MODEL),
        }
    }
}

impl Backend for OnnxBackend {
    fn model(&self) -> &str {
        self.sface.model_version()
    }

    fn embed(&mut self, path: &Path, boxes: &[String]) -> Vec<std::result::Result<Vec<f32>, &'static str>> {
        let Ok((rgb, width, height)) = crate::pipeline::tagging::decode_image_sync(path, None) else {
            return boxes.iter().map(|_| Err("decode_failed")).collect();
        };
        let detections = self.yunet.detect(&rgb, width, height).unwrap_or_default();
        boxes
            .iter()
            .map(|bbox| {
                let target = pixel_box(bbox, width, height)?;
                let matched = detections
                    .iter()
                    .map(|detection| {
                        let found = [detection.bbox[0], detection.bbox[1], detection.bbox[2] - detection.bbox[0], detection.bbox[3] - detection.bbox[1]];
                        (overlap(&target, &found), detection)
                    })
                    .filter(|(score, _)| *score >= 0.3)
                    .max_by(|a, b| a.0.total_cmp(&b.0));
                let crop = matched
                    .and_then(|(_, detection)| crate::models::face_align::align_112(&rgb, width, height, &detection.landmarks))
                    .or_else(|| crate::pipeline::tagging::crop_and_resize_face(&rgb, width, height, &target))
                    .ok_or("no_crop")?;
                self.sface.embed(&crop).map_err(|_| "no_embedding")
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "face_refresh_tests.rs"]
mod tests;
