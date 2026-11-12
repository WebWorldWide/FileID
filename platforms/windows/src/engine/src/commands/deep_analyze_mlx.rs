//! Deep Analyze in the engine the Mac app hosts: the batch loop of
//! `run_deep_analyze_batch`, with the Swift engine's MLX analysis
//! (`crate::apple::vlm`) as the only backend. Events follow the Swift runner
//! (`DeepAnalyzeRunner.run`): loading, per-file progress with the live caption,
//! one `deepAnalyzeFileDone` per file and a single terminal complete.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::deep_analyze::{append_caption_chunk, batch_eta_seconds, emit_scope_failure, fetch_face_names, same_people, scoped_catalog_path};
use super::deep_scope::{DeepScope, ScopeError};
use crate::apple::vlm::{self, Event, MlxLease, Resident};
use crate::ipc::{
    sink::Sink, DeepAnalyzeComplete, DeepAnalyzeFileDone, DeepAnalyzeProgress, DeepAnalyzeStarting, DeepAnalyzeStartingPhase,
    EngineError, EventPayload, IpcEvent, ModelDownloadProgress, Wrap,
};
use crate::pipeline::deep_analyze::{analyze_file_via_mlx, AnalyzeMode};

fn complete(model_kind: &str, processed: u64, failed: u64, started: Instant, cancelled: bool) -> DeepAnalyzeComplete {
    DeepAnalyzeComplete { processed, failed, total_seconds: started.elapsed().as_secs_f64(), model_kind: model_kind.to_owned(), cancelled }
}

async fn send_complete(sink: &Sink, complete: DeepAnalyzeComplete) {
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(complete)))).await;
}

/// `ModelDownloadProgress` frames for a load, as the Swift runner forwards them.
fn load_progress(sink: &Sink, model_kind: &str) -> impl FnMut(Event) + use<> {
    let sink = sink.clone();
    let model_kind = model_kind.to_owned();
    move |event| {
        if let Event::Progress(value) = event {
            let bytes = |key: &str| value[key].as_u64().filter(|bytes| *bytes > 0);
            let _ = sink.try_send(IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(ModelDownloadProgress {
                model_kind: model_kind.clone(),
                fraction: value["fraction"].as_f64().unwrap_or(0.0),
                message: value["message"].as_str().unwrap_or_default().to_owned(),
                bytes_done: bytes("bytesDone"),
                total_bytes: bytes("totalBytes"),
            }))));
        }
    }
}

/// Load the model for a batch. Emits the failure and the terminal complete
/// itself and returns `None` when the batch can't start.
async fn start(sink: &Sink, model_kind: &str, cancel: &Arc<AtomicBool>, started: Instant) -> Option<MlxLease> {
    if !vlm::installed(model_kind) {
        sink.send(
            EngineError::new(
                "vlm_model_missing",
                format!("The {model_kind} model isn't installed yet. Install it from the Deep Analyze tab, then try again."),
            )
            .with_model(model_kind.to_owned())
            .into_event(),
        )
        .await;
        send_complete(sink, complete(model_kind, 0, 0, started, false)).await;
        return None;
    }
    let display = vlm::model_spec(model_kind).ok().and_then(|spec| spec["displayName"].as_str().map(str::to_owned)).unwrap_or_else(|| model_kind.to_owned());
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeStarting(Wrap::new(DeepAnalyzeStarting {
        model_kind: model_kind.to_owned(),
        phase: DeepAnalyzeStartingPhase::LoadingModel,
        message: format!("Loading {display}…"),
    }))))
    .await;
    match Resident::shared().acquire(model_kind, cancel, load_progress(sink, model_kind)).await {
        Ok(lease) => Some(lease),
        Err(_) if cancel.load(Ordering::Relaxed) => {
            send_complete(sink, complete(model_kind, 0, 0, started, true)).await;
            None
        }
        Err(error) => {
            sink.send(EngineError::new("deep_load_failed", format!("Could not load {display}: {error:#}")).into_event()).await;
            send_complete(sink, complete(model_kind, 0, 0, started, false)).await;
            None
        }
    }
}

fn already_done(db: &Mutex<rusqlite::Connection>, file_id: i64, model_kind: &str, tags_only: bool) -> bool {
    let sql = if tags_only {
        "SELECT EXISTS(SELECT 1 FROM files WHERE id=?1 AND vlm_model=?2)"
    } else {
        "SELECT EXISTS(SELECT 1 FROM files WHERE id=?1 AND vlm_full_model=?2)"
    };
    db.lock().query_row(sql, rusqlite::params![file_id, model_kind], |row| row.get::<_, bool>(0)).unwrap_or(false)
}

fn image_path(db: &Mutex<rusqlite::Connection>, file_id: i64) -> Option<std::path::PathBuf> {
    db.lock()
        .query_row("SELECT path_text FROM files WHERE id=?1 AND kind='image'", [file_id], |row| row.get::<_, String>(0))
        .ok()
        .map(std::path::PathBuf::from)
}

/// Per-token live caption on `deepAnalyzeProgress`, throttled to 4 Hz.
fn caption_stream(sink: &Sink, model_kind: &str, processed: u64, total: u64, eta: Option<f64>, path: Option<String>) -> impl FnMut(&str) + use<> {
    let sink = sink.clone();
    let model_kind = model_kind.to_owned();
    let buffer = Arc::new(Mutex::new(String::new()));
    let mut last = Instant::now().checked_sub(Duration::from_millis(500)).unwrap_or_else(Instant::now);
    move |chunk: &str| {
        append_caption_chunk(&buffer, chunk);
        let now = Instant::now();
        if now.duration_since(last) < Duration::from_millis(250) {
            return;
        }
        last = now;
        let _ = sink.try_send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(DeepAnalyzeProgress {
            processed,
            total,
            eta_seconds: eta,
            current_path: path.clone(),
            model_kind: model_kind.clone(),
            current_caption: Some(buffer.lock().clone()),
        }))));
    }
}

/// Keeps a run's proposed names unique per folder, as the Swift engine does
/// (`RESERVES_PROPOSED_NAMES`), and stores the reserved name.
fn reserve_name(
    db: &Arc<Mutex<rusqlite::Connection>>,
    file_id: i64,
    name: Option<String>,
    reserved: &mut std::collections::HashSet<String>,
) -> Option<String> {
    use crate::pipeline::deep_analyze_naming::{reserve_proposed_name, RESERVES_PROPOSED_NAMES};
    if !RESERVES_PROPOSED_NAMES || name.is_none() {
        return name;
    }
    let conn = db.lock();
    let path: String = conn.query_row("SELECT path_text FROM files WHERE id=?1", [file_id], |row| row.get(0)).unwrap_or_default();
    let unique = reserve_proposed_name(name.clone(), &path, reserved);
    if unique != name
        && let Err(error) = conn.execute("UPDATE files SET vlm_proposed_name=?1 WHERE id=?2", rusqlite::params![unique, file_id])
    {
        tracing::warn!(?error, file_id, "storing the reserved proposed name failed");
        return name;
    }
    unique
}

/// Deep Analyze `file_ids` with the MLX model; sends the terminal complete.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_batch(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    model_kind: &str,
    file_ids: Vec<i64>,
    cancel: Arc<AtomicBool>,
    skip_existing: bool,
    tags_only: bool,
    propose_renames: bool,
    scope: Option<DeepScope>,
) {
    let started = Instant::now();
    // `AIModelKind(rawValue:)` refuses a kind outside the favourites and the catalog.
    if crate::models::mlx::repo(model_kind).is_none() {
        sink.send(
            EngineError::new("unknown_model", format!("Unknown Deep Analyze model: {model_kind}."))
                .with_model(model_kind.to_owned())
                .into_event(),
        )
        .await;
        send_complete(&sink, DeepAnalyzeComplete { total_seconds: 0.0, ..complete(model_kind, 0, 1, started, false) }).await;
        return;
    }
    if cancel.load(Ordering::Relaxed) {
        send_complete(&sink, complete(model_kind, 0, 0, started, true)).await;
        return;
    }
    if file_ids.is_empty() {
        send_complete(&sink, complete(model_kind, 0, 0, started, false)).await;
        return;
    }
    if let Some(scope) = &scope
        && let Err(error) = scope.validate_current_roots()
    {
        emit_scope_failure(&sink, complete(model_kind, 0, 1, started, cancel.load(Ordering::Relaxed)), error.to_string()).await;
        return;
    }
    let mode = if tags_only {
        AnalyzeMode::TagsOnly
    } else if propose_renames {
        AnalyzeMode::Both
    } else {
        AnalyzeMode::CaptionAndTags
    };
    let Some(mut lease) = start(&sink, model_kind, &cancel, started).await else { return };

    let total = file_ids.len() as u64;
    let grouping = crate::pipeline::near_duplicate_grouping::plan(&db.lock(), &file_ids);
    let ordered = grouping.ordered.clone();
    let mut shared: std::collections::HashMap<usize, (i64, Vec<String>, Option<String>)> = std::collections::HashMap::new();
    let mut reserved = std::collections::HashSet::new();
    let (mut processed, mut failed) = (0u64, 0u64);
    let batch_started = Instant::now();
    let mut rolling_fps = 0.0f64;

    for (index, file_id) in ordered.iter().copied().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            send_complete(&sink, complete(model_kind, processed, failed, started, true)).await;
            return;
        }
        if let Some(scope) = &scope
            && let Err(error) = scoped_catalog_path(&db, scope, file_id)
        {
            emit_scope_failure(&sink, complete(model_kind, processed, failed + 1, started, cancel.load(Ordering::Relaxed)), error.to_string()).await;
            return;
        }
        if skip_existing && already_done(&db, file_id, model_kind, tags_only) {
            continue;
        }
        let group = grouping.group_of.get(&file_id).copied();
        let suffix = grouping.suffixes.get(&file_id).cloned().unwrap_or_default();
        if let Some((from_id, from_faces, base_name)) = group.and_then(|g| shared.get(&g)).cloned() {
            let inherited = {
                let conn = db.lock();
                let faces = fetch_face_names(&conn, file_id);
                (from_id != file_id && same_people(&faces, &from_faces)).then(|| {
                    let name = base_name.as_deref().map(|name| crate::pipeline::near_duplicate_grouping::appending_sequence(name, &suffix));
                    crate::pipeline::deep_analyze::inherit_vlm_results(&conn, file_id, from_id, model_kind, mode, name.as_deref())
                })
            };
            match inherited {
                Some(Ok(mut out)) => {
                    out.proposed_name = reserve_name(&db, file_id, out.proposed_name, &mut reserved);
                    processed += 1;
                    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeFileDone(Wrap::new(DeepAnalyzeFileDone {
                        file_id: out.file_id,
                        description: out.description.unwrap_or_default(),
                        proposed_name: out.proposed_name,
                        model_kind: model_kind.to_owned(),
                        inherited_from_file_id: Some(from_id),
                    }))))
                    .await;
                    continue;
                }
                Some(Err(error)) => tracing::warn!(?error, file_id, "inherit near-duplicate caption failed"),
                None => {}
            }
        }

        let current_path: Option<String> =
            db.lock().query_row("SELECT path_text FROM files WHERE id = ?1", [file_id], |row| row.get(0)).ok();
        let eta = batch_eta_seconds(rolling_fps, index as u64, total);
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(DeepAnalyzeProgress {
            processed: index as u64,
            total,
            eta_seconds: eta,
            current_path: current_path.clone(),
            model_kind: model_kind.to_owned(),
            current_caption: None,
        }))))
        .await;

        // A queued Chat answer gets the model between files (the Swift
        // runner's ChatYield); the weights stay loaded for the batch.
        if Resident::shared().has_waiters() {
            drop(lease);
            tokio::task::yield_now().await;
            match Resident::shared().acquire(model_kind, &cancel, load_progress(&sink, model_kind)).await {
                Ok(again) => lease = again,
                Err(error) => {
                    let cancelled = cancel.load(Ordering::Relaxed);
                    if cancelled {
                        send_complete(&sink, complete(model_kind, processed, failed, started, true)).await;
                    } else {
                        emit_scope_failure(&sink, complete(model_kind, processed, failed, started, false), error.to_string()).await;
                    }
                    return;
                }
            }
        }
        // Decode the next distinct photo while this one runs, as the Swift runner does.
        if let Some(next) = ordered[index + 1..]
            .iter()
            .find(|next| group.is_none() || grouping.group_of.get(next).copied() != group)
            .and_then(|next| image_path(&db, *next))
        {
            lease.prefetch(&next).await;
        }

        let file_started = Instant::now();
        let face_names = fetch_face_names(&db.lock(), file_id);
        let on_token = caption_stream(&sink, model_kind, index as u64, total, eta, current_path);
        let outcome =
            analyze_file_via_mlx(db.clone(), &mut lease, file_id, model_kind, mode, cancel.clone(), &face_names, on_token, scope.as_ref()).await;
        let seconds = file_started.elapsed().as_secs_f64();
        if seconds > 0.0 {
            rolling_fps = if rolling_fps <= 0.0 { 1.0 / seconds } else { 0.7 * rolling_fps + 0.3 / seconds };
        }
        if cancel.load(Ordering::Relaxed) {
            send_complete(&sink, complete(model_kind, processed, failed, started, true)).await;
            return;
        }
        let outcome = match (outcome, group) {
            (Ok(mut out), Some(g)) if !shared.contains_key(&g) => {
                let base_name = out.proposed_name.clone();
                if let Some(name) = base_name.as_deref().filter(|_| !suffix.is_empty()) {
                    let named = crate::pipeline::near_duplicate_grouping::appending_sequence(name, &suffix);
                    if db.lock().execute("UPDATE files SET vlm_proposed_name=?1 WHERE id=?2", rusqlite::params![named, file_id]).is_ok() {
                        out.proposed_name = Some(named);
                    }
                }
                shared.insert(g, (file_id, face_names.clone(), base_name));
                Ok(out)
            }
            (other, _) => other,
        };
        match outcome {
            Ok(mut out) => {
                out.proposed_name = reserve_name(&db, file_id, out.proposed_name, &mut reserved);
                processed += 1;
                sink.send(IpcEvent::now(EventPayload::DeepAnalyzeFileDone(Wrap::new(DeepAnalyzeFileDone {
                    file_id: out.file_id,
                    description: out.description.unwrap_or_default(),
                    proposed_name: out.proposed_name,
                    model_kind: model_kind.to_owned(),
                    inherited_from_file_id: None,
                }))))
                .await;
            }
            Err(error) if error.downcast_ref::<ScopeError>().is_some() => {
                emit_scope_failure(&sink, complete(model_kind, processed, failed + 1, started, false), error.to_string()).await;
                return;
            }
            Err(error) => {
                failed += 1;
                tracing::warn!(?error, file_id, "deep analyze file failed");
            }
        }
    }
    tracing::info!(processed, failed, seconds = batch_started.elapsed().as_secs_f64(), "[DEEP] MLX batch done");
    send_complete(&sink, complete(model_kind, processed, failed, started, false)).await;
}
