use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rusqlite::OptionalExtension;
use super::deep_scope::{DeepScope, ScopeError};

use crate::ipc::{
    self, sink::Sink, DeepAnalyzeComplete, DeepAnalyzeFileDone, DeepAnalyzeProgress,
    DeepAnalyzeStarting, DeepAnalyzeStartingPhase, EngineError, EventPayload, IpcEvent, Wrap,
};
use crate::pipeline::deep_analyze::{analyze_file, analyze_file_scoped, analyze_file_via_server, AnalyzeMode};
use crate::pipeline::naming_context::{self, NamingContext};

pub(super) fn same_people(lhs: &[String], rhs: &[String]) -> bool {
    let a: std::collections::BTreeSet<&String> = lhs.iter().collect();
    let b: std::collections::BTreeSet<&String> = rhs.iter().collect();
    a == b
}

pub(crate) fn append_caption_chunk(buf: &Arc<Mutex<String>>, chunk: &str) {
    let trimmed = chunk.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut b = buf.lock();
    if !b.is_empty() && !b.ends_with(' ') {
        b.push(' ');
    }
    b.push_str(trimmed);
}

pub(super) fn batch_eta_seconds(rolling_fps: f64, completed: u64, total: u64) -> Option<f64> {
    let remaining = total.saturating_sub(completed);
    if rolling_fps > 0.01 && remaining > 0 {
        Some(remaining as f64 / rolling_fps)
    } else {
        None
    }
}

/// Read live in the engine's selected face space; fails when the source changed or is gone.
pub(super) fn fetch_naming(conn: &rusqlite::Connection, file_id: i64) -> anyhow::Result<NamingContext> {
    let (model, processing) = naming_context::selected_space();
    naming_context::load(conn, file_id, model.as_deref(), processing)
}

pub(super) const SOURCE_CHANGED: &str = "A file changed or can't be reached. Scan the folder again, then describe it.";

pub(crate) async fn send_gpu_failure_complete(
    sink: &Sink,
    model_kind: &str,
    processed: u64,
    failed: u64,
    total_seconds: f64,
) {
    sink.send(EngineError::new(
        "gpu_device_removed",
        crate::coordinator::GPU_DEVICE_REMOVED_MESSAGE,
    )
    .with_model(model_kind.to_string())
    .into_event())
    .await;
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
        DeepAnalyzeComplete {
            processed,
            failed,
            total_seconds,
            model_kind: model_kind.to_string(),
            cancelled: false,
        },
    ))))
    .await;
}

enum SingleFileBackend {
    Cli(crate::models::vlm::VlmRunner),
    Server(crate::models::resident_service::ResidentLease),
}

/// The per-file CLI is optional: a runtime that only ships llama-server must still analyze a single file.
async fn single_file_backend(model_kind: &str, cancel: &Arc<AtomicBool>) -> anyhow::Result<SingleFileBackend> {
    let cli_error = match crate::models::vlm::VlmRunner::find(cancel).await {
        Ok(runner) => return Ok(SingleFileBackend::Cli(runner)),
        Err(error) => error,
    };
    if crate::models::vlm::find_weights(model_kind).is_none() {
        return Err(cli_error);
    }
    let server = crate::models::resident_service::ResidentService::shared()
        .acquire(model_kind, cancel.clone())
        .await
        .map_err(|server_error| anyhow::anyhow!("{cli_error:#}; llama-server: {server_error:#}"))?;
    Ok(SingleFileBackend::Server(server))
}

pub(crate) async fn handle_deep_analyze_file(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeFilePayload,
    cancel: Arc<AtomicBool>,
) {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        if !super::deep_analyze_mlx::announce(&sink, &payload.model_kind).await {
            return;
        }
        let filter = deep_analyze_target_filter();
        let query_db = db.clone();
        let file_id = payload.file_id;
        let ids = tokio::task::spawn_blocking(move || {
            collect_file_ids(&query_db, &format!("WHERE id = ?1 AND {filter}"), &[&file_id]).unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        return super::deep_analyze_mlx::run_batch(sink, db, &payload.model_kind, ids, cancel, false, false, true, None).await;
    }
    #[cfg_attr(all(target_os = "macos", feature = "apple-adapters"), allow(unreachable_code))]
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeStarting(Wrap::new(
        DeepAnalyzeStarting {
            model_kind: payload.model_kind.clone(),
            phase: DeepAnalyzeStartingPhase::LoadingModel,
            message: "Describing 1 file…".to_string(),
        },
    ))))
    .await;

    let backend = match single_file_backend(&payload.model_kind, &cancel).await {
        Ok(backend) => backend,
        Err(err) => {
            tracing::warn!(?err, "[VLM] no usable runtime for a single-file pass");
            sink.send(EngineError::event("llama_cpp_missing", "Describe Files can't run because its AI runtime isn't installed or isn't working. Install it again in Settings, under Performance."))
            .await;
            // Always send a terminal Complete so the UI clears the 'Loading model…' card.
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed: 0,
                    failed: 1,
                    total_seconds: 0.0,
                    model_kind: payload.model_kind.clone(),
                    // A load/query failure isn't a user cancel: report the ACTUAL cancel state.
                    cancelled: cancel.load(Ordering::Relaxed),
                },
            ))))
            .await;
            return;
        }
    };

    let sink_c = sink.clone();
    let model_kind = payload.model_kind.clone();
    let model_kind_for_progress = model_kind.clone();
    let file_id = payload.file_id;
    let started_at = Instant::now();
    // Wire emission is throttled to 4 Hz so a fast VLM does not flood the sink.
    let caption_buf = Arc::new(Mutex::new(String::new()));
    let last_emit = Arc::new(Mutex::new(Instant::now().checked_sub(Duration::from_millis(500)).unwrap_or_else(Instant::now)));
    let caption_buf_cb = caption_buf.clone();
    let last_emit_cb = last_emit.clone();
    let context = {
        let db = db.clone();
        tokio::task::spawn_blocking(move || fetch_naming(&db.lock(), file_id))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|loaded| loaded)
    };
    let on_token = move |chunk: &str| {
            // try_send + drop-on-overflow: tokens can fire 50+/sec and spawning an awaiting send per token would pile
            // up tasks if the sink filled; the UI gets the next chunk a few ms later.
            append_caption_chunk(&caption_buf_cb, chunk);
            let now = Instant::now();
            let should_emit = {
                let mut last = last_emit_cb.lock();
                if now.duration_since(*last) >= Duration::from_millis(250) {
                    *last = now;
                    true
                } else {
                    false
                }
            };
            if !should_emit {
                return;
            }
            let snapshot = caption_buf_cb.lock().clone();
            let kind = model_kind_for_progress.clone();
            let _ = sink_c.try_send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(
                DeepAnalyzeProgress {
                    processed: 0,
                    total: 1,
                    eta_seconds: None,
                    current_path: None,
                    model_kind: kind,
                    current_caption: Some(snapshot),
                },
            ))));
    };
    let outcome = match (&backend, &context) {
        (_, Err(error)) => {
            tracing::warn!(%error, file_id, "naming evidence unavailable");
            Err(anyhow::anyhow!(SOURCE_CHANGED))
        }
        (SingleFileBackend::Cli(runner), Ok(context)) => {
            analyze_file(db, runner, file_id, &model_kind, AnalyzeMode::Both, cancel.clone(), context, on_token).await
        }
        (SingleFileBackend::Server(server), Ok(context)) => {
            analyze_file_via_server(db, server, file_id, &model_kind, AnalyzeMode::Both, cancel.clone(), context, on_token, None).await
        }
    };
    if let SingleFileBackend::Server(server) = backend
        && let Err(error) = server.finish(outcome.is_ok() && !cancel.load(Ordering::Relaxed)).await {
            tracing::warn!(?error, "[VLM-SERVER] single-file lease cleanup failed");
        }

    match outcome {
        Ok(out) => {
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeFileDone(Wrap::new(
                DeepAnalyzeFileDone {
                    file_id: out.file_id,
                    description: out.description.clone().unwrap_or_default(),
                    proposed_name: out.proposed_name.clone(),
                    model_kind: model_kind.clone(),
                    inherited_from_file_id: None,
                },
            ))))
            .await;
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed: 1,
                    failed: 0,
                    total_seconds: started_at.elapsed().as_secs_f64(),
                    model_kind,
                    cancelled: false,
                },
            ))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "[VLM] single-file pass failed");
            sink.send(EngineError::event("deep_analyze_failed", "FileID couldn't describe this file. Try again."))
            .await;
            // Terminal Complete on failure too so the card clears and Analyze-All re-enables; a genuine failure must
            // report cancelled:false so the '(1 failed)' warning fires.
            let was_cancelled = cancel.load(Ordering::Relaxed);
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed: 0,
                    failed: 1,
                    total_seconds: started_at.elapsed().as_secs_f64(),
                    model_kind,
                    cancelled: was_cancelled,
                },
            ))))
            .await;
        }
    }
}

pub(crate) async fn handle_deep_analyze_folder(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeFolderPayload,
    cancel: Arc<AtomicBool>,
) {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    if !super::deep_analyze_mlx::announce(&sink, &payload.model_kind).await {
        return;
    }
    // Sargable range seek on the path_text index, not a `LIKE 'prefix%'` full-table scan.
    let lo = payload.path_prefix.clone();
    let filter = deep_analyze_target_filter();
    let query_db = db.clone();
    let ids_result = tokio::task::spawn_blocking(move || match crate::scan_session::prefix_upper_bound(&lo) {
        Some(hi) => collect_file_ids(
            &query_db,
            &format!("WHERE path_text >= ?1 AND path_text < ?2 AND {filter}"),
            &[&lo, &hi],
        ),
        None => collect_file_ids(&query_db, &format!("WHERE {filter}"), &[]),
    })
    .await
    .unwrap_or_else(|join| Err(rusqlite::Error::ToSqlConversionFailure(Box::new(join))));
    let ids = match ids_result {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "deep_analyze_folder query");
            // Terminal Complete so the UI clears the 'Preparing…' card.
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed: 0,
                    failed: 1,
                    total_seconds: 0.0,
                    model_kind: payload.model_kind.clone(),
                    // A load/query failure isn't a user cancel: report the ACTUAL cancel state.
                    cancelled: cancel.load(Ordering::Relaxed),
                },
            ))))
            .await;
            return;
        }
    };
    run_deep_analyze_batch(sink, db, &payload.model_kind, ids, cancel, true, false, true, None).await;
}

pub(crate) async fn handle_deep_analyze_all(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    mut payload: ipc::DeepAnalyzeAllPayload,
    cancel: Arc<AtomicBool>,
) {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    if !super::deep_analyze_mlx::announce(&sink, &payload.model_kind).await {
        return;
    }
    let file_ids = payload.file_ids.take();
    let excluded_folders = payload.excluded_folders.take();
    let planning_db = db.clone();
    let selection = if file_ids.as_ref().is_some_and(Vec::is_empty) {
        DeepScope::from_fields_cancelable(file_ids.as_deref(), excluded_folders.as_deref(), &|| cancel.load(Ordering::Relaxed))
            .map(|scope| (scope, Vec::new())).map_err(anyhow::Error::from)
    } else {
        admitted_scope_plan(crate::resource_scheduler::ResourceScheduler::shared(), cancel.clone(), move |cancelled| {
            let scope = DeepScope::from_fields_cancelable(file_ids.as_deref(), excluded_folders.as_deref(), cancelled)?;
            let ids = collect_scoped_file_ids_cancelable(&planning_db, &scope, cancelled)?;
            Ok((scope, ids))
        }).await
    };
    let (scope, ids) = match selection {
        Ok(value) => value,
        Err(error) if error.downcast_ref::<ScopeError>() == Some(&ScopeError::Cancelled) => {
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(DeepAnalyzeComplete {
                processed: 0, failed: 0, total_seconds: 0.0,
                model_kind: payload.model_kind.clone(), cancelled: true,
            })))).await;
            return;
        }
        Err(error) => {
            emit_scope_failure(&sink, DeepAnalyzeComplete {
                processed: 0, failed: 1, total_seconds: 0.0,
                model_kind: payload.model_kind.clone(), cancelled: cancel.load(Ordering::Relaxed),
            }, &ScopeFailure::from_error(&error)).await;
            return;
        }
    };
    run_deep_analyze_batch(
        sink,
        db,
        &payload.model_kind,
        ids,
        cancel,
        payload.skip_existing,
        payload.tags_only,
        payload.propose_renames,
        Some(scope),
    )
    .await;
}

/// 'pdf' is included only when the pdf-analyze render path is compiled in (otherwise queuing PDFs only manufactures
/// failures); `failed = 0` excludes rows a prior GPU death marked failed.
pub(crate) fn deep_analyze_target_filter() -> &'static str {
    // The Mac-hosted engine's MLX analysis also reads documents, audio and 3D models
    // (`DeepAnalyzeRunner.resolveTargets`).
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        "kind IN ('image','pdf','video','doc','audio','model') AND failed = 0"
    }
    #[cfg(all(feature = "pdf-analyze", not(all(target_os = "macos", feature = "apple-adapters"))))]
    {
        "kind IN ('image','video','pdf') AND failed = 0"
    }
    #[cfg(all(not(feature = "pdf-analyze"), not(all(target_os = "macos", feature = "apple-adapters"))))]
    {
        "kind IN ('image','video') AND failed = 0"
    }
}

fn collect_file_ids(
    db: &Arc<Mutex<rusqlite::Connection>>,
    where_clause: &str,
    params: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Vec<i64>> {
    let conn = db.lock();
    let sql = format!("SELECT id FROM files {} ORDER BY id", where_clause);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| r.get::<_, i64>(0))?;
    rows.collect()
}

struct ScopePlanCancellation(Arc<AtomicBool>);
impl Drop for ScopePlanCancellation {
    fn drop(&mut self) { self.0.store(true, Ordering::Relaxed); }
}

async fn admitted_scope_plan<T: Send + 'static>(
    scheduler: Arc<crate::resource_scheduler::ResourceScheduler>,
    cancel: Arc<AtomicBool>,
    plan: impl FnOnce(&dyn Fn() -> bool) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let abandoned = Arc::new(AtomicBool::new(false));
    let _guard = ScopePlanCancellation(abandoned.clone());
    tokio::task::spawn_blocking(move || {
        let cancelled = || cancel.load(Ordering::Relaxed) || abandoned.load(Ordering::Relaxed);
        let _lease = scheduler.reserve_queued(
            crate::resource_scheduler::Demand { memory_mb: 128, cpu: 1, io: 1 },
            crate::resource_scheduler::AcceleratorDemand::default(),
            crate::resource_scheduler::Priority::Interactive, cancelled,
        ).map_err(|error| if cancelled() { ScopeError::Cancelled.into() } else { error })?;
        if cancelled() { return Err(ScopeError::Cancelled.into()); }
        let output = plan(&cancelled)?;
        if cancelled() { return Err(ScopeError::Cancelled.into()); }
        Ok(output)
    }).await?
}

#[cfg(test)]
fn collect_scoped_file_ids(
    db: &Arc<Mutex<rusqlite::Connection>>,
    scope: &DeepScope,
) -> anyhow::Result<Vec<i64>> {
    collect_scoped_file_ids_cancelable(db, scope, &|| false)
}

fn collect_scoped_file_ids_cancelable(
    db: &Arc<Mutex<rusqlite::Connection>>,
    scope: &DeepScope,
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Vec<i64>> {
    if cancelled() { return Err(ScopeError::Cancelled.into()); }
    scope.validate_current_roots()?;
    let filter = deep_analyze_target_filter();
    let mut ids = Vec::new();
    let mut append = |id, path: String| -> anyhow::Result<()> {
        if cancelled() { return Err(ScopeError::Cancelled.into()); }
        if scope.allows_file(id, std::path::Path::new(&path))? {
            anyhow::ensure!(ids.len() < 4_194_304, "Deep scope target list exceeds its 128 MiB admission; select a smaller scope.");
            ids.push(id);
        }
        Ok(())
    };
    if let Some(selected) = scope.selected_ids() {
        for id in selected {
            if cancelled() { return Err(ScopeError::Cancelled.into()); }
            let target = db.lock().query_row(&format!("SELECT id, path_text, length(CAST(path_text AS BLOB)) FROM files WHERE id = ?1 AND {filter}"), [id], |r| {
                if r.get::<_, i64>(2)? > 131_072 {
                    return Err(rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(ScopeError::InvalidFilePath)));
                }
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            }).optional()?;
            if let Some((id, path)) = target { append(id, path)?; }
        }
    } else {
        let upper: Option<i64> = db.lock().query_row(&format!("SELECT MAX(id) FROM files WHERE {filter}"), [], |r| r.get(0))?;
        let Some(upper) = upper else { return Ok(ids); };
        let mut after = i64::MIN;
        loop {
            if cancelled() { return Err(ScopeError::Cancelled.into()); }
            let targets = {
                let conn = db.lock();
                let mut stmt = conn.prepare(&format!("SELECT id, path_text, length(CAST(path_text AS BLOB)) FROM files WHERE id > ?1 AND id <= ?2 AND {filter} ORDER BY id LIMIT 256"))?;
                let mut rows = stmt.query([after, upper])?;
                let mut targets = Vec::new();
                while let Some(row) = rows.next()? {
                    if cancelled() { return Err(ScopeError::Cancelled.into()); }
                    anyhow::ensure!(row.get::<_, i64>(2)? <= 131_072, "Catalog target path exceeds safe scope planning bounds.");
                    targets.push((row.get::<_, i64>(0)?, row.get::<_, String>(1)?));
                }
                targets
            };
            if targets.is_empty() { break; }
            for (id, path) in targets { after = id; append(id, path)?; }
        }
    }
    Ok(ids)
}

pub(super) fn scoped_catalog_path(
    db: &Arc<Mutex<rusqlite::Connection>>,
    scope: &DeepScope,
    id: i64,
) -> anyhow::Result<std::path::PathBuf> {
    let path: String = db.lock().query_row(
        &format!("SELECT path_text FROM files WHERE id = ?1 AND {}", deep_analyze_target_filter()),
        [id], |r| r.get(0),
    )?;
    Ok(scope.source_path(id, std::path::Path::new(&path))?.ok_or(ScopeError::ExcludedTarget)?)
}

pub(crate) const STOPPED_BY_HELPER: &str =
    "Describe Files stopped because FileID's helper ran into a problem. Try again, and reopen FileID if it keeps happening.";

/// `message` is shown by the app; `detail` is the internal error and only goes to the log.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScopeFailure {
    pub(crate) message: String,
    pub(crate) detail: String,
}

impl ScopeFailure {
    pub(crate) fn internal(detail: impl Into<String>) -> Self {
        Self { message: STOPPED_BY_HELPER.to_owned(), detail: detail.into() }
    }

    pub(crate) fn from_error(error: &anyhow::Error) -> Self {
        match error.downcast_ref::<ScopeError>() {
            Some(scope) => Self { message: scope.to_string(), detail: scope.to_string() },
            None => Self::internal(format!("{error:#}")),
        }
    }
}

pub(super) async fn emit_scope_failure(sink: &Sink, complete: DeepAnalyzeComplete, failure: &ScopeFailure) {
    tracing::warn!(detail = %failure.detail, model = %complete.model_kind, "[DEEP-ANALYZE] run stopped");
    sink.send(EngineError::new(
        "deep_analyze_failed",
        failure.message.clone(),
    )
    .with_model(complete.model_kind.clone())
    .into_event()).await;
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(complete)))).await;
}

pub(crate) async fn emit_batch_terminal_after_cleanup(
    sink: &Sink,
    mut complete: DeepAnalyzeComplete,
    failure: Option<ScopeFailure>,
    cleanup: impl std::future::Future<Output = anyhow::Result<()>>,
) -> Option<ScopeFailure> {
    let cleanup_started = Instant::now();
    let failure = match cleanup.await {
        Ok(()) => failure,
        Err(error) => Some(match failure {
            Some(reason) => ScopeFailure { detail: format!("{}; native cleanup failed: {error:#}", reason.detail), ..reason },
            None => ScopeFailure::internal(format!("Native cleanup failed: {error:#}")),
        }),
    };
    complete.total_seconds += cleanup_started.elapsed().as_secs_f64();
    if let Some(failure) = &failure {
        emit_scope_failure(sink, complete, failure).await;
    } else {
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(complete)))).await;
    }
    failure
}

async fn finalize_batch(
    sink: &Sink,
    server: &mut Option<crate::models::resident_service::ResidentLease>,
    complete: DeepAnalyzeComplete,
    failure: Option<ScopeFailure>,
) {
    let server = server.take();
    let keep = failure.is_none() && !complete.cancelled;
    emit_batch_terminal_after_cleanup(sink, complete, failure, async move {
        match server { Some(server) => server.finish(keep).await, None => Ok(()) }
    }).await;
}

async fn run_deep_analyze_batch(
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
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    return super::deep_analyze_mlx::run_batch(sink, db, model_kind, file_ids, cancel, skip_existing, tags_only, propose_renames, scope).await;
    #[cfg_attr(all(target_os = "macos", feature = "apple-adapters"), allow(unreachable_code))]
    if file_ids.is_empty() {
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(DeepAnalyzeComplete {
            processed: 0, failed: 0, total_seconds: 0.0,
            model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
        })))).await;
        return;
    }
    if let Some(scope) = &scope
        && let Err(error) = scope.validate_current_roots() {
            emit_scope_failure(&sink, DeepAnalyzeComplete {
                processed: 0, failed: 1, total_seconds: 0.0,
                model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
            }, &ScopeFailure::from_error(&error.into())).await;
            return;
        }
    // TagsOnly = one VLM call per file (background auto-tag, ~3× faster); Both = caption + tags + rename (manual Deep
    // Analyze).
    let mode = if tags_only {
        AnalyzeMode::TagsOnly
    } else if propose_renames {
        AnalyzeMode::Both
    } else {
        AnalyzeMode::CaptionAndTags
    };

    // Resolve both backends before DeepAnalyzeStarting so a server-only runtime proceeds while a clean runtime-missing
    // error still surfaces. Weights gate first: without gguf/mmproj neither backend works, and erroring after Starting
    // would strand the UI on 'Loading model…'.
    let weights = crate::models::vlm::find_weights(model_kind);
    if weights.is_none() {
        sink.send(EngineError::new(
            "vlm_model_missing",
            "That AI model isn't installed yet. Install it on the Describe Files page, then try again.",
        )
        .with_model(model_kind.to_string())
        .into_event())
        .await;
        return;
    }
    // The CLI binary is optional: the persistent server only needs llama-server; the no-backend gate below surfaces a
    // runtime error if the server can't start either.
    let discovery_started_at = std::time::Instant::now();
    let runner = match crate::models::vlm::VlmRunner::find(&cancel).await {
        Ok(runner) => Some(runner),
        Err(error) if error.downcast_ref::<crate::models::native_admission::HostAdmissionError>().is_some()
            || !crate::models::native_process::ProcessTracker::shared().allows_discovery() => {
            emit_scope_failure(&sink, DeepAnalyzeComplete {
                processed: 0, failed: 1, total_seconds: discovery_started_at.elapsed().as_secs_f64(),
                model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
            }, &ScopeFailure::internal(format!("Native CLI discovery refused: {error:#}"))).await;
            return;
        }
        Err(_) => None,
    };
    if runner.is_none() {
        tracing::warn!("[VLM] llama-mtmd-cli unavailable; will rely on the persistent server");
    }

    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeStarting(Wrap::new(
        DeepAnalyzeStarting {
            model_kind: model_kind.to_string(),
            phase: DeepAnalyzeStartingPhase::LoadingModel,
            message: format!("Describing {} file(s)…", file_ids.len()),
        },
    ))))
    .await;

    // Prefer the persistent llama-server (loads the model once, ~1-3 s/file); the per-file CLI reloads the multi-GB
    // model each call, turning a library pass into hours. Retained-server cleanup is awaited before batch terminal
    // events.
    let mut server = match weights {
        Some(_) => {
            match crate::models::resident_service::ResidentService::shared().acquire(model_kind, cancel.clone()).await {
                Ok(s) => {
                    // Verify the server accepts our multimodal payload shape (e.g. a 400 on the image_url data-URI)
                    // BEFORE committing the batch; otherwise fall back to the per-file CLI.
                    match crate::pipeline::deep_analyze::vlm_server_payload_ok(&s, &cancel).await {
                        Ok(()) => {
                            tracing::info!(model_kind, "[VLM-SERVER] persistent server up; payload self-test OK; using it for the batch");
                            Some(s)
                        }
                        Err(probe_err) => {
                            let admission_failed = probe_err.downcast_ref::<crate::models::native_admission::HostAdmissionError>().is_some();
                            let cleanup = s.shutdown().await;
                            if admission_failed || cleanup.is_err() || !crate::models::native_process::ProcessTracker::shared().allows_fallback() {
                                let message = cleanup.err().map_or_else(|| probe_err.to_string(), |error| format!("{probe_err}; native cleanup: {error}"));
                                emit_scope_failure(&sink, DeepAnalyzeComplete {
                                    processed: 0, failed: 1, total_seconds: 0.0,
                                    model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
                                }, &ScopeFailure::internal(message)).await;
                                return;
                            }
                            tracing::warn!(?probe_err, "[VLM-SERVER] payload self-test failed; falling back to per-file CLI");
                            sink.send(EngineError::event(
                                "vlm_server_payload_rejected",
                                "Describe Files is using a slower method for this run because the fast one didn't start.",
                            ))
                            .await;
                            None
                        }
                    }
                }
                Err(err) => {
                    if !crate::models::native_admission::startup_allows_cli_fallback(
                        &err, crate::models::native_process::ProcessTracker::shared(),
                    ) {
                        emit_scope_failure(&sink, DeepAnalyzeComplete {
                            processed: 0, failed: 1, total_seconds: 0.0,
                            model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
                        }, &ScopeFailure::from_error(&err)).await;
                        return;
                    }
                    tracing::warn!(?err, "[VLM-SERVER] unavailable; falling back to per-file CLI");
                    None
                }
            }
        }
        None => None,
    };

    let total = file_ids.len() as u64;
    let grouping = {
        let conn = db.lock();
        crate::pipeline::near_duplicate_grouping::plan(&conn, &file_ids)
    };
    let file_ids = grouping.ordered.clone();
    let mut shared: std::collections::HashMap<usize, (i64, Vec<String>, Option<String>)> = std::collections::HashMap::new();
    let mut processed = 0u64;
    let mut failed = 0u64;
    // Use the server until it errors; if it dies mid-batch and a CLI runner exists, fall back to it for the remaining
    // files.
    let mut use_server = server.is_some();
    let started_at = Instant::now();
    // Rolling files/sec feeds the ETA on the NEXT file's progress frames; EMA 0.7 old / 0.3 new, as scan_session.rs.
    let mut rolling_fps = 0.0f64;

    // No runtime can run the (present) weights: surface it ONCE instead of failing every file, then clear the UI's
    // DeepAnalyze* state.
    if server.is_none() && runner.is_none() {
        sink.send(EngineError::event(
            "llama_cpp_missing",
            "Describe Files can't run because its AI runtime isn't installed or isn't working. Install it again in Settings, under Performance.",
        ))
        .await;
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
            DeepAnalyzeComplete {
                processed: 0,
                failed: 0,
                total_seconds: started_at.elapsed().as_secs_f64(),
                model_kind: model_kind.to_string(),
                cancelled: cancel.load(Ordering::Relaxed),
            },
        ))))
        .await;
        return;
    }

    for (idx, file_id) in file_ids.iter().copied().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            finalize_batch(&sink, &mut server, DeepAnalyzeComplete {
                processed, failed, total_seconds: started_at.elapsed().as_secs_f64(),
                model_kind: model_kind.to_owned(), cancelled: true,
            }, None).await;
            return;
        }

        if let Some(scope) = &scope
            && let Err(error) = scoped_catalog_path(&db, scope, file_id) {
                finalize_batch(&sink, &mut server, DeepAnalyzeComplete {
                    processed, failed: failed + 1, total_seconds: started_at.elapsed().as_secs_f64(),
                    model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
                }, Some(ScopeFailure::from_error(&error))).await;
                return;
            }

        if skip_existing {
            let already = {
                let conn = db.lock();
                if tags_only {
                    // Done when this model analyzed it (vlm_model match): the processed marker is written even when no
                    // tag survives filtering, so keying on a source='vlm' tag row re-analyzed such files every run.
                    conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM files WHERE id=?1 AND vlm_model=?2)",
                        rusqlite::params![file_id, model_kind],
                        |r| r.get::<_, bool>(0),
                    )
                    .unwrap_or(false)
                } else {
                    // Only a completed full pass satisfies skip-existing; caption-only or tags-only runs may leave
                    // partial results.
                    conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM files \
                         WHERE id=?1 AND vlm_full_model = ?2)",
                        rusqlite::params![file_id, model_kind],
                        |r| r.get::<_, bool>(0),
                    )
                    .unwrap_or(false)
                }
            };
            if already {
                let is_last = idx as u64 == total.saturating_sub(1);
                if idx % 100 == 0 || is_last {
                    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(
                        DeepAnalyzeProgress {
                            processed: idx as u64,
                            total,
                            eta_seconds: batch_eta_seconds(rolling_fps, idx as u64, total),
                            current_path: None,
                            model_kind: model_kind.to_string(),
                            current_caption: None,
                        },
                    ))))
                    .await;
                }
                continue;
            }
        }

        let group_index = grouping.group_of.get(&file_id).copied();
        let suffix = grouping.suffixes.get(&file_id).cloned().unwrap_or_default();
        if let Some((from_id, from_faces, base_name)) = group_index.and_then(|g| shared.get(&g)).cloned() {
            let inherited = {
                let conn = db.lock();
                match fetch_naming(&conn, file_id) {
                    Ok(context) if from_id != file_id && same_people(&context.subjects, &from_faces) => {
                        let name = base_name.as_deref().map(|n| crate::pipeline::near_duplicate_grouping::appending_sequence(n, &suffix));
                        Some(crate::pipeline::deep_analyze::inherit_vlm_results(&conn, file_id, from_id, model_kind, mode, name.as_deref(), &context))
                    }
                    _ => None,
                }
            };
            match inherited {
                Some(Ok(out)) => {
                    processed += 1;
                    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeFileDone(Wrap::new(DeepAnalyzeFileDone {
                        file_id: out.file_id,
                        description: out.description.clone().unwrap_or_default(),
                        proposed_name: out.proposed_name.clone(),
                        model_kind: model_kind.to_string(),
                        inherited_from_file_id: Some(from_id),
                    }))))
                    .await;
                    continue;
                }
                Some(Err(error)) => tracing::warn!(?error, file_id, "inherit near-duplicate caption failed"),
                None => {}
            }
        }

        let current_path: Option<String> = {
            let conn = db.lock();
            conn.query_row(
                "SELECT path_text FROM files WHERE id = ?1",
                rusqlite::params![file_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
        };
        // The IPC currentPath carries the real path (not redacted), as the macOS reference; never logged here.
        let eta_seconds = batch_eta_seconds(rolling_fps, idx as u64, total);
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(
            DeepAnalyzeProgress {
                processed: idx as u64,
                total,
                eta_seconds,
                current_path: current_path.clone(),
                model_kind: model_kind.to_string(),
                current_caption: None,
            },
        ))))
        .await;

        let sink_c = sink.clone();
        let model_kind_c = model_kind.to_string();
        // Carry ETA and current path onto streamed caption frames too.
        let current_path_cb = current_path.clone();
        let caption_buf = Arc::new(Mutex::new(String::new()));
        let last_emit = Arc::new(Mutex::new(Instant::now().checked_sub(Duration::from_millis(500)).unwrap_or_else(Instant::now)));
        let caption_buf_cb = caption_buf.clone();
        let last_emit_cb = last_emit.clone();
        let on_token = move |chunk: &str| {
            append_caption_chunk(&caption_buf_cb, chunk);
            let now = Instant::now();
            let should_emit = {
                let mut last = last_emit_cb.lock();
                if now.duration_since(*last) >= Duration::from_millis(250) {
                    *last = now;
                    true
                } else {
                    false
                }
            };
            if !should_emit {
                return;
            }
            let snapshot = caption_buf_cb.lock().clone();
            let kind = model_kind_c.clone();
            let _ = sink_c.try_send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(
                Wrap::new(DeepAnalyzeProgress {
                    processed: idx as u64,
                    total,
                    eta_seconds,
                    current_path: current_path_cb.clone(),
                    model_kind: kind,
                    current_caption: Some(snapshot),
                }),
            )));
        };
        // A queued Chat answer gets the model between files; it stays loaded, so the batch resumes without a reload
        // unless the answer used a different model.
        if use_server && server.is_some() && crate::models::resident_service::ResidentService::shared().has_waiters() {
            drop(server.take());
            tokio::task::yield_now().await;
            match crate::models::resident_service::ResidentService::shared().acquire(model_kind, cancel.clone()).await {
                Ok(lease) => server = Some(lease),
                Err(error) if runner.is_some() && !cancel.load(Ordering::Relaxed) => {
                    tracing::warn!(?error, "[VLM-SERVER] could not take the model back after a hand-over; using the per-file CLI");
                    use_server = false;
                }
                Err(error) => {
                    finalize_batch(&sink, &mut server, DeepAnalyzeComplete {
                        processed, failed, total_seconds: started_at.elapsed().as_secs_f64(),
                        model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
                    }, (!cancel.load(Ordering::Relaxed)).then(|| ScopeFailure::from_error(&error))).await;
                    return;
                }
            }
        }
        let server_active = if use_server { server.as_ref() } else { None };
        let file_started = Instant::now();
        let context = {
            let conn = db.lock();
            fetch_naming(&conn, file_id)
        };
        let context = match context {
            Ok(context) => context,
            Err(error) => {
                failed += 1;
                tracing::warn!(%error, file_id, "naming evidence unavailable");
                sink.send(EngineError::event("deep_source_changed", SOURCE_CHANGED)).await;
                continue;
            }
        };
        let outcome = if let Some(srv) = server_active {
            analyze_file_via_server(
                db.clone(),
                srv,
                file_id,
                model_kind,
                mode,
                cancel.clone(),
                &context,
                on_token,
                scope.as_ref(),
            )
            .await
        } else if let Some(r) = runner.as_ref() {
            analyze_file_scoped(
                db.clone(),
                r,
                file_id,
                model_kind,
                mode,
                cancel.clone(),
                &context,
                on_token,
                scope.as_ref(),
            )
            .await
        } else {
            Err(anyhow::anyhow!(
                "no VLM backend available — server failed to start and the CLI binary is missing"
            ))
        };

        let dt = file_started.elapsed().as_secs_f64();
        if dt > 0.0 {
            let instant = 1.0 / dt;
            rolling_fps = if rolling_fps <= 0.0 {
                instant
            } else {
                0.7 * rolling_fps + 0.3 * instant
            };
        }

        let outcome = match (outcome, group_index) {
            (Ok(mut out), Some(g)) if !shared.contains_key(&g) => {
                let base_name = out.proposed_name.clone();
                if let Some(name) = base_name.as_deref().filter(|_| !suffix.is_empty()) {
                    let named = crate::pipeline::near_duplicate_grouping::appending_sequence(name, &suffix);
                    let conn = db.lock();
                    if conn
                        .execute("UPDATE files SET vlm_proposed_name=?1 WHERE id=?2", rusqlite::params![named, file_id])
                        .is_ok()
                    {
                        out.proposed_name = Some(named);
                    }
                }
                shared.insert(g, (file_id, context.subjects.clone(), base_name));
                Ok(out)
            }
            (other, _) => other,
        };
        match outcome {
            Ok(out) => {
                processed += 1;
                sink.send(IpcEvent::now(EventPayload::DeepAnalyzeFileDone(Wrap::new(
                    DeepAnalyzeFileDone {
                        file_id: out.file_id,
                        description: out.description.clone().unwrap_or_default(),
                        proposed_name: out.proposed_name.clone(),
                        model_kind: model_kind.to_string(),
                        inherited_from_file_id: None,
                    },
                ))))
                .await;
            }
            Err(err) => {
                failed += 1;
                if err.downcast_ref::<ScopeError>().is_some()
                    || err.downcast_ref::<crate::models::native_admission::HostAdmissionError>().is_some()
                {
                    finalize_batch(&sink, &mut server, DeepAnalyzeComplete {
                        processed, failed, total_seconds: started_at.elapsed().as_secs_f64(),
                        model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
                    }, Some(ScopeFailure::from_error(&err))).await;
                    return;
                }
                tracing::warn!(?err, file_id, "deep analyze file failed");
                // A per-file error (unreadable image, one rejected request) must NOT tear down a HEALTHY server and
                // drop the batch to the far slower CLI. Re-probe with the startup self-test payload and abandon the
                // server ONLY if that also fails.
                if use_server && runner.is_some() {
                    let probe = match server.as_ref() {
                        Some(srv) => crate::models::native_admission::probe_requests_fallback(
                            crate::pipeline::deep_analyze::vlm_server_payload_ok(srv, &cancel).await,
                        ),
                        None => Ok(true),
                    };
                    let server_dead = match probe {
                        Ok(dead) => dead,
                        Err(probe_error) => {
                            let cleanup = match server.take() {
                                Some(srv) => srv.shutdown().await,
                                None => Ok(()),
                            };
                            let message = cleanup.err().map_or_else(|| probe_error.to_string(), |error| format!("{probe_error}; native cleanup: {error}"));
                            emit_scope_failure(&sink, DeepAnalyzeComplete {
                                processed, failed, total_seconds: started_at.elapsed().as_secs_f64(),
                                model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
                            }, &ScopeFailure::internal(message)).await;
                            return;
                        }
                    };
                    if server_dead {
                        if let Some(srv) = server.take()
                            && let Err(cleanup) = srv.shutdown().await {
                                emit_scope_failure(&sink, DeepAnalyzeComplete {
                                    processed, failed, total_seconds: started_at.elapsed().as_secs_f64(),
                                    model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
                                }, &ScopeFailure::internal(format!("{cleanup:#}"))).await;
                                return;
                            }
                        if !crate::models::native_process::ProcessTracker::shared().allows_fallback() {
                            emit_scope_failure(&sink, DeepAnalyzeComplete {
                                processed, failed, total_seconds: started_at.elapsed().as_secs_f64(),
                                model_kind: model_kind.to_owned(), cancelled: cancel.load(Ordering::Relaxed),
                            }, &ScopeFailure::internal("Native fallback blocked by failed, unconfirmed or closed cleanup")).await;
                            return;
                        }
                        tracing::warn!(
                            "[DEEP-ANALYZE] persistent server is unresponsive; falling back to per-file CLI for the rest of the batch"
                        );
                        use_server = false;
                    } else {
                        tracing::debug!(
                            file_id,
                            "[DEEP-ANALYZE] per-file error but server still healthy; keeping the persistent server"
                        );
                    }
                }
            }
        }
    }

    finalize_batch(&sink, &mut server, DeepAnalyzeComplete {
        processed, failed, total_seconds: started_at.elapsed().as_secs_f64(),
        model_kind: model_kind.to_owned(), cancelled: false,
    }, None).await;
}

#[cfg(test)]
#[path = "deep_scope_handler_tests.rs"]
mod scope_handler_tests;

#[cfg(test)]
#[path = "deep_scope_admission_tests.rs"]
mod scope_admission_tests;

#[cfg(test)]
mod tests {
    use super::append_caption_chunk;
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn run_caption_chunks(chunks: &[&str]) -> String {
        let buf = Arc::new(Mutex::new(String::new()));
        for c in chunks {
            append_caption_chunk(&buf, c);
        }
        
        buf.lock().clone()
    }

    #[test]
    fn batch_eta_seconds_mirrors_scan_eta_semantics() {
        assert_eq!(super::batch_eta_seconds(0.0, 0, 100), None);
        assert_eq!(super::batch_eta_seconds(2.0, 10, 100), Some(45.0));
        assert_eq!(super::batch_eta_seconds(2.0, 100, 100), None);
        // A vanishingly small rate is "no rate" (the scan pipeline's > 0.01 fps gate).
        assert_eq!(super::batch_eta_seconds(0.001, 1, 100), None);
    }

    #[test]
    fn caption_chunks_join_with_single_space() {
        let out = run_caption_chunks(&["A", "dog", "sits", "on", "a", "couch"]);
        assert_eq!(out, "A dog sits on a couch");
    }

    #[test]
    fn caption_chunks_trim_trailing_whitespace() {
        let out = run_caption_chunks(&["A", "dog ", " sits  ", "on", "  a couch"]);
        assert_eq!(out, "A dog sits on a couch");
    }

    #[test]
    fn caption_chunks_drop_blank_lines() {
        let out = run_caption_chunks(&["A", "", "dog", "   ", "sits"]);
        assert_eq!(out, "A dog sits");
    }

    #[test]
    fn caption_chunks_handle_multi_word_lines() {
        let out = run_caption_chunks(&["A dog sits", "on a couch"]);
        assert_eq!(out, "A dog sits on a couch");
    }

    fn in_memory_db() -> Arc<Mutex<rusqlite::Connection>> {
        let conn = crate::db::test_support::memory_db();
        Arc::new(Mutex::new(conn))
    }

    fn insert_file(
        db: &Arc<Mutex<rusqlite::Connection>>,
        path: &str,
        kind: &str,
        failed: i64,
        vlm_model: Option<&str>,
        vlm_description: Option<&str>,
    ) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO files \
             (path_text, path_hash, size_bytes, scanned_at, kind, extension, failed, vlm_model, vlm_description) \
             VALUES (?1, 0, 1, 0.0, ?2, '', ?3, ?4, ?5)",
            rusqlite::params![path, kind, failed, vlm_model, vlm_description],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn target_filter_includes_pdfs_and_excludes_failed() {
        let db = in_memory_db();
        let img = insert_file(&db, r"C:\lib\a.jpg", "image", 0, None, None);
        let vid = insert_file(&db, r"C:\lib\b.mp4", "video", 0, None, None);
        let pdf = insert_file(&db, r"C:\lib\c.pdf", "pdf", 0, None, None);
        let dead = insert_file(&db, r"C:\lib\d.jpg", "image", 1, None, None);
        let _doc = insert_file(&db, r"C:\lib\e.docx", "doc", 0, None, None);

        let ids = super::collect_file_ids(
            &db,
            &format!("WHERE {}", super::deep_analyze_target_filter()),
            &[],
        )
        .unwrap();

        assert!(ids.contains(&img), "image must be a target");
        assert!(ids.contains(&vid), "video must be a target");
        #[cfg(feature = "pdf-analyze")]
        assert!(ids.contains(&pdf), "pdf must be a target when render ships");
        #[cfg(not(feature = "pdf-analyze"))]
        assert!(!ids.contains(&pdf), "pdf excluded without the render feature");
        assert!(!ids.contains(&dead), "failed=1 row must be excluded");
    }

    /// A file captioned by an OLD model is not "already done" for a NEW one, so a VLM switch re-analyzes.
    #[test]
    fn full_pass_skip_is_model_aware() {
        let db = in_memory_db();
        let fid = insert_file(
            &db,
            r"C:\lib\a.jpg",
            "image",
            0,
            Some("gemma-3-4b"),
            Some("a dog on a couch"),
        );

        let skip_for = |model: &str| -> bool {
            let conn = db.lock();
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM files \
                 WHERE id=?1 AND vlm_model=?2 AND vlm_description IS NOT NULL)",
                rusqlite::params![fid, model],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
        };

        assert!(skip_for("gemma-3-4b"), "same-model row is already done");
        assert!(
            !skip_for("qwen2.5-vl-7b"),
            "a model switch must re-analyze, not skip the old model's caption"
        );
    }
}
