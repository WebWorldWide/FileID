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

/// The visual model for timelines: a curated model when installed, otherwise
/// the smallest installed catalog vision model.
pub(crate) fn installed_model() -> Option<&'static str> {
    VLM_PREFERENCE
        .into_iter()
        .find(|kind| crate::models::vlm::find_weights(kind).is_some())
        .or_else(|| {
            let mut installed: Vec<&'static crate::models::catalog::CatalogEntry> = crate::models::catalog::entries()
                .iter()
                .filter(|e| {
                    crate::models::catalog::runnable_vision(&e.id).is_some()
                        && crate::models::vlm::find_weights(&e.id).is_some()
                })
                .collect();
            installed.sort_by(|a, b| a.estimated_resident_gb.total_cmp(&b.estimated_resident_gb));
            installed.first().map(|e| e.id.as_str())
        })
}

const SPEECH_PREFERENCE: [&str; 2] = ["qwen3_asr_0_6b_gguf_q8_0", "qwen3_asr_1_7b_gguf_q8_0"];

/// An installed speech model, preferring the evaluated Qwen3-ASR sizes.
pub(crate) fn installed_speech_model() -> Option<&'static str> {
    let installed = |id: &str| {
        crate::models::catalog::runnable_speech(id).is_some() && crate::models::vlm::find_weights(id).is_some()
    };
    SPEECH_PREFERENCE.into_iter().find(|id| installed(id)).or_else(|| {
        crate::models::catalog::entries()
            .iter()
            .find(|e| installed(&e.id))
            .map(|e| e.id.as_str())
    })
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
        bail!("Install a visual model on the Describe Files page before analyzing videos. No model will be downloaded automatically.")
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
    let speech = match installed_speech_model() {
        Some(_) => " Speech is transcribed on this device in 30-second passages after the visual pass.",
        None => " Install a speech model from the model catalog to add searchable transcripts.",
    };
    let message = if mode == Some("moments") {
        format!("Queued significant-moment drafts from overlapping frame sequences. Review every proposal; sampled frames can still miss actions or hidden outcomes.{speech}")
    } else {
        format!("Visual sampling uses low-resolution change signals to select a representative moment in each ten-second window, along with a starting frame. Fast events can still be missed.{speech}")
    };
    let mut response = super::catalog::response(&request.request_id, "ok", Some(message));
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
    let jobs = with_db(database, |conn| super::catalog::jobs(conn)).await;
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
        let owned = id.to_owned();
        let Some(job) = with_db(database, move |conn| start(conn, &owned)).await? else {
            return Ok(());
        };
        publish(id, sink, database).await;
        let speech = installed_speech_model();
        let visual_share = if speech.is_some() { 0.8 } else { 1.0 };
        let lease = ResidentService::shared()
            .acquire(&job.recipe.model_kind, cancel.clone())
            .await?;
        let result = process(id, &job, &lease, &frames, &cancel, sink, database, visual_share).await;
        let cleanup = lease.finish(result.is_ok()).await;
        result.and(cleanup)?;
        // The speech model loads only after the visual model is released, so
        // the two never need to be resident together.
        if let Some(speech) = speech {
            let lease = ResidentService::shared().acquire(speech, cancel.clone()).await?;
            let result = transcribe(id, &job, speech, &lease, &frames, &cancel, sink, database, visual_share).await;
            let cleanup = lease.finish(result.is_ok()).await;
            result.and(cleanup)?;
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let _ = std::fs::remove_dir_all(&frames);
    *worker().running.lock() = None;
    let terminal = match outcome {
        Ok(()) => Some(None),
        Err(error) if error.is::<Interrupted>() || cancel.load(Ordering::Acquire) => None,
        Err(error) => Some(Some(format!("{error:#}").chars().take(2_000).collect::<String>())),
    };
    let owned = id.to_owned();
    let _ = with_db(database, move |conn| {
        match terminal {
            Some(None) => {
                conn.execute("UPDATE catalog_jobs SET state='completed',progress=1,updated_at=?1 WHERE id=?2 AND state='running'", params![now(), owned])?;
            }
            Some(Some(error)) => {
                conn.execute(
                    "UPDATE catalog_jobs SET state='failed',error=?1,updated_at=?2 WHERE id=?3 AND state='running'",
                    params![error, now(), owned],
                )?;
            }
            None => {}
        }
        Ok(())
    })
    .await;
    publish(id, sink, database).await;
    let owned = id.to_owned();
    with_db(database, move |conn| {
        Ok(conn.query_row("SELECT state FROM catalog_jobs WHERE id=?1", [owned], |r| r.get::<_, String>(0))?)
    })
    .await
    .is_ok_and(|state| state == "queued")
}

/// Confirms the job is still running, then loads and re-verifies its source.
async fn running_source(database: &Arc<Mutex<Connection>>, id: &str, file_id: i64) -> Result<Source> {
    let id = id.to_owned();
    with_db(database, move |conn| {
        require_running(conn, &id)?;
        let source = Source::load(conn, file_id)?;
        source.verify(conn, file_id)?;
        Ok(source)
    })
    .await
}

/// Confirms the job is still running and runs `query` in the same lock turn.
async fn while_running<T: Send + 'static>(
    database: &Arc<Mutex<Connection>>,
    id: &str,
    query: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    let id = id.to_owned();
    with_db(database, move |conn| {
        require_running(conn, &id)?;
        query(conn)
    })
    .await
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

#[derive(Clone)]
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

/// Runs `work` against the writer on the blocking pool. A scan flush or face
/// clustering can hold the writer for seconds; locking it on an async worker
/// would park a runtime thread, and enough of those stall every command.
pub(crate) async fn with_db<T: Send + 'static>(
    database: &Arc<Mutex<Connection>>,
    work: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    let database = database.clone();
    blocking(move || work(&mut database.lock())).await
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
    share: f64,
) -> Result<()> {
    std::fs::create_dir_all(frames)?;
    let total = job.ids.len().max(1);
    for (index, &file_id) in job.ids.iter().enumerate().skip(job.index) {
        let source = running_source(database, id, file_id).await?;
        let probe = {
            let (path, flag) = (source.path.clone(), cancel.clone());
            blocking(move || video_export::analysis_probe(&path, &flag))
                .await
                .map_err(|error| interrupted_or(cancel, error))?
        };
        let duration = probe.duration;
        let version = job.recipe.model_version.as_str();
        {
            let (coverage, revision, model_version) = (format!("{id}-{file_id}-coverage"), source.revision.clone(), version.to_owned());
            with_db(database, move |conn| {
                upsert_coverage(conn, &coverage, file_id, 0.0, duration, "incomplete", &revision, &model_version)
            })
            .await?;
        }

        let signals = scan_signals(id, file_id, &source, duration, cancel, database).await?;
        let size = video_export::fitted_size(probe.width, probe.height, FRAME_SIDE);
        let caption_prompt = crate::models::vlm::caption_prompt_with_faces(&[]);
        for seconds in analysis::sample_times(duration, job.recipe.interval_seconds, &signals) {
            let evidence = format!(
                "frame:{file_id}:{}:{version}:{}",
                source.revision,
                analysis::millis(seconds)
            );
            let (lookup, revision) = (evidence.clone(), source.revision.clone());
            let exists: bool = while_running(database, id, move |conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM catalog_passages WHERE id=?1 AND source_revision=?2 AND stale=0)",
                    params![lookup, revision],
                    |r| r.get(0),
                )?)
            })
            .await?;
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
            let (caption, checked, model_version) = (caption.chars().take(4_000).collect::<String>(), source.clone(), version.to_owned());
            while_running(database, id, move |conn| {
                checked.verify(conn, file_id)?;
                conn.execute(
                    "INSERT OR REPLACE INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES(?1,?2,?3,?3,?4,?5,?6,0)",
                    params![evidence, file_id, seconds, caption, checked.revision, model_version],
                )?;
                upsert_coverage(
                    conn,
                    &evidence,
                    file_id,
                    seconds,
                    duration.min(seconds + 0.001).max(seconds + 0.000_001),
                    "sampled",
                    &checked.revision,
                    &model_version,
                )
            })
            .await?;
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
                (index, total, share),
                model,
                frames,
                cancel,
                sink,
                database,
                decode_frame,
            )
            .await?;
        }
        let (checked, model_version, job_id) = (source.clone(), version.to_owned(), id.to_owned());
        while_running(database, id, move |conn| {
            persist_chapter_suggestions(conn, file_id, &checked, &model_version, duration)?;
            conn.execute(
                "UPDATE catalog_jobs SET checkpoint_json=?1,progress=?2,updated_at=?3 WHERE id=?4 AND state='running'",
                params![
                    (index + 1).to_string(),
                    share * (index + 1) as f64 / total as f64,
                    now(),
                    job_id
                ],
            )?;
            Ok(())
        })
        .await?;
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
        let end = duration.min(start + SIGNAL_CHUNK_SECONDS);
        let coverage = format!(
            "visual-change:{file_id}:{}:signal-v1:{}",
            source.revision,
            analysis::millis(start)
        );
        let (pending, revision) = (coverage.clone(), source.revision.clone());
        while_running(database, id, move |conn| {
            upsert_coverage(conn, &pending, file_id, start, end, "incomplete", &revision, analysis::SIGNAL_MODEL_VERSION)
        })
        .await?;
        let (path, flag) = (source.path.clone(), cancel.clone());
        let chunk = match blocking(move || video_export::signal_frames(&path, start, end, &flag)).await {
            Ok(chunk) => chunk,
            Err(_) if cancel.load(Ordering::Acquire) => return Err(Interrupted.into()),
            // Uniform sampling still covers the file; the unscanned span stays
            // recorded as incomplete signal coverage.
            Err(_) => break,
        };
        let checked = source.clone();
        with_db(database, move |conn| {
            checked.verify(conn, file_id)?;
            upsert_coverage(conn, &coverage, file_id, start, end, "sampled", &checked.revision, analysis::SIGNAL_MODEL_VERSION)
        })
        .await?;
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
    (file_index, total_files, share): (usize, usize, f64),
    model: &M,
    frames: &Path,
    cancel: &Arc<AtomicBool>,
    sink: &Sink,
    database: &Arc<Mutex<Connection>>,
    decoder: impl FrameDecoder,
) -> Result<()> {
    let windows = analysis::windows(duration, signals);
    if windows.is_empty() {
        return Err(unreadable());
    }
    let mut prefetched: Option<Prefetch> = None;
    let result = moment_windows(
        id,
        file_id,
        source,
        duration,
        &windows,
        size,
        frame_version,
        (file_index, total_files, share),
        model,
        frames,
        cancel,
        sink,
        database,
        decoder,
        &mut prefetched,
    )
    .await;
    if let Some(pending) = prefetched {
        pending.discard();
    }
    result
}

/// A window's frame extraction started while the model works on the previous
/// window. Frame extraction is CPU work; inference is GPU work.
struct Prefetch {
    window: usize,
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<Result<Vec<PathBuf>>>,
}

impl Prefetch {
    fn spawn<D: FrameDecoder>(
        decoder: D,
        source: &Path,
        (window, plan): (usize, Vec<(f64, PathBuf)>),
        size: (u32, u32),
        cancel: &Arc<AtomicBool>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (source, cancel, flag) = (source.to_owned(), cancel.clone(), stop.clone());
        let task =
            tokio::task::spawn_blocking(move || decode_window(&decoder, &source, &plan, size, &cancel, Some(&flag)));
        Self { window, stop, task }
    }

    fn discard(self) {
        self.stop.store(true, Ordering::Release);
        tokio::spawn(async move {
            if let Ok(Ok(paths)) = self.task.await {
                remove_all(&paths);
            }
        });
    }
}

pub(crate) trait FrameDecoder: Fn(&Path, f64, (u32, u32), &Path, &AtomicBool) -> Result<()> + Clone + Send + Sync + 'static {}
impl<T: Fn(&Path, f64, (u32, u32), &Path, &AtomicBool) -> Result<()> + Clone + Send + Sync + 'static> FrameDecoder for T {}

fn decode_frame(source: &Path, seconds: f64, size: (u32, u32), output: &Path, cancel: &AtomicBool) -> Result<()> {
    let rgb = video_export::frame_rgb(source, seconds, size.0, size.1, cancel)?;
    write_jpeg(output, rgb, size.0, size.1)
}

/// Decodes every planned frame in order; on failure, cancellation or `stop`
/// (checked between frames) removes what it wrote.
fn decode_window<D: FrameDecoder>(
    decoder: &D,
    source: &Path,
    plan: &[(f64, PathBuf)],
    size: (u32, u32),
    cancel: &AtomicBool,
    stop: Option<&AtomicBool>,
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::with_capacity(plan.len());
    for (seconds, output) in plan {
        let decoded = if stop.is_some_and(|s| s.load(Ordering::Acquire)) {
            Err(anyhow!("frame prefetch discarded"))
        } else {
            decoder(source, *seconds, size, output, cancel)
        };
        if let Err(error) = decoded {
            let _ = std::fs::remove_file(output);
            remove_all(&paths);
            return Err(error);
        }
        paths.push(output.clone());
    }
    Ok(paths)
}

/// Timestamps of a window's distinct frames and where each frame is written.
fn window_plan(window: &analysis::Window, duration: f64, frames: &Path) -> Result<(Vec<f64>, Vec<(f64, PathBuf)>)> {
    // Requests past the last decodable frame resolve to that frame; the
    // distinct-index pass collapses such repeats before inference.
    let times: Vec<f64> = window.times.iter().map(|t| t.min((duration - 0.05).max(0.0))).collect();
    let distinct = analysis::distinct_frame_indices(&times, window)?;
    let kept: Vec<f64> = distinct.iter().map(|&i| times[i]).collect();
    let plan = distinct
        .iter()
        .map(|&frame| {
            (times[frame], frames.join(format!("moment-{}-{frame}.jpg", analysis::millis(window.start))))
        })
        .collect();
    Ok((kept, plan))
}

#[allow(clippy::too_many_arguments)]
async fn moment_windows<M: FrameModel, D: FrameDecoder>(
    id: &str,
    file_id: i64,
    source: &Source,
    duration: f64,
    windows: &[analysis::Window],
    size: (u32, u32),
    frame_version: &str,
    (file_index, total_files, share): (usize, usize, f64),
    model: &M,
    frames: &Path,
    cancel: &Arc<AtomicBool>,
    sink: &Sink,
    database: &Arc<Mutex<Connection>>,
    decoder: D,
    prefetched: &mut Option<Prefetch>,
) -> Result<()> {
    let version = format!("timeline-moment-sequence-v1/{frame_version}");
    for (index, window) in windows.iter().enumerate() {
        let coverage = format!(
            "moment-window:{file_id}:{}:{version}:{}",
            source.revision,
            analysis::millis(window.start)
        );
        let (lookup, revision, model_version) = (coverage.clone(), source.revision.clone(), version.clone());
        let processed: bool = while_running(database, id, move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_coverage WHERE id=?1 AND status='sampled' AND source_revision=?2 AND model_version=?3)",
                params![lookup, revision, model_version],
                |r| r.get(0),
            )?)
        })
        .await?;
        if !processed {
            {
                let (pending, checked, model_version, (start, end)) = (coverage.clone(), source.clone(), version.clone(), (window.start, window.end));
                with_db(database, move |conn| {
                    checked.verify(conn, file_id)?;
                    upsert_coverage(conn, &pending, file_id, start, end, "incomplete", &checked.revision, &model_version)
                })
                .await?;
            }
            let (kept, plan) = window_plan(window, duration, frames)?;
            let pending = prefetched.take();
            let paths = match pending {
                Some(pending) if pending.window == index => {
                    pending.task.await.map_err(|error| anyhow!(error)).and_then(|decoded| decoded)
                }
                other => {
                    if let Some(stale) = other {
                        stale.discard();
                    }
                    let (path, flag, decode) = (source.path.clone(), cancel.clone(), decoder.clone());
                    blocking(move || decode_window(&decode, &path, &plan, size, &flag, None)).await
                }
            }
            .map_err(|error| interrupted_or(cancel, error))?;
            if let Some(next) = windows.get(index + 1) {
                match window_plan(next, duration, frames) {
                    Ok((_, next_plan)) => {
                        *prefetched = Some(Prefetch::spawn(
                            decoder.clone(),
                            &source.path,
                            (index + 1, next_plan),
                            size,
                            cancel,
                        ));
                    }
                    Err(error) => tracing::debug!(?error, "next moment window has no usable frames"),
                }
            }
            let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
            let raw = model
                .sequence(&refs, &analysis::moment_prompt(&kept), MOMENT_TOKENS, cancel.clone())
                .await;
            remove_all(&paths);
            let raw = raw.map_err(|error| interrupted_or(cancel, error.context(unreadable())))?;
            let moments = analysis::parse_moments(&raw, kept.len())?;
            let chapters = analysis::moment_chapters(&moments, &kept, window, file_id, &source.revision, &version);
            let prefix = analysis::moment_prefix(file_id, &source.revision, window.start);
            let (job_id, checked, model_version, (start, end)) = (id.to_owned(), source.clone(), version.clone(), (window.start, window.end));
            with_db(database, move |conn| {
                let tx = conn.transaction()?;
                require_running(&tx, &job_id)?;
                checked.verify(&tx, file_id)?;
                tx.execute(
                    "DELETE FROM catalog_chapters WHERE file_id=?1 AND user_edited=0 AND substr(id,1,?2)=?3",
                    params![file_id, prefix.chars().count() as i64, prefix],
                )?;
                for chapter in &chapters {
                    if !user_edited(&tx, &chapter.id)? {
                        upsert_chapter(&tx, chapter)?;
                    }
                }
                upsert_coverage(&tx, &coverage, file_id, start, end, "sampled", &checked.revision, &model_version)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        }
        let progress =
            share * (file_index as f64 + 0.7 * (index + 1) as f64 / windows.len() as f64) / total_files as f64;
        let job_id = id.to_owned();
        with_db(database, move |conn| set_progress(conn, &job_id, progress)).await?;
        publish(id, sink, database).await;
    }
    Ok(())
}

/// Speech pass: 16 kHz chunks of each video's first audio track are
/// transcribed by the speech model; every chunk's coverage is recorded so a
/// resumed job skips verified chunks and an unreadable one stays incomplete.
#[allow(clippy::too_many_arguments)]
async fn transcribe<M: FrameModel>(
    id: &str,
    job: &Job,
    speech_model: &str,
    model: &M,
    frames: &Path,
    cancel: &Arc<AtomicBool>,
    sink: &Sink,
    database: &Arc<Mutex<Connection>>,
    visual_share: f64,
) -> Result<()> {
    std::fs::create_dir_all(frames)?;
    let version = format!("timeline-speech-v1/{speech_model}");
    let total = job.ids.len().max(1);
    for (index, &file_id) in job.ids.iter().enumerate() {
        let source = running_source(database, id, file_id).await?;
        let probe = {
            let (path, flag) = (source.path.clone(), cancel.clone());
            blocking(move || video_export::analysis_probe(&path, &flag))
                .await
                .map_err(|error| interrupted_or(cancel, error))?
        };
        if !probe.has_audio {
            continue;
        }
        let coverage = format!("speech-coverage:{file_id}:{}:{version}", source.revision);
        {
            let (pending, revision, model_version, duration) = (coverage.clone(), source.revision.clone(), version.clone(), probe.duration);
            with_db(database, move |conn| conn_coverage_if_absent(conn, &pending, file_id, duration, &revision, &model_version)).await?;
        }
        let chunks = analysis::speech_chunks(probe.duration, analysis::SPEECH_CHUNK_SECONDS);
        let mut complete = true;
        for (chunk_index, &(start, end)) in chunks.iter().enumerate() {
            let chunk_id = format!("{coverage}:chunk:{chunk_index}");
            let (lookup, revision, model_version) = (chunk_id.clone(), source.revision.clone(), version.clone());
            let verified: bool = while_running(database, id, move |conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM catalog_coverage WHERE id=?1 AND status='verified' AND source_revision=?2 AND model_version=?3)",
                    params![lookup, revision, model_version],
                    |r| r.get(0),
                )?)
            })
            .await?;
            if !verified {
                let (path, flag) = (source.path.clone(), cancel.clone());
                let audio = blocking(move || video_export::audio_wav(&path, start, end - start, &flag)).await;
                let text = match audio {
                    Ok(audio) => {
                        let wav = frames.join(format!("speech-{file_id}-{chunk_index}.wav"));
                        std::fs::write(&wav, audio)?;
                        let reply = model.caption(&wav, analysis::SPEECH_PROMPT, 400, cancel.clone()).await;
                        let _ = std::fs::remove_file(&wav);
                        Some(analysis::transcript_text(
                            &reply.map_err(|error| interrupted_or(cancel, error))?,
                        ))
                    }
                    Err(_) if cancel.load(Ordering::Acquire) => return Err(Interrupted.into()),
                    Err(_) => None,
                };
                complete &= text.is_some();
                let (checked, model_version) = (source.clone(), version.clone());
                while_running(database, id, move |conn| {
                    checked.verify(conn, file_id)?;
                    let status = match text {
                        Some(text) => {
                            if !text.is_empty() {
                                conn.execute(
                                    "INSERT OR REPLACE INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES(?1,?2,?3,?4,?5,?6,?7,0.5)",
                                    params![format!("speech:{file_id}:{}:{model_version}:{}", checked.revision, analysis::millis(start)), file_id, start, end, text, checked.revision, model_version],
                                )?;
                            }
                            "verified"
                        }
                        None => "incomplete",
                    };
                    upsert_coverage(conn, &chunk_id, file_id, start, end, status, &checked.revision, &model_version)
                })
                .await?;
            }
            let progress = visual_share
                + (1.0 - visual_share) * (index as f64 + (chunk_index + 1) as f64 / chunks.len() as f64) / total as f64;
            let job_id = id.to_owned();
            with_db(database, move |conn| set_progress(conn, &job_id, progress)).await?;
            publish(id, sink, database).await;
        }
        let checked = source.clone();
        with_db(database, move |conn| {
            checked.verify(conn, file_id)?;
            conn.execute(
                "UPDATE catalog_coverage SET status=?1 WHERE id=?2",
                params![if complete { "verified" } else { "incomplete" }, coverage],
            )?;
            Ok(())
        })
        .await?;
    }
    Ok(())
}

fn conn_coverage_if_absent(
    conn: &Connection,
    id: &str,
    file_id: i64,
    duration: f64,
    revision: &str,
    version: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?1,?2,0,?3,'incomplete',?4,?5)",
        params![id, file_id, duration, revision, version],
    )?;
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
