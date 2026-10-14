//! Portable timeline worker: the Linux/Windows counterpart of the macOS
//! `TimelineAnalysis` actor. Decodes with the vetted FFmpeg runtime, captions
//! sampled frames and moment windows with the resident VLM, and persists
//! evidence, coverage and draft chapters to the shared catalog tables.
//! Unexamined footage stays explicitly incomplete; nothing here claims an
//! event did not happen.

use super::video_export;
use crate::ipc::sink::Sink;
use crate::ipc::{CatalogChapter, CatalogRequest, CatalogResponse, EventPayload, IpcEvent, Wrap};
use crate::models::resident_service::{ResidentLease, ResidentService};
use crate::pipeline::timeline_analysis as analysis;
use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

pub(crate) const JOB_KIND: &str = "timelineSample";
const INTERVAL_SECONDS: f64 = 10.0;
const SIGNAL_CHUNK_SECONDS: f64 = 60.0;
const FRAME_SIDE: u32 = 768;
const CAPTION_TOKENS: u32 = 80;
const MOMENT_TOKENS: u32 = 400;
const MAX_FILES: usize = 1_000;
const VLM_PREFERENCE: [&str; 5] = [
    "qwen3_vl_4b",
    "qwen3_vl_8b",
    "qwen2_5_vl_7b",
    "gemma_3_4b",
    "mistral_small_3_2",
];

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Recipe {
    model_kind: String,
    model_version: String,
    interval_seconds: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeline_mode: Option<String>,
}

#[derive(Debug)]
struct Interrupted;
impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Timeline job interrupted")
    }
}
impl std::error::Error for Interrupted {}

/// The visual model the worker consults. The resident VLM lease is the real
/// implementation; tests substitute a scripted model to exercise the worker.
pub(crate) trait FrameModel: Sync {
    fn caption(
        &self,
        image: &Path,
        prompt: &str,
        tokens: u32,
        cancel: Arc<AtomicBool>,
    ) -> impl Future<Output = Result<String>> + Send;
    fn sequence(
        &self,
        images: &[&Path],
        prompt: &str,
        tokens: u32,
        cancel: Arc<AtomicBool>,
    ) -> impl Future<Output = Result<String>> + Send;
}

impl FrameModel for ResidentLease {
    fn caption(
        &self,
        image: &Path,
        prompt: &str,
        tokens: u32,
        cancel: Arc<AtomicBool>,
    ) -> impl Future<Output = Result<String>> + Send {
        self.complete_with_cancel(image, prompt, tokens, cancel)
    }
    fn sequence(
        &self,
        images: &[&Path],
        prompt: &str,
        tokens: u32,
        cancel: Arc<AtomicBool>,
    ) -> impl Future<Output = Result<String>> + Send {
        self.complete_sequence_with_cancel(images, prompt, tokens, cancel)
    }
}

fn source_changed() -> anyhow::Error {
    anyhow!("The source changed during analysis. Rescan it before continuing.")
}

fn unreadable() -> anyhow::Error {
    anyhow!("A video frame could not be read or analyzed. Partial coverage remains explicitly incomplete.")
}

#[derive(Default)]
struct Worker {
    scheduled: Mutex<HashSet<String>>,
    running: Mutex<Option<(String, Arc<AtomicBool>)>>,
    gate: tokio::sync::Mutex<()>,
}

fn worker() -> &'static Worker {
    static WORKER: OnceLock<Worker> = OnceLock::new();
    WORKER.get_or_init(Worker::default)
}

fn now() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}

fn frames_root() -> PathBuf {
    std::env::temp_dir().join("fileid-timeline-frames")
}

pub(crate) fn installed_model() -> Option<&'static str> {
    VLM_PREFERENCE
        .into_iter()
        .find(|kind| crate::models::vlm::find_weights(kind).is_some())
}

/// Marks interrupted timeline jobs paused and removes abandoned frames.
/// Called once when the catalog database opens.
pub fn recover(conn: &Connection) -> Result<()> {
    let _ = std::fs::remove_dir_all(frames_root());
    conn.execute(
        "UPDATE catalog_jobs SET state='paused',error='Interrupted. Resume to continue from the last completed file.',updated_at=?1 WHERE kind=?2 AND state IN ('running','queued')",
        params![now(), JOB_KIND],
    )?;
    Ok(())
}

pub(crate) fn is_timeline_job(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT kind FROM catalog_jobs WHERE id=?1", [id], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .is_some_and(|kind| kind == JOB_KIND))
}

/// Validates and records a new timeline job; returns its id for scheduling.
pub(crate) fn enqueue(
    conn: &mut Connection,
    request: &CatalogRequest,
    model: Option<&str>,
    ffmpeg: bool,
) -> Result<(String, CatalogResponse)> {
    let ids = request.file_ids.as_deref().unwrap_or_default();
    if request.request_id.is_empty()
        || request.request_id.chars().count() > 200
        || ids.is_empty()
        || ids.len() > MAX_FILES
    {
        bail!("Invalid catalog request")
    }
    let mode = request.timeline_mode.as_deref();
    if mode.is_some_and(|mode| !["sampled", "moments"].contains(&mode)) {
        bail!("Invalid timeline analysis mode")
    }
    if !ffmpeg {
        bail!("Timeline analysis needs the vetted local FFmpeg runtime, which is not installed with this build.")
    }
    let Some(model) = model else {
        bail!("Install a visual model in Deep Analyze before starting timeline analysis. No model will be downloaded automatically.")
    };
    let mut unique = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let tx = conn.transaction()?;
    for id in &unique {
        let kind: Option<String> = tx
            .query_row("SELECT kind FROM files WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        if kind.as_deref() != Some("video") {
            bail!("Timeline analysis accepts catalogued videos only")
        }
    }
    let job_id = uuid::Uuid::new_v4().to_string();
    let recipe = Recipe {
        model_kind: model.to_owned(),
        model_version: format!("timeline-frame-v2/{model}@portable"),
        interval_seconds: INTERVAL_SECONDS,
        timeline_mode: mode.map(str::to_owned),
    };
    let stamp = now();
    tx.execute(
        "INSERT INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,created_at,updated_at) VALUES(?1,?2,?3,?4,'queued',?5,?5)",
        params![job_id, JOB_KIND, serde_json::to_string(&unique)?, serde_json::to_string(&recipe)?, stamp],
    )?;
    tx.commit()?;
    let message = if mode == Some("moments") {
        "Queued significant-moment drafts from overlapping frame sequences. Review every proposal; sampled frames can still miss actions or hidden outcomes. On-device speech transcription is not yet available on this platform."
    } else {
        "Visual sampling uses low-resolution change signals to select a representative moment in each ten-second window, along with a starting frame. Fast events can still be missed. On-device speech transcription is not yet available on this platform."
    };
    let mut response = super::catalog::response(&request.request_id, "ok", Some(message.into()));
    response.jobs = super::catalog::jobs(conn)?;
    Ok((job_id, response))
}

/// Stops the running job's decoder/model call promptly after pause or cancel.
pub(crate) fn interrupt(job_id: &str) {
    if let Some((id, cancel)) = worker().running.lock().as_ref() {
        if id == job_id {
            cancel.store(true, Ordering::Release);
        }
    }
}

pub(crate) fn schedule(job_id: String, sink: Sink, database: Arc<Mutex<Connection>>) {
    if !worker().scheduled.lock().insert(job_id.clone()) {
        return;
    }
    tokio::spawn(async move {
        let _turn = worker().gate.lock().await;
        let requeue = run(&job_id, &sink, &database).await;
        worker().scheduled.lock().remove(&job_id);
        if requeue {
            schedule(job_id, sink, database);
        }
    });
}

async fn publish(job_id: &str, sink: &Sink, database: &Arc<Mutex<Connection>>) {
    let jobs = super::catalog::jobs(&database.lock());
    if let Ok(jobs) = jobs {
        let mut response = super::catalog::response(job_id, "ok", None);
        response.jobs = jobs;
        sink.send(IpcEvent::now(EventPayload::CatalogResponse(Wrap::new(response))))
            .await;
    }
}

struct Job {
    ids: Vec<i64>,
    recipe: Recipe,
    index: usize,
}

fn start(conn: &Connection, id: &str) -> Result<Option<Job>> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT file_ids_json,recipe_json,checkpoint_json FROM catalog_jobs WHERE id=?1 AND state='queued' AND kind=?2",
            params![id, JOB_KIND],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default())),
        )
        .optional()?;
    let Some((ids, recipe, checkpoint)) = row else {
        return Ok(None);
    };
    conn.execute(
        "UPDATE catalog_jobs SET state='running',error=NULL,updated_at=?1 WHERE id=?2",
        params![now(), id],
    )?;
    let recipe: Recipe = serde_json::from_str(&recipe)?;
    if recipe
        .timeline_mode
        .as_deref()
        .is_some_and(|mode| !["sampled", "moments"].contains(&mode))
    {
        bail!("Invalid timeline analysis mode")
    }
    Ok(Some(Job {
        ids: serde_json::from_str(&ids)?,
        recipe,
        index: checkpoint.parse().unwrap_or(0),
    }))
}

/// Runs one job to a terminal or paused state. Returns true when the job was
/// re-queued (resumed) while it was finishing and must run again.
async fn run(id: &str, sink: &Sink, database: &Arc<Mutex<Connection>>) -> bool {
    let cancel = Arc::new(AtomicBool::new(false));
    *worker().running.lock() = Some((id.to_owned(), cancel.clone()));
    let frames = frames_root().join(id);
    let outcome = async {
        let Some(job) = start(&database.lock(), id)? else {
            return Ok(());
        };
        publish(id, sink, database).await;
        let lease = ResidentService::shared()
            .acquire(&job.recipe.model_kind, cancel.clone())
            .await?;
        let result = process(id, &job, &lease, &frames, &cancel, sink, database).await;
        let cleanup = lease.finish(result.is_ok()).await;
        result.and(cleanup)
    }
    .await;
    let _ = std::fs::remove_dir_all(&frames);
    *worker().running.lock() = None;
    {
        let conn = database.lock();
        match outcome {
            Ok(()) => {
                let _ = conn.execute("UPDATE catalog_jobs SET state='completed',progress=1,updated_at=?1 WHERE id=?2 AND state='running'", params![now(), id]);
            }
            Err(error) if error.is::<Interrupted>() || cancel.load(Ordering::Acquire) => {}
            Err(error) => {
                let _ = conn.execute(
                    "UPDATE catalog_jobs SET state='failed',error=?1,updated_at=?2 WHERE id=?3 AND state='running'",
                    params![format!("{error:#}").chars().take(2_000).collect::<String>(), now(), id],
                );
            }
        }
    }
    publish(id, sink, database).await;
    database
        .lock()
        .query_row("SELECT state FROM catalog_jobs WHERE id=?1", [id], |r| {
            r.get::<_, String>(0)
        })
        .is_ok_and(|state| state == "queued")
}

fn require_running(conn: &Connection, id: &str) -> Result<()> {
    let state: Option<String> = conn
        .query_row("SELECT state FROM catalog_jobs WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    if state.as_deref() != Some("running") {
        return Err(Interrupted.into());
    }
    Ok(())
}

pub(crate) fn revision(conn: &Connection, file_id: i64) -> Result<String> {
    let (size, modified): (i64, Option<f64>) =
        conn.query_row("SELECT size_bytes,modified_at FROM files WHERE id=?1", [file_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    Ok(format!(
        "{}:{}",
        size,
        modified
            .map(|m| m.to_bits().to_string())
            .unwrap_or_else(|| "unknown".into())
    ))
}

/// True when the file on disk still matches the catalogued size and mtime.
fn on_disk_matches(path: &Path, size: i64, modified: Option<f64>) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let disk = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());
    metadata.len() as i64 == size && matches!((modified, disk), (Some(a), Some(b)) if (a - b).abs() < 0.000_001)
}

struct Source {
    path: PathBuf,
    size: i64,
    modified: Option<f64>,
    revision: String,
}

impl Source {
    fn load(conn: &Connection, file_id: i64) -> Result<Self> {
        let (path, size, modified): (String, i64, Option<f64>) = conn
            .query_row(
                "SELECT path_text,size_bytes,modified_at FROM files WHERE id=?1",
                [file_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .context("The video is no longer in the catalog")?;
        Ok(Self {
            path: PathBuf::from(path),
            size,
            modified,
            revision: revision(conn, file_id)?,
        })
    }

    fn verify(&self, conn: &Connection, file_id: i64) -> Result<()> {
        if !on_disk_matches(&self.path, self.size, self.modified) || revision(conn, file_id)? != self.revision {
            return Err(source_changed());
        }
        Ok(())
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| anyhow!(error))?
}

fn interrupted_or(cancel: &AtomicBool, error: anyhow::Error) -> anyhow::Error {
    if cancel.load(Ordering::Acquire) {
        Interrupted.into()
    } else {
        error
    }
}

fn upsert_coverage(
    conn: &Connection,
    id: &str,
    file_id: i64,
    start: f64,
    end: f64,
    status: &str,
    revision: &str,
    model: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![id, file_id, start, end, status, revision, model],
    )?;
    Ok(())
}

pub(crate) fn upsert_chapter(conn: &Connection, chapter: &CatalogChapter) -> Result<()> {
    conn.execute(
        "INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited,stale) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) \
         ON CONFLICT(id) DO UPDATE SET start_seconds=excluded.start_seconds,end_seconds=excluded.end_seconds,title=excluded.title,summary=excluded.summary,source_revision=excluded.source_revision,model_version=excluded.model_version,confidence=excluded.confidence,user_edited=excluded.user_edited,stale=excluded.stale",
        params![
            chapter.id, chapter.file_id, chapter.start_seconds, chapter.end_seconds, chapter.title.chars().take(200).collect::<String>(),
            chapter.summary.chars().take(4_000).collect::<String>(), chapter.source_revision, chapter.model_version, chapter.confidence,
            chapter.user_edited, chapter.stale
        ],
    )?;
    Ok(())
}

fn user_edited(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT user_edited FROM catalog_chapters WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or(false))
}

fn set_progress(conn: &Connection, id: &str, progress: f64) -> Result<()> {
    conn.execute(
        "UPDATE catalog_jobs SET progress=MAX(progress,?1),updated_at=?2 WHERE id=?3 AND state='running'",
        params![progress.clamp(0.0, 1.0), now(), id],
    )?;
    Ok(())
}

fn write_jpeg(path: &Path, rgb: Vec<u8>, width: u32, height: u32) -> Result<()> {
    let image = image::RgbImage::from_raw(width, height, rgb).context("Invalid decoded frame")?;
    image.save_with_format(path, image::ImageFormat::Jpeg)?;
    Ok(())
}

async fn decode_jpeg(
    source: &Path,
    seconds: f64,
    size: (u32, u32),
    output: PathBuf,
    cancel: &Arc<AtomicBool>,
) -> Result<PathBuf> {
    let (source, flag) = (source.to_owned(), cancel.clone());
    blocking(move || {
        let rgb = video_export::frame_rgb(&source, seconds, size.0, size.1, &flag)?;
        write_jpeg(&output, rgb, size.0, size.1)?;
        Ok(output)
    })
    .await
    .map_err(|error| interrupted_or(cancel, error))
}

#[allow(clippy::too_many_arguments)]
async fn process<M: FrameModel>(
    id: &str,
    job: &Job,
    model: &M,
    frames: &Path,
    cancel: &Arc<AtomicBool>,
    sink: &Sink,
    database: &Arc<Mutex<Connection>>,
) -> Result<()> {
    std::fs::create_dir_all(frames)?;
    let total = job.ids.len().max(1);
    for (index, &file_id) in job.ids.iter().enumerate().skip(job.index) {
        require_running(&database.lock(), id)?;
        let source = Source::load(&database.lock(), file_id)?;
        source.verify(&database.lock(), file_id)?;
        let probe = {
            let (path, flag) = (source.path.clone(), cancel.clone());
            blocking(move || video_export::analysis_probe(&path, &flag))
                .await
                .map_err(|error| interrupted_or(cancel, error))?
        };
        let duration = probe.duration;
        let version = job.recipe.model_version.as_str();
        upsert_coverage(
            &database.lock(),
            &format!("{id}-{file_id}-coverage"),
            file_id,
            0.0,
            duration,
            "incomplete",
            &source.revision,
            version,
        )?;

        let signals = scan_signals(id, file_id, &source, duration, cancel, database).await?;
        let size = video_export::fitted_size(probe.width, probe.height, FRAME_SIDE);
        let caption_prompt = crate::models::vlm::caption_prompt_with_faces(&[]);
        for seconds in analysis::sample_times(duration, job.recipe.interval_seconds, &signals) {
            require_running(&database.lock(), id)?;
            let evidence = format!(
                "frame:{file_id}:{}:{version}:{}",
                source.revision,
                analysis::millis(seconds)
            );
            let exists: bool = database.lock().query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_passages WHERE id=?1 AND source_revision=?2 AND stale=0)",
                params![evidence, source.revision],
                |r| r.get(0),
            )?;
            if exists {
                continue;
            }
            let frame = decode_jpeg(
                &source.path,
                seconds.min((duration - 0.05).max(0.0)),
                size,
                frames.join(format!("{}.jpg", analysis::millis(seconds))),
                cancel,
            )
            .await?;
            let caption = model
                .caption(&frame, &caption_prompt, CAPTION_TOKENS, cancel.clone())
                .await;
            let _ = std::fs::remove_file(&frame);
            let caption = caption.map_err(|error| interrupted_or(cancel, error.context(unreadable())))?;
            let caption = caption.trim();
            if caption.is_empty() {
                return Err(unreadable());
            }
            let conn = database.lock();
            require_running(&conn, id)?;
            source.verify(&conn, file_id)?;
            conn.execute(
                "INSERT OR REPLACE INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES(?1,?2,?3,?3,?4,?5,?6,0)",
                params![evidence, file_id, seconds, caption.chars().take(4_000).collect::<String>(), source.revision, version],
            )?;
            upsert_coverage(
                &conn,
                &evidence,
                file_id,
                seconds,
                duration.min(seconds + 0.001).max(seconds + 0.000_001),
                "sampled",
                &source.revision,
                version,
            )?;
        }

        if job.recipe.timeline_mode.as_deref() == Some("moments") {
            analyze_moments(
                id,
                file_id,
                &source,
                duration,
                &signals,
                size,
                version,
                (index, total),
                model,
                frames,
                cancel,
                sink,
                database,
            )
            .await?;
        }
        require_running(&database.lock(), id)?;
        persist_chapter_suggestions(&database.lock(), file_id, &source, version, duration)?;
        {
            let conn = database.lock();
            conn.execute(
                "UPDATE catalog_jobs SET checkpoint_json=?1,progress=?2,updated_at=?3 WHERE id=?4 AND state='running'",
                params![(index + 1).to_string(), (index + 1) as f64 / total as f64, now(), id],
            )?;
        }
        publish(id, sink, database).await;
    }
    Ok(())
}

async fn scan_signals(
    id: &str,
    file_id: i64,
    source: &Source,
    duration: f64,
    cancel: &Arc<AtomicBool>,
    database: &Arc<Mutex<Connection>>,
) -> Result<Vec<analysis::Signal>> {
    let mut signals = Vec::new();
    let mut previous: Option<Vec<u8>> = None;
    let mut start = 0.0;
    while start < duration {
        require_running(&database.lock(), id)?;
        let end = duration.min(start + SIGNAL_CHUNK_SECONDS);
        let coverage = format!(
            "visual-change:{file_id}:{}:signal-v1:{}",
            source.revision,
            analysis::millis(start)
        );
        upsert_coverage(
            &database.lock(),
            &coverage,
            file_id,
            start,
            end,
            "incomplete",
            &source.revision,
            analysis::SIGNAL_MODEL_VERSION,
        )?;
        let (path, flag) = (source.path.clone(), cancel.clone());
        let chunk = match blocking(move || video_export::signal_frames(&path, start, end, &flag)).await {
            Ok(chunk) => chunk,
            Err(_) if cancel.load(Ordering::Acquire) => return Err(Interrupted.into()),
            // Uniform sampling still covers the file; the unscanned span stays
            // recorded as incomplete signal coverage.
            Err(_) => break,
        };
        source.verify(&database.lock(), file_id)?;
        upsert_coverage(
            &database.lock(),
            &coverage,
            file_id,
            start,
            end,
            "sampled",
            &source.revision,
            analysis::SIGNAL_MODEL_VERSION,
        )?;
        signals.extend(analysis::signals_from_frames(&chunk, previous.as_deref()));
        previous = chunk.last().map(|(_, pixels)| pixels.clone());
        start = end;
    }
    Ok(signals)
}

#[allow(clippy::too_many_arguments)]
async fn analyze_moments<M: FrameModel>(
    id: &str,
    file_id: i64,
    source: &Source,
    duration: f64,
    signals: &[analysis::Signal],
    size: (u32, u32),
    frame_version: &str,
    (file_index, total_files): (usize, usize),
    model: &M,
    frames: &Path,
    cancel: &Arc<AtomicBool>,
    sink: &Sink,
    database: &Arc<Mutex<Connection>>,
) -> Result<()> {
    let windows = analysis::windows(duration, signals);
    if windows.is_empty() {
        return Err(unreadable());
    }
    let version = format!("timeline-moment-sequence-v1/{frame_version}");
    for (index, window) in windows.iter().enumerate() {
        require_running(&database.lock(), id)?;
        let coverage = format!(
            "moment-window:{file_id}:{}:{version}:{}",
            source.revision,
            analysis::millis(window.start)
        );
        let processed: bool = database.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM catalog_coverage WHERE id=?1 AND status='sampled' AND source_revision=?2 AND model_version=?3)",
            params![coverage, source.revision, version],
            |r| r.get(0),
        )?;
        if !processed {
            {
                let conn = database.lock();
                source.verify(&conn, file_id)?;
                upsert_coverage(
                    &conn,
                    &coverage,
                    file_id,
                    window.start,
                    window.end,
                    "incomplete",
                    &source.revision,
                    &version,
                )?;
            }
            // Requests past the last decodable frame resolve to that frame; the
            // distinct-index pass collapses such repeats before inference.
            let times: Vec<f64> = window.times.iter().map(|t| t.min((duration - 0.05).max(0.0))).collect();
            let distinct = analysis::distinct_frame_indices(&times, window)?;
            let mut paths = Vec::with_capacity(distinct.len());
            for &frame in &distinct {
                let output = frames.join(format!("moment-{}-{frame}.jpg", analysis::millis(window.start)));
                match decode_jpeg(&source.path, times[frame], size, output, cancel).await {
                    Ok(path) => paths.push(path),
                    Err(error) => {
                        remove_all(&paths);
                        return Err(error);
                    }
                }
            }
            let kept: Vec<f64> = distinct.iter().map(|&i| times[i]).collect();
            let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
            let raw = model
                .sequence(&refs, &analysis::moment_prompt(&kept), MOMENT_TOKENS, cancel.clone())
                .await;
            remove_all(&paths);
            let raw = raw.map_err(|error| interrupted_or(cancel, error.context(unreadable())))?;
            let moments = analysis::parse_moments(&raw, kept.len())?;
            let chapters = analysis::moment_chapters(&moments, &kept, window, file_id, &source.revision, &version);
            let mut conn = database.lock();
            let tx = conn.transaction()?;
            require_running(&tx, id)?;
            source.verify(&tx, file_id)?;
            let prefix = analysis::moment_prefix(file_id, &source.revision, window.start);
            tx.execute(
                "DELETE FROM catalog_chapters WHERE file_id=?1 AND user_edited=0 AND substr(id,1,?2)=?3",
                params![file_id, prefix.chars().count() as i64, prefix],
            )?;
            for chapter in &chapters {
                if !user_edited(&tx, &chapter.id)? {
                    upsert_chapter(&tx, chapter)?;
                }
            }
            upsert_coverage(
                &tx,
                &coverage,
                file_id,
                window.start,
                window.end,
                "sampled",
                &source.revision,
                &version,
            )?;
            tx.commit()?;
        }
        let progress = (file_index as f64 + 0.7 * (index + 1) as f64 / windows.len() as f64) / total_files as f64;
        set_progress(&database.lock(), id, progress)?;
        publish(id, sink, database).await;
    }
    Ok(())
}

fn remove_all(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

fn persist_chapter_suggestions(
    conn: &Connection,
    file_id: i64,
    source: &Source,
    frame_version: &str,
    duration: f64,
) -> Result<()> {
    let mut statement = conn.prepare(
        "SELECT start_seconds,text FROM catalog_passages WHERE file_id=?1 AND source_revision=?2 AND model_version=?3 AND stale=0 AND start_seconds IS NOT NULL AND confidence=0 ORDER BY start_seconds,id",
    )?;
    let captions = statement
        .query_map(params![file_id, source.revision, frame_version], |r| {
            Ok(analysis::Caption {
                seconds: r.get(0)?,
                text: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let suggestions = analysis::chapter_suggestions(file_id, &source.revision, frame_version, duration, &captions);
    if suggestions.is_empty() {
        return Ok(());
    }
    source.verify(conn, file_id)?;
    conn.execute(
        "DELETE FROM catalog_chapters WHERE file_id=?1 AND user_edited=0 AND model_version LIKE ?2",
        params![file_id, format!("{}%", analysis::CHAPTER_SUGGESTION_PREFIX)],
    )?;
    for chapter in &suggestions {
        if !user_edited(conn, &chapter.id)? {
            upsert_chapter(conn, chapter)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "timeline_tests.rs"]
mod tests;
