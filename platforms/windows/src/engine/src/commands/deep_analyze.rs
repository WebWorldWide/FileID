//! Deep Analyze (VLM caption + rename) IPC handlers — single file, folder
//! prefix, or whole library. Streams per-token caption chunks to the UI at
//! 4 Hz so a 50-tok/sec VLM doesn't flood the sink.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::ipc::{
    self, sink::Sink, DeepAnalyzeComplete, DeepAnalyzeFileDone, DeepAnalyzeProgress,
    DeepAnalyzeStarting, DeepAnalyzeStartingPhase, EngineError, EventPayload, IpcEvent, Wrap,
};
use crate::pipeline::deep_analyze::{
    analyze_file, analyze_file_via_server, is_vlm_server_request_error, sanitize_proposed_name,
    AnalyzeMode, AnalyzeOutcome,
};

/// Append a per-token caption chunk from `llama-mtmd-cli` with normalized
/// single-space separators. The CLI emits one stdout line per `on_token`
/// call with variable whitespace; trim + join with one space produces
/// clean English-prose output regardless of the model's whitespace habit.
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

/// Rolling-rate ETA for the Deep Analyze batch, mirroring the scan pipeline's
/// EMA approach (scan_session.rs `maybe_emit_progress`): seconds-remaining =
/// (total - completed) / rolling_fps, or None until there's a positive rate or
/// when nothing remains. Keeps the Deep Analyze progress UI's ETA consistent
/// with the scan sidebar (F-C2-008).
fn batch_eta_seconds(rolling_fps: f64, completed: u64, total: u64) -> Option<f64> {
    let remaining = total.saturating_sub(completed);
    if rolling_fps > 0.01 && remaining > 0 {
        Some(remaining as f64 / rolling_fps)
    } else {
        None
    }
}

fn suffixed_proposed_name(base: &str, suffix: &str) -> String {
    let suffix = sanitize_proposed_name(suffix);
    let max_base = 80usize.saturating_sub(suffix.len().saturating_add(1));
    let mut prefix: String = base.chars().take(max_base).collect();
    while prefix.ends_with('-') || prefix.ends_with('_') {
        prefix.pop();
    }
    if prefix.is_empty() {
        suffix
    } else {
        format!("{prefix}-{suffix}")
    }
}

fn reserve_batch_proposed_name(
    db: &Arc<Mutex<rusqlite::Connection>>,
    outcome: &mut AnalyzeOutcome,
    reserved: &mut HashSet<String>,
) -> anyhow::Result<()> {
    let Some(base) = outcome.proposed_name.as_deref() else {
        return Ok(());
    };
    if reserved.insert(base.to_ascii_lowercase()) {
        return Ok(());
    }
    let path: String = {
        let conn = db.lock();
        conn.query_row(
            "SELECT path_text FROM files WHERE id=?1",
            [outcome.file_id],
            |row| row.get(0),
        )?
    };
    let source_stem = std::path::Path::new(&path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(sanitize_proposed_name)
        .unwrap_or_else(|| "source".to_string());
    let mut ordinal = 1u32;
    let unique = loop {
        let suffix = if ordinal == 1 {
            source_stem.clone()
        } else {
            format!("{source_stem}-{ordinal}")
        };
        let candidate = suffixed_proposed_name(base, &suffix);
        if reserved.insert(candidate.to_ascii_lowercase()) {
            break candidate;
        }
        ordinal = ordinal.saturating_add(1);
    };
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE files SET vlm_proposed_name=?1 WHERE id=?2",
            rusqlite::params![unique, outcome.file_id],
        )?;
    }
    outcome.proposed_name = Some(unique);
    Ok(())
}

/// Distinct named people in a file, formatted for display. Skips clusters flagged
/// `is_unknown` (explicitly opted out). Feeds the Deep Analyze prompt + the
/// deterministic filename prefix so renames carry the people you've named.
/// Byte-faithful with the macOS engine's `fetchFaceNames`. (item 3)
fn fetch_face_names(conn: &rusqlite::Connection, file_id: i64) -> Vec<String> {
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT persons.title, persons.first_name, persons.name \
         FROM persons \
         INNER JOIN face_prints ON face_prints.person_id = persons.id \
         WHERE face_prints.file_id = ?1 \
           AND IFNULL(persons.is_unknown, 0) = 0",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map([file_id], |row| {
        let title: Option<String> = row.get(0)?;
        let first: Option<String> = row.get(1)?;
        let legacy: Option<String> = row.get(2)?;
        Ok(format_person_ref(
            title.as_deref(),
            first.as_deref(),
            legacy.as_deref(),
        ))
    });
    let mut names = Vec::new();
    if let Ok(rows) = rows {
        for formatted in rows.flatten() {
            if !formatted.is_empty() {
                names.push(formatted);
            }
        }
    }
    names
}

/// `title + first_name`, else `first_name`, else `title`, else legacy `name`
/// (each trimmed). Matches the macOS `formatPersonRef`. (item 3)
fn format_person_ref(title: Option<&str>, first: Option<&str>, legacy: Option<&str>) -> String {
    let t = title.unwrap_or("").trim();
    let f = first.unwrap_or("").trim();
    if !t.is_empty() && !f.is_empty() {
        return format!("{t} {f}");
    }
    if !f.is_empty() {
        return f.to_string();
    }
    if !t.is_empty() {
        return t.to_string();
    }
    legacy.unwrap_or("").trim().to_string()
}

async fn send_cancelled_complete(sink: &Sink, model_kind: &str) {
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
        DeepAnalyzeComplete {
            processed: 0,
            failed: 0,
            total_seconds: 0.0,
            model_kind: model_kind.to_string(),
            cancelled: true,
        },
    ))))
    .await;
}

/// Terminal response for setup failures before any file is processed.
async fn send_early_failure_complete(sink: &Sink, model_kind: &str, cancel: &AtomicBool) {
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
        DeepAnalyzeComplete {
            processed: 0,
            failed: 1,
            total_seconds: 0.0,
            model_kind: model_kind.to_string(),
            cancelled: cancel.load(Ordering::Relaxed),
        },
    ))))
    .await;
}

async fn send_single_file_failure_complete(
    sink: &Sink,
    model_kind: &str,
    error: &anyhow::Error,
    cancel: &AtomicBool,
    total_seconds: f64,
) {
    let was_cancelled = cancel.load(Ordering::Relaxed);
    if !was_cancelled {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "deep_analyze_failed".into(),
            message: format!("{error}"),
            path: None,
            model_kind: Some(model_kind.to_string()),
        }))))
        .await;
    }
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
        DeepAnalyzeComplete {
            processed: 0,
            failed: u64::from(!was_cancelled),
            total_seconds,
            model_kind: model_kind.to_string(),
            cancelled: was_cancelled,
        },
    ))))
    .await;
}

pub(crate) async fn send_gpu_failure_complete(
    sink: &Sink,
    model_kind: &str,
    processed: u64,
    failed: u64,
    total_seconds: f64,
) {
    sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
        kind: "gpu_device_removed".into(),
        message: crate::coordinator::GPU_DEVICE_REMOVED_MESSAGE.into(),
        path: None,
        model_kind: Some(model_kind.to_string()),
    }))))
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

struct GpuCancelBridge(tokio::task::JoinHandle<()>);

impl GpuCancelBridge {
    fn start(cancel: Arc<AtomicBool>) -> Self {
        Self(tokio::spawn(async move {
            crate::coordinator::wait_for_process_gpu_device_removed().await;
            cancel.store(true, Ordering::Release);
        }))
    }
}

impl Drop for GpuCancelBridge {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn canonical_vlm_model_kind(model_kind: &str) -> Option<&'static str> {
    match crate::models::registry::lookup_full(model_kind) {
        crate::models::registry::LookupResult::Found(model)
            if matches!(
                model.id,
                "qwen2_5_vl_7b" | "gemma_3_4b" | "mistral_small_3_2"
            ) =>
        {
            Some(model.id)
        }
        crate::models::registry::LookupResult::Found(_)
        | crate::models::registry::LookupResult::Unknown => None,
    }
}

async fn resolve_model_or_reject(
    sink: &Sink,
    model_kind: &str,
    cancel: &AtomicBool,
) -> Option<&'static str> {
    if let Some(canonical) = canonical_vlm_model_kind(model_kind) {
        return Some(canonical);
    }
    sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
        kind: "unknown_model".into(),
        message: format!("Unknown Deep Analyze model: {model_kind}."),
        path: None,
        model_kind: Some(model_kind.to_string()),
    }))))
    .await;
    send_early_failure_complete(sink, model_kind, cancel).await;
    None
}

pub(crate) async fn handle_deep_analyze_file(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeFilePayload,
    cancel: Arc<AtomicBool>,
) {
    if cancel.load(Ordering::Acquire) {
        send_cancelled_complete(&sink, &payload.model_kind).await;
        return;
    }
    let Some(model_kind) =
        resolve_model_or_reject(&sink, &payload.model_kind, &cancel).await
    else {
        return;
    };
    if crate::coordinator::process_gpu_device_removed() {
        send_gpu_failure_complete(&sink, model_kind, 0, 1, 0.0).await;
        return;
    }
    let _gpu_cancel = GpuCancelBridge::start(cancel.clone());
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeStarting(Wrap::new(
        DeepAnalyzeStarting {
            model_kind: model_kind.to_string(),
            phase: DeepAnalyzeStartingPhase::LoadingModel,
            message: format!("Captioning file #{}…", payload.file_id),
        },
    ))))
    .await;

    let runner = match crate::models::vlm::VlmRunner::find() {
        Ok(r) => r,
        Err(err) => {
            if crate::coordinator::process_gpu_device_removed() {
                send_gpu_failure_complete(&sink, model_kind, 0, 1, 0.0).await;
                return;
            }
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "llama_cpp_missing".into(),
                message: format!("{err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
            // Always send a terminal Complete so the UI clears the
            // "Loading model…" card instead of stranding forever (#6).
            send_early_failure_complete(&sink, model_kind, &cancel).await;
            return;
        }
    };

    let sink_c = sink.clone();
    let model_kind = model_kind.to_string();
    let model_kind_for_progress = model_kind.clone();
    let file_id = payload.file_id;
    let started_at = Instant::now();
    // Accumulate per-token text so the UI can render the live caption
    // stream word-by-word. Throttle wire emission to 4 Hz so a
    // 50-tok/sec VLM doesn't flood the sink.
    let caption_buf = Arc::new(Mutex::new(String::new()));
    let last_emit = Arc::new(Mutex::new(
        Instant::now()
            .checked_sub(Duration::from_millis(500))
            .unwrap_or_else(Instant::now),
    ));
    let caption_buf_cb = caption_buf.clone();
    let last_emit_cb = last_emit.clone();
    let face_names = {
        let conn = db.lock();
        fetch_face_names(&conn, file_id)
    };
    let outcome = analyze_file(
        db,
        &runner,
        file_id,
        &model_kind,
        AnalyzeMode::Both,
        cancel.clone(),
        &face_names,
        move |chunk| {
            // Intentional try_send + drop-on-overflow. Per-token streaming
            // can fire 50+/sec and the original tokio::spawn(async {
            // send.await }) pattern would pile up unbounded tasks if the
            // sink filled. Drops are fine — UI gets the next chunk a few
            // ms later.
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
        },
    )
    .await;

    match outcome {
        Ok(out) => {
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeFileDone(Wrap::new(
                DeepAnalyzeFileDone {
                    file_id: out.file_id,
                    description: out.description.clone().unwrap_or_default(),
                    proposed_name: out.proposed_name.clone(),
                    model_kind: model_kind.clone(),
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
            if crate::coordinator::process_gpu_device_removed()
                || crate::models::runtime::error_has_device_removed_marker(&err)
            {
                send_gpu_failure_complete(
                    &sink,
                    &model_kind,
                    0,
                    1,
                    started_at.elapsed().as_secs_f64(),
                )
                .await;
                return;
            }
            send_single_file_failure_complete(
                &sink,
                &model_kind,
                &err,
                &cancel,
                started_at.elapsed().as_secs_f64(),
            )
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
    if cancel.load(Ordering::Acquire) {
        send_cancelled_complete(&sink, &payload.model_kind).await;
        return;
    }
    let Some(model_kind) =
        resolve_model_or_reject(&sink, &payload.model_kind, &cancel).await
    else {
        return;
    };
    let lo = match normalize_folder_scope(&payload.path_prefix) {
        Ok(scope) => scope,
        Err(message) => {
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "deep_analyze_invalid_scope".into(),
                message: message.into(),
                path: Some(payload.path_prefix.clone()),
                model_kind: Some(model_kind.to_string()),
            }))))
            .await;
            send_early_failure_complete(&sink, model_kind, &cancel).await;
            return;
        }
    };
    // P16: sargable range seek on the path_text index instead of a
    // non-sargable `LIKE 'prefix%'` full-table scan.
    let ids_result = collect_folder_file_ids(&db, &lo);
    let ids = match ids_result {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "deep_analyze_folder query");
            // Terminal Complete on the query failure so the UI clears the
            // "Preparing…" card instead of stranding forever (#6).
            send_early_failure_complete(&sink, model_kind, &cancel).await;
            return;
        }
    };
    // Folder-scoped Deep Analyze is a manual action → full enrichment (Both).
    run_deep_analyze_batch(sink, db, model_kind, ids, cancel, true, false, true).await;
}

pub(crate) async fn handle_deep_analyze_all(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeAllPayload,
    cancel: Arc<AtomicBool>,
) {
    if cancel.load(Ordering::Acquire) {
        send_cancelled_complete(&sink, &payload.model_kind).await;
        return;
    }
    let Some(model_kind) =
        resolve_model_or_reject(&sink, &payload.model_kind, &cancel).await
    else {
        return;
    };
    if payload.file_ids.as_ref().is_some_and(|ids| ids.len() > MAX_SELECTED_FILE_IDS) {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "deep_analyze_selection_too_large".into(),
            message: format!(
                "Analyze Selected accepts at most {MAX_SELECTED_FILE_IDS} files per run. Reduce the selection and try again."
            ),
            path: None,
            model_kind: Some(model_kind.to_string()),
        }))))
        .await;
        send_early_failure_complete(&sink, model_kind, &cancel).await;
        return;
    }
    let ids_result = match payload.file_ids.as_deref() {
        Some(requested) => collect_requested_file_ids(&db, requested),
        None => {
            let (exclusion_sql, exclusion_params) =
                exclusion_where_clause(payload.excluded_folders.as_deref().unwrap_or(&[]));
            let params: Vec<&dyn rusqlite::ToSql> = exclusion_params
                .iter()
                .map(|p| p as &dyn rusqlite::ToSql)
                .collect();
            collect_file_ids(
                &db,
                &format!("WHERE {}{}", deep_analyze_target_filter(), exclusion_sql),
                &params,
            )
        }
    };
    let ids = match ids_result {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "deep_analyze_all query");
            // Terminal Complete on the query failure so the UI clears the
            // "Preparing…" card instead of stranding forever (#6).
            send_early_failure_complete(&sink, model_kind, &cancel).await;
            return;
        }
    };
    if ids.is_empty()
        && payload
            .file_ids
            .as_ref()
            .is_some_and(|requested| !requested.is_empty())
    {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "deep_analyze_no_supported_files".into(),
            message: "None of the selected files can be analyzed. Select an image, video, audio file, PDF, or OBJ model and try again.".into(),
            path: None,
            model_kind: Some(model_kind.to_string()),
        }))))
        .await;
    }
    run_deep_analyze_batch(
        sink,
        db,
        model_kind,
        ids,
        cancel,
        payload.skip_existing,
        payload.tags_only,
        payload.propose_renames,
    )
    .await;
}

/// The `kind IN (...) AND failed = 0` predicate every Deep Analyze target query
/// shares. `'pdf'` is included only when the `pdf-analyze` render path is
/// compiled in (default-on) — without it `rasterize_for_vlm` returns a
/// feature-gate error for every PDF, so queuing them would only manufacture
/// failures (F-C1-005). `'audio'` is named from its embedded title/artist tags
/// (no VLM — `analyze_metadata_named_file`), not rasterized. `failed = 0`
/// excludes rows a prior GPU death marked failed, parity with macOS (F-C1-022).
pub(crate) fn deep_analyze_target_filter() -> &'static str {
    #[cfg(feature = "pdf-analyze")]
    {
        "kind IN ('image','video','pdf','audio','model') AND failed = 0 AND (kind != 'model' OR lower(path_text) LIKE '%.obj')"
    }
    #[cfg(not(feature = "pdf-analyze"))]
    {
        "kind IN ('image','video','audio','model') AND failed = 0 AND (kind != 'model' OR lower(path_text) LIKE '%.obj')"
    }
}

/// Build a `" AND NOT (...) AND NOT (...)"` SQL fragment (empty string if
/// `excluded` is empty) plus its bound params, for `deepAnalyzeAll`'s
/// whole-library scan. Each excluded folder becomes a `[lo, hi)` range over
/// `path_text COLLATE NOCASE`, using the same separator-terminated-prefix
/// technique as `path_safety::resolve_exclusions` (scan exclusions) so
/// excluding "C:\Photos" does NOT also exclude "C:\PhotosBackup" — a bare
/// `LIKE prefix%` (as deepAnalyzeFolder's pathPrefix uses, by contract) would
/// get that wrong. No scan-root containment check: unlike a scan exclusion,
/// a Deep Analyze exclusion applies library-wide, mirroring `purgeExcluded`'s
/// `resolve_exclusion_unrooted` (any absolute folder is a valid target).
///
/// The range is case-folded (`COLLATE NOCASE`) only where the filesystem is,
/// i.e. on Windows. On Linux `/Photos` and `/photos` are different directories,
/// so folding there would exclude a folder the user never asked to exclude —
/// the wrong direction for a privacy control. Matches the app-side comparison
/// in each platform's settings sanitizer.
pub(crate) fn exclusion_where_clause(excluded: &[String]) -> (String, Vec<String>) {
    let mut sql = String::new();
    let mut params = Vec::new();
    let sep = if cfg!(windows) { '\\' } else { '/' };
    let collate = if cfg!(windows) { " COLLATE NOCASE" } else { "" };
    for raw in excluded {
        let Some(resolved) = crate::util::path_safety::resolve_exclusion_unrooted(
            std::path::Path::new(raw),
        ) else {
            continue;
        };
        let mut lo = resolved.original;
        if !lo.ends_with(sep) {
            lo.push(sep);
        }
        let Some(hi) = crate::scan_session::prefix_upper_bound(&lo) else {
            continue;
        };
        let n1 = params.len() + 1;
        let n2 = params.len() + 2;
        use std::fmt::Write as _;
        let _ = write!(
            sql,
            " AND NOT (path_text{collate} >= ?{n1} AND path_text{collate} < ?{n2})"
        );
        params.push(lo);
        params.push(hi);
    }
    (sql, params)
}

fn normalize_folder_scope(raw: &str) -> Result<String, &'static str> {
    let normalized = raw.trim().replace('/', "\\");
    if normalized.is_empty() {
        return Err("Choose a folder before starting Deep Analyze.");
    }
    if normalized.starts_with("\\\\?\\") || normalized.starts_with("\\\\.\\") {
        return Err("Device paths aren't valid Deep Analyze folder scopes.");
    }
    let bytes = normalized.as_bytes();
    let drive_absolute = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'\\';
    let unc_absolute = normalized.starts_with("\\\\")
        && normalized[2..]
            .split('\\')
            .filter(|part| !part.is_empty())
            .take(2)
            .count()
            == 2;
    if !drive_absolute && !unc_absolute {
        return Err("Deep Analyze folder scope must be an absolute path.");
    }
    if normalized.split('\\').any(|part| part == "..") {
        return Err("Deep Analyze folder scope can't contain parent traversal.");
    }
    let mut scoped = normalized.trim_end_matches('\\').to_string();
    scoped.push('\\');
    Ok(scoped)
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

const MAX_SELECTED_FILE_IDS: usize = 10_000;
const ID_QUERY_CHUNK: usize = 400;

fn collect_folder_file_ids(
    db: &Arc<Mutex<rusqlite::Connection>>,
    scope: &str,
) -> rusqlite::Result<Vec<i64>> {
    let Some(upper_bound) = crate::scan_session::prefix_upper_bound(scope) else {
        return Ok(Vec::new());
    };
    collect_file_ids(
        db,
        &format!(
            "WHERE path_text COLLATE NOCASE >= ?1 \
             AND path_text COLLATE NOCASE < ?2 \
             AND {}",
            deep_analyze_target_filter()
        ),
        &[&scope, &upper_bound],
    )
}

fn collect_requested_file_ids(
    db: &Arc<Mutex<rusqlite::Connection>>,
    requested: &[i64],
) -> rusqlite::Result<Vec<i64>> {
    let mut seen = HashSet::with_capacity(requested.len());
    let requested = requested
        .iter()
        .copied()
        .filter(|id| *id > 0 && seen.insert(*id))
        .collect::<Vec<_>>();
    let conn = db.lock();
    let mut valid = HashSet::with_capacity(requested.len());
    for chunk in requested.chunks(ID_QUERY_CHUNK) {
        let placeholders = (0..chunk.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id FROM files WHERE {} AND id IN ({placeholders})",
            deep_analyze_target_filter()
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| r.get::<_, i64>(0))?;
        for row in rows {
            valid.insert(row?);
        }
    }
    Ok(requested.into_iter().filter(|id| valid.contains(id)).collect())
}

fn filter_pending_file_ids(
    db: &Arc<Mutex<rusqlite::Connection>>,
    file_ids: Vec<i64>,
    model_kind: &str,
) -> rusqlite::Result<Vec<i64>> {
    let conn = db.lock();
    let mut completed = HashSet::with_capacity(file_ids.len());
    for chunk in file_ids.chunks(ID_QUERY_CHUNK) {
        let placeholders = (0..chunk.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id FROM files WHERE vlm_full_model = ? AND id IN ({placeholders})"
        );
        let mut values = Vec::with_capacity(chunk.len() + 1);
        values.push(rusqlite::types::Value::Text(model_kind.to_string()));
        values.extend(chunk.iter().copied().map(rusqlite::types::Value::Integer));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |r| r.get::<_, i64>(0))?;
        for row in rows {
            completed.insert(row?);
        }
    }
    Ok(file_ids
        .into_iter()
        .filter(|id| !completed.contains(id))
        .collect())
}

/// `vlm_full_model` is the successful full-`Both` completion marker. A full pass
/// subsumes later partial requests for the same model. Partial-only work stays
/// unmarked, and a partial model switch invalidates the old marker, so a later
/// full skip-existing pass cannot skip missing components. Legacy rows have a
/// NULL marker and safely rerun once.
#[cfg(test)]
fn skip_existing_done(conn: &rusqlite::Connection, file_id: i64, model_kind: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE id=?1 AND vlm_full_model=?2)",
        rusqlite::params![file_id, model_kind],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
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
) {
    if cancel.load(Ordering::Acquire) {
        send_cancelled_complete(&sink, model_kind).await;
        return;
    }
    let file_ids = if skip_existing {
        match filter_pending_file_ids(&db, file_ids, model_kind) {
            Ok(ids) => ids,
            Err(err) => {
                tracing::warn!(?err, "deep_analyze skip-existing query");
                sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                    kind: "deep_analyze_query_failed".into(),
                    message: "FileID couldn't read the Deep Analyze completion state. Check the database and try again.".into(),
                    path: None,
                    model_kind: Some(model_kind.to_string()),
                }))))
                .await;
                send_early_failure_complete(&sink, model_kind, &cancel).await;
                return;
            }
        }
    } else {
        file_ids
    };
    if file_ids.is_empty() {
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
            DeepAnalyzeComplete {
                processed: 0,
                failed: 0,
                total_seconds: 0.0,
                model_kind: model_kind.to_string(),
                cancelled: false,
            },
        ))))
        .await;
        return;
    }
    if crate::coordinator::process_gpu_device_removed() {
        send_gpu_failure_complete(&sink, model_kind, 0, 1, 0.0).await;
        return;
    }
    let _gpu_cancel = GpuCancelBridge::start(cancel.clone());

    // TagsOnly = one VLM call/file (background auto-tag, ~3× faster); Both =
    // caption + tags + rename (the manual Deep Analyze pass).
    let mode = if tags_only {
        AnalyzeMode::TagsOnly
    } else if propose_renames {
        AnalyzeMode::Both
    } else {
        AnalyzeMode::CaptionAndTags
    };

    // Resolve both VLM backends up front so we can gate correctly BEFORE
    // sending DeepAnalyzeStarting. The persistent llama-server only needs
    // llama-server.exe; the per-file CLI needs llama-mtmd-cli.exe. find() is a
    // cheap (~one --version probe) check and find_weights is just file
    // existence — doing them first lets a server-capable runtime proceed even
    // when the CLI-binary check fails (the ordering trap), while still
    // surfacing a clean "runtime missing" error when NOTHING is available.
    // Weights gate FIRST: without the model's gguf/mmproj on disk, neither the
    // persistent server nor the per-file CLI can analyze anything. Surface a
    // clear, actionable error BEFORE DeepAnalyzeStarting — the client's Error
    // handler doesn't clear DeepAnalyze* state, so erroring after Starting would
    // strand the UI on a "Loading model…" banner.
    let weights = crate::models::vlm::find_weights(model_kind);
    if crate::coordinator::process_gpu_device_removed() {
        send_gpu_failure_complete(&sink, model_kind, 0, 1, 0.0).await;
        return;
    }
    if weights.is_none() {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "vlm_model_missing".into(),
            message: format!(
                "The {model_kind} model isn't installed yet. Install it from the Deep Analyze tab, then try again."
            ),
            path: None,
            model_kind: Some(model_kind.to_string()),
        }))))
        .await;
        // Terminal Complete so the app's command slot is released — the client's
        // Error handler doesn't clear DeepAnalyze* state, so an Error alone would
        // strand the tab forever. Matches the sibling setup-failure branches. (M7)
        send_early_failure_complete(&sink, model_kind, &cancel).await;
        return;
    }
    // The CLI binary (llama-mtmd-cli.exe) is OPTIONAL: the persistent server only
    // needs llama-server.exe. None just means "server-only"; the no-backend gate
    // below surfaces a runtime error if the server also can't start.
    let runner = crate::models::vlm::VlmRunner::find().ok();
    if runner.is_none() {
        tracing::warn!("[VLM] llama-mtmd-cli unavailable; will rely on the persistent server");
    }

    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeStarting(Wrap::new(
        DeepAnalyzeStarting {
            model_kind: model_kind.to_string(),
            phase: DeepAnalyzeStartingPhase::LoadingModel,
            message: format!("Analyzing {} file(s)…", file_ids.len()),
        },
    ))))
    .await;

    // Prefer the PERSISTENT llama-server (loads the model ONCE → ~1-3 s/file).
    // The per-file CLI `runner` reloads the multi-GB model on every call, which
    // is fine for a 1-file Deep Analyze but turns a whole-library pass into many
    // hours. Fall back to the CLI when weights are missing or the server can't
    // start. The server is dropped (and killed) when this function returns —
    // including the cancel-early path below.
    let server = match weights {
        Some((gguf, mmproj)) => {
            match crate::models::vlm_server::VlmServer::start(&gguf, &mmproj, &cancel).await {
                Ok(s) => {
                    // A2: verify the server accepts our multimodal payload shape
                    // BEFORE committing the whole batch to it. If it rejects the
                    // request (e.g. 400 on the image_url data-URI — a format that
                    // was never hardware-verified), fall back to the per-file CLI
                    // instead of failing every file silently.
                    let probe = tokio::select! {
                        result = crate::pipeline::deep_analyze::vlm_server_payload_ok(&s) => result,
                        _ = async {
                            while !cancel.load(Ordering::Relaxed) {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        } => Err(anyhow::anyhow!("VLM payload self-test cancelled")),
                    };
                    match probe {
                        Ok(()) => {
                            tracing::info!(model_kind, "[VLM-SERVER] persistent server up; payload self-test OK; using it for the batch");
                            Some(s)
                        }
                        Err(probe_err) => {
                            tracing::warn!(?probe_err, "[VLM-SERVER] payload self-test failed; falling back to per-file CLI");
                            if !cancel.load(Ordering::Relaxed) {
                                sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                                    kind: "vlm_server_payload_rejected".into(),
                                    message: format!(
                                        "The VLM server rejected the image request format; using the slower per-file path instead. ({probe_err:#})"
                                    ),
                                    path: None,
                                    model_kind: None,
                                }))))
                                .await;
                            }
                            None
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(?err, "[VLM-SERVER] unavailable; falling back to per-file CLI");
                    None
                }
            }
        }
        None => None,
    };

    let total = file_ids.len() as u64;
    if crate::coordinator::process_gpu_device_removed() {
        send_gpu_failure_complete(&sink, model_kind, 0, 1, 0.0).await;
        return;
    }
    if cancel.load(Ordering::Relaxed) {
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
            DeepAnalyzeComplete {
                processed: 0,
                failed: 0,
                total_seconds: 0.0,
                model_kind: model_kind.to_string(),
                cancelled: true,
            },
        ))))
        .await;
        return;
    }
    let mut processed = 0u64;
    let mut failed = 0u64;
    // Use the persistent server until it errors; if it dies mid-batch and a CLI
    // runner exists, fall back to per-file CLI for the remaining files instead of
    // failing every one. (audit E5)
    let mut use_server = server.is_some();
    let started_at = Instant::now();
    // Rolling files/sec over completed files → the ETA shown on the NEXT file's
    // progress frames. EMA-smoothed (0.7 old / 0.3 new), mirroring the scan
    // pipeline (scan_session.rs). (F-C2-008)
    let mut rolling_fps = 0.0f64;
    let mut reserved_proposed_names = HashSet::new();

    // No runtime can run the (present) weights: the persistent server didn't
    // start AND there's no CLI binary. Surface the runtime problem ONCE here
    // instead of failing every file in the loop, then clear the UI's
    // DeepAnalyze* state (Starting was already sent above).
    if server.is_none() && runner.is_none() {
        if crate::coordinator::process_gpu_device_removed() {
            send_gpu_failure_complete(&sink, model_kind, 0, 1, started_at.elapsed().as_secs_f64())
                .await;
            return;
        }
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "llama_cpp_missing".into(),
            message: "The llama.cpp runtime isn't usable for image analysis (no working \
                      llama-server.exe or llama-mtmd-cli.exe). Update it from \
                      Settings -> Performance -> 'Install llama.cpp runtime'."
                .into(),
            path: None,
            model_kind: None,
        }))))
        .await;
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
            DeepAnalyzeComplete {
                processed: 0,
                failed: 0,
                total_seconds: started_at.elapsed().as_secs_f64(),
                model_kind: model_kind.to_string(),
                // Runtime-missing error, not a user cancel. (audit F-A2)
                cancelled: cancel.load(Ordering::Relaxed),
            },
        ))))
        .await;
        return;
    }

    // Files run in WAVES sized to the persistent server's slot count
    // (llama-server was started with a matching `-np`): with 2 slots, one
    // request's GPU decode overlaps the other's CPU-side image preprocessing.
    // The CLI fallback stays strictly sequential — each call spawns a fresh
    // process that reloads the multi-GB model.
    let mut cursor = 0usize;
    while cursor < file_ids.len() {
        if crate::coordinator::process_gpu_device_removed() {
            send_gpu_failure_complete(
                &sink,
                model_kind,
                processed,
                failed.saturating_add(1),
                started_at.elapsed().as_secs_f64(),
            )
            .await;
            return;
        }
        if cancel.load(Ordering::Relaxed) {
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed,
                    failed,
                    total_seconds: started_at.elapsed().as_secs_f64(),
                    model_kind: model_kind.to_string(),
                    cancelled: true,
                },
            ))))
            .await;
            return;
        }

        let wave_cap = if use_server {
            server.as_ref().map(|s| s.slots.max(1)).unwrap_or(1)
        } else {
            1
        };
        let mut wave: Vec<(usize, i64, Option<String>)> = Vec::with_capacity(wave_cap);
        while wave.len() < wave_cap && cursor < file_ids.len() {
            let idx = cursor;
            let file_id = file_ids[idx];
            cursor += 1;
            let current_path: Option<String> = {
                let conn = db.lock();
                conn.query_row(
                    "SELECT path_text FROM files WHERE id = ?1",
                    rusqlite::params![file_id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            };
            wave.push((idx, file_id, current_path));
        }
        if wave.is_empty() {
            continue;
        }

        // ETA from the rate of the files completed BEFORE this wave. The IPC
        // currentPath carries the real path (not redacted) for parity with the
        // macOS reference; we never log it here. (F-C2-008)
        //
        // The UI has exactly ONE "current file" slot (name, thumbnail, caption,
        // N-of-M counter) and no monotonic guard, so only the wave's FIRST
        // (lowest-idx) file may drive the live progress channel — emitting a
        // frame per in-flight file made the counter tick backwards and the
        // name/thumbnail/caption flip between concurrent files every wave.
        // Terminal DeepAnalyzeFileDone events still fire for every file.
        {
            let (idx, _file_id, current_path) = &wave[0];
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(
                DeepAnalyzeProgress {
                    processed: *idx as u64,
                    total,
                    eta_seconds: batch_eta_seconds(rolling_fps, *idx as u64, total),
                    current_path: current_path.clone(),
                    model_kind: model_kind.to_string(),
                    current_caption: None,
                },
            ))))
            .await;
        }

        // Persistent server while it's healthy (model already resident); else
        // per-file CLI. `use_server` flips off below if the server dies. (audit E5)
        let server_active = if use_server { server.as_ref() } else { None };
        let wave_used_server = server_active.is_some();
        let streamer_idx = wave[0].0;
        let file_started = Instant::now();
        let wave_futures = wave.iter().map(|(idx, file_id, current_path)| {
            let idx = *idx;
            let file_id = *file_id;
            let is_streamer = idx == streamer_idx;
            let sink_c = sink.clone();
            let model_kind_c = model_kind.to_string();
            // Carry ETA + the current file path onto the streamed caption
            // frames too, so the Deep Analyze UI keeps showing both while a
            // caption renders token-by-token (the macOS schema usage). (F-C2-008)
            let current_path_cb = current_path.clone();
            let eta_seconds = batch_eta_seconds(rolling_fps, idx as u64, total);
            let caption_buf = Arc::new(Mutex::new(String::new()));
            let last_emit = Arc::new(Mutex::new(
                Instant::now()
                    .checked_sub(Duration::from_millis(500))
                    .unwrap_or_else(Instant::now),
            ));
            let caption_buf_cb = caption_buf.clone();
            let last_emit_cb = last_emit.clone();
            let db = db.clone();
            let cancel = cancel.clone();
            let runner = runner.as_ref();
            async move {
                let on_token = move |chunk: &str| {
                    // Non-streamer wave members stay silent on the live
                    // progress channel (see the pre-wave frame comment).
                    if !is_streamer {
                        return;
                    }
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
                let face_names = {
                    let conn = db.lock();
                    fetch_face_names(&conn, file_id)
                };
                let outcome = if let Some(srv) = server_active {
                    analyze_file_via_server(
                        db.clone(),
                        srv,
                        file_id,
                        model_kind,
                        mode,
                        cancel.clone(),
                        &face_names,
                        on_token,
                    )
                    .await
                } else if let Some(r) = runner {
                    analyze_file(
                        db.clone(),
                        r,
                        file_id,
                        model_kind,
                        mode,
                        cancel.clone(),
                        &face_names,
                        on_token,
                    )
                    .await
                } else {
                    // Neither backend available (server failed to start AND no
                    // CLI binary). Can't analyze this file — record a failure
                    // and move on.
                    Err(anyhow::anyhow!(
                        "no VLM backend available — server failed to start and the CLI binary is missing"
                    ))
                };
                (file_id, outcome)
            }
        });
        let outcomes = futures_util::future::join_all(wave_futures).await;

        // Fold the wave's wall time into the rolling rate driving the next
        // wave's ETA (EMA, mirroring scan_session.rs). (F-C2-008)
        let dt = file_started.elapsed().as_secs_f64();
        if dt > 0.0 {
            let instant = outcomes.len() as f64 / dt;
            rolling_fps = if rolling_fps <= 0.0 {
                instant
            } else {
                0.7 * rolling_fps + 0.3 * instant
            };
        }

        // Drain EVERY outcome before acting on a terminal condition: with
        // `join_all` the sibling files of a wave have already run (and
        // persisted their results), so returning mid-iteration on the first
        // device-removed/cancel outcome would silently drop their FileDone
        // events and undercount `processed` in the terminal event.
        let mut wave_gpu_dead = false;
        let mut wave_cancelled = false;
        let mut server_probed_this_wave = false;
        for (file_id, outcome) in outcomes {
            match outcome {
                Ok(mut out) => {
                    if let Err(err) = reserve_batch_proposed_name(
                        &db,
                        &mut out,
                        &mut reserved_proposed_names,
                    ) {
                        failed += 1;
                        tracing::warn!(?err, file_id, "deep analyze proposed-name reservation failed");
                        continue;
                    }
                    processed += 1;
                    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeFileDone(Wrap::new(
                        DeepAnalyzeFileDone {
                            file_id: out.file_id,
                            description: out.description.clone().unwrap_or_default(),
                            proposed_name: out.proposed_name.clone(),
                            model_kind: model_kind.to_string(),
                        },
                    ))))
                    .await;
                }
                Err(err) => {
                    if crate::coordinator::process_gpu_device_removed()
                        || crate::models::runtime::error_has_device_removed_marker(&err)
                    {
                        failed += 1;
                        wave_gpu_dead = true;
                        continue;
                    }
                    if cancel.load(Ordering::Relaxed) {
                        wave_cancelled = true;
                        continue;
                    }
                    tracing::warn!(?err, file_id, "deep analyze file failed");
                    // Local decode/render failures never say anything about server
                    // health. Only a request that reached the server earns the
                    // bounded liveness probe and possible CLI fallback.
                    if use_server
                        && runner.is_some()
                        && !server_probed_this_wave
                        && is_vlm_server_request_error(&err)
                    {
                        server_probed_this_wave = true;
                        // Bound the liveness re-probe. vlm_server_payload_ok runs a
                        // real completion that blocks on the HTTP client's 300s
                        // timeout when the server is wedged — which would ignore the
                        // user's cancel and stall the whole batch. Race it against the
                        // cancel flag (like the startup self-test above) AND cap it
                        // with a short wall-clock timeout; a cancelled or timed-out
                        // probe means we stop trusting the server and abort promptly
                        // (fall back / honor cancel) instead of hanging. (M5)
                        let server_dead = match server.as_ref() {
                            Some(srv) => tokio::select! {
                                biased;
                                _ = async {
                                    while !cancel.load(Ordering::Relaxed) {
                                        tokio::time::sleep(Duration::from_millis(100)).await;
                                    }
                                } => true,
                                probe = tokio::time::timeout(
                                    Duration::from_secs(15),
                                    crate::pipeline::deep_analyze::vlm_server_payload_ok(srv),
                                ) => match probe {
                                    Ok(r) => r.is_err(),
                                    Err(_) => true,
                                },
                            },
                            None => true,
                        };
                        if server_dead {
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
                    // A dead server fails every in-flight file of the wave at
                    // once — those are transport failures, not file failures.
                    // The sequential code lost at most one file to a server
                    // death; keep that bound by retrying each one on the CLI.
                    if wave_used_server && !use_server {
                        if let Some(r) = runner.as_ref() {
                            let face_names = {
                                let conn = db.lock();
                                fetch_face_names(&conn, file_id)
                            };
                            match analyze_file(
                                db.clone(),
                                r,
                                file_id,
                                model_kind,
                                mode,
                                cancel.clone(),
                                &face_names,
                                |_| {},
                            )
                            .await
                            {
                                Ok(mut out) => {
                                    if let Err(err) = reserve_batch_proposed_name(
                                        &db,
                                        &mut out,
                                        &mut reserved_proposed_names,
                                    ) {
                                        failed += 1;
                                        tracing::warn!(
                                            ?err,
                                            file_id,
                                            "deep analyze proposed-name reservation failed"
                                        );
                                        continue;
                                    }
                                    processed += 1;
                                    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeFileDone(
                                        Wrap::new(DeepAnalyzeFileDone {
                                            file_id: out.file_id,
                                            description: out
                                                .description
                                                .clone()
                                                .unwrap_or_default(),
                                            proposed_name: out.proposed_name.clone(),
                                            model_kind: model_kind.to_string(),
                                        }),
                                    )))
                                    .await;
                                    continue;
                                }
                                Err(retry_err) => {
                                    if crate::coordinator::process_gpu_device_removed()
                                        || crate::models::runtime::error_has_device_removed_marker(
                                            &retry_err,
                                        )
                                    {
                                        failed += 1;
                                        wave_gpu_dead = true;
                                        continue;
                                    }
                                    if cancel.load(Ordering::Relaxed) {
                                        wave_cancelled = true;
                                        continue;
                                    }
                                    tracing::warn!(
                                        ?retry_err,
                                        file_id,
                                        "[DEEP-ANALYZE] CLI retry after server death also failed"
                                    );
                                }
                            }
                        }
                    }
                    failed += 1;
                }
            }
        }
        if wave_gpu_dead {
            send_gpu_failure_complete(
                &sink,
                model_kind,
                processed,
                failed,
                started_at.elapsed().as_secs_f64(),
            )
            .await;
            return;
        }
        if wave_cancelled {
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed,
                    failed,
                    total_seconds: started_at.elapsed().as_secs_f64(),
                    model_kind: model_kind.to_string(),
                    cancelled: true,
                },
            ))))
            .await;
            return;
        }
    }

    if crate::coordinator::process_gpu_device_removed() {
        send_gpu_failure_complete(
            &sink,
            model_kind,
            processed,
            failed.saturating_add(1),
            started_at.elapsed().as_secs_f64(),
        )
        .await;
        return;
    }
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
        DeepAnalyzeComplete {
            processed,
            failed,
            total_seconds: started_at.elapsed().as_secs_f64(),
            model_kind: model_kind.to_string(),
            cancelled: false,
        },
    ))))
    .await;
}

#[cfg(test)]
mod tests {
    use super::{append_caption_chunk, reserve_batch_proposed_name, GpuCancelBridge};
    use parking_lot::Mutex;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn run_caption_chunks(chunks: &[&str]) -> String {
        let buf = Arc::new(Mutex::new(String::new()));
        for c in chunks {
            append_caption_chunk(&buf, c);
        }
        let result = buf.lock().clone();
        result
    }

    #[test]
    fn batch_proposed_names_disambiguate_with_the_source_stem() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let paths = [
            std::path::PathBuf::from("library").join("report-2026-08-01.pdf"),
            std::path::PathBuf::from("library").join("report-2026-08-02.pdf"),
        ];
        for (index, path) in paths.iter().enumerate() {
            let id = index as i64 + 1;
            conn.execute(
                "INSERT INTO files
                    (id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed,vlm_proposed_name)
                 VALUES (?1,?2,?1,10,1.0,'pdf','pdf',0,'service-report')",
                rusqlite::params![id, path.to_string_lossy()],
            )
            .unwrap();
        }
        let db = Arc::new(Mutex::new(conn));
        let mut reserved = HashSet::new();
        let mut first = crate::pipeline::deep_analyze::AnalyzeOutcome {
            file_id: 1,
            proposed_name: Some("service-report".into()),
            ..Default::default()
        };
        let mut second = crate::pipeline::deep_analyze::AnalyzeOutcome {
            file_id: 2,
            proposed_name: Some("service-report".into()),
            ..Default::default()
        };

        reserve_batch_proposed_name(&db, &mut first, &mut reserved).unwrap();
        reserve_batch_proposed_name(&db, &mut second, &mut reserved).unwrap();

        assert_eq!(first.proposed_name.as_deref(), Some("service-report"));
        assert_eq!(
            second.proposed_name.as_deref(),
            Some("service-report-report-2026-08-02")
        );
        let persisted: String = db
            .lock()
            .query_row(
                "SELECT vlm_proposed_name FROM files WHERE id=2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted, "service-report-report-2026-08-02");
    }

    #[test]
    fn exclusion_where_clause_is_empty_for_no_exclusions() {
        let (sql, params) = super::exclusion_where_clause(&[]);
        assert_eq!(sql, "");
        assert!(params.is_empty());
    }

    #[test]
    fn exclusion_where_clause_respects_folder_boundaries_end_to_end() {
        // The exact false-positive scan exclusions already guard against
        // (path_safety::resolve_exclusions_containment): excluding a folder
        // must not also exclude a sibling that merely shares a text prefix.
        let root = if cfg!(windows) { r"C:\Library" } else { "/library" };
        let excluded_dir = if cfg!(windows) {
            r"C:\Library\Photos"
        } else {
            "/library/photos"
        };
        let sibling_dir = if cfg!(windows) {
            r"C:\Library\PhotosBackup"
        } else {
            "/library/photosbackup"
        };

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let insert = |id: i64, path: &str| {
            conn.execute(
                "INSERT INTO files
                    (id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed)
                 VALUES (?1,?2,?1,10,1.0,'image','jpg',0)",
                rusqlite::params![id, path],
            )
            .unwrap();
        };
        insert(1, &format!("{excluded_dir}{}kept_out.jpg", std::path::MAIN_SEPARATOR));
        insert(2, &format!("{sibling_dir}{}kept_in.jpg", std::path::MAIN_SEPARATOR));
        insert(3, &format!("{root}{}also_kept_in.jpg", std::path::MAIN_SEPARATOR));

        let (exclusion_sql, exclusion_params) =
            super::exclusion_where_clause(&[excluded_dir.to_string()]);
        assert!(exclusion_sql.contains("AND NOT"));
        assert_eq!(exclusion_params.len(), 2);

        let params: Vec<&dyn rusqlite::ToSql> = exclusion_params
            .iter()
            .map(|p| p as &dyn rusqlite::ToSql)
            .collect();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id FROM files WHERE {}{} ORDER BY id",
                super::deep_analyze_target_filter(),
                exclusion_sql
            ))
            .unwrap();
        let ids: Vec<i64> = stmt
            .query_map(rusqlite::params_from_iter(params), |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            ids,
            vec![2, 3],
            "the excluded folder's own file must be dropped; a same-prefix \
             sibling and an unrelated file must survive"
        );
    }

    #[test]
    fn exclusion_where_clause_ignores_relative_paths() {
        let (sql, params) = super::exclusion_where_clause(&["not/absolute".to_string()]);
        assert_eq!(sql, "");
        assert!(params.is_empty());
    }

    #[test]
    fn batch_eta_seconds_mirrors_scan_eta_semantics() {
        // No rate yet (first file) → None, just like the scan ramp-up.
        assert_eq!(super::batch_eta_seconds(0.0, 0, 100), None);
        // 2 files/sec, 10 of 100 done → 90 remaining → 45 s.
        assert_eq!(super::batch_eta_seconds(2.0, 10, 100), Some(45.0));
        // Nothing remaining → None (no negative/zero ETA).
        assert_eq!(super::batch_eta_seconds(2.0, 100, 100), None);
        // A vanishingly small rate is treated as "no rate" (matches the scan
        // pipeline's > 0.01 fps gate) so we don't emit an absurd ETA.
        assert_eq!(super::batch_eta_seconds(0.001, 1, 100), None);
    }

    #[test]
    fn caption_chunks_join_with_single_space() {
        let out = run_caption_chunks(&["A", "dog", "sits", "on", "a", "couch"]);
        assert_eq!(out, "A dog sits on a couch");
    }

    #[tokio::test]
    async fn process_gpu_failure_cancels_an_active_deep_analyze_job_in_an_isolated_process() {
        const CHILD: &str = "FILEID_DEEP_GPU_CANCEL_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let cancel = Arc::new(AtomicBool::new(false));
            let _bridge = GpuCancelBridge::start(cancel.clone());
            assert!(crate::coordinator::latch_process_gpu_device_removed());
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while !cancel.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("GPU failure must cancel the active Deep Analyze job");
            return;
        }

        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("commands::deep_analyze::tests::process_gpu_failure_cancels_an_active_deep_analyze_job_in_an_isolated_process")
            .arg("--exact")
            .env(CHILD, "1")
            .status()
            .expect("launch isolated Deep Analyze GPU-cancel test");
        assert!(status.success());
    }

    #[tokio::test]
    async fn preset_cancel_is_terminal_before_any_deep_analyze_lifecycle_event() {
        let db = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
        let cancel = Arc::new(AtomicBool::new(true));

        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);
        super::handle_deep_analyze_file(
            sink,
            db.clone(),
            crate::ipc::DeepAnalyzeFilePayload {
                file_id: 1,
                model_kind: "gemma3_4b".into(),
            },
            cancel.clone(),
        )
        .await;
        assert_cancelled_only(&mut events).await;

        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);
        super::handle_deep_analyze_folder(
            sink,
            db.clone(),
            crate::ipc::DeepAnalyzeFolderPayload {
                path_prefix: "C:\\queued".into(),
                model_kind: "gemma3_4b".into(),
            },
            cancel.clone(),
        )
        .await;
        assert_cancelled_only(&mut events).await;

        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);
        super::handle_deep_analyze_all(
            sink,
            db,
            crate::ipc::DeepAnalyzeAllPayload {
                model_kind: "gemma3_4b".into(),
                skip_existing: false,
                file_ids: None,
                tags_only: false,
                propose_renames: true,
                excluded_folders: None,
            },
            cancel,
        )
        .await;
        assert_cancelled_only(&mut events).await;
    }

    async fn assert_cancelled_only(
        events: &mut tokio::sync::mpsc::Receiver<crate::ipc::IpcEvent>,
    ) {
        let event = events.recv().await.expect("Deep Analyze cancelled completion");
        match event.payload {
            crate::ipc::EventPayload::DeepAnalyzeComplete(complete) => {
                assert!(complete.inner.cancelled);
                assert_eq!(complete.inner.processed, 0);
                assert_eq!(complete.inner.failed, 0);
            }
            other => panic!("expected only DeepAnalyzeComplete, got {other:?}"),
        }
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn unknown_model_emits_typed_error_and_terminal_failure() {
        let db = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);

        super::handle_deep_analyze_file(
            sink,
            db,
            crate::ipc::DeepAnalyzeFilePayload {
                file_id: 1,
                model_kind: "not_a_model".into(),
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await;

        let error = events.recv().await.expect("typed unknown-model error");
        match error.payload {
            crate::ipc::EventPayload::Error(error) => {
                assert_eq!(error.inner.kind, "unknown_model");
                assert_eq!(error.inner.model_kind.as_deref(), Some("not_a_model"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let complete = events.recv().await.expect("terminal Deep Analyze failure");
        match complete.payload {
            crate::ipc::EventPayload::DeepAnalyzeComplete(complete) => {
                assert_eq!(complete.inner.processed, 0);
                assert_eq!(complete.inner.failed, 1);
                assert!(!complete.inner.cancelled);
                assert_eq!(complete.inner.model_kind, "not_a_model");
            }
            other => panic!("expected DeepAnalyzeComplete, got {other:?}"),
        }
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn cooperative_single_file_cancel_emits_only_cancelled_terminal() {
        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);
        let cancel = AtomicBool::new(true);
        super::send_single_file_failure_complete(
            &sink,
            "gemma3_4b",
            &anyhow::anyhow!("cancelled"),
            &cancel,
            0.25,
        )
        .await;

        let event = events.recv().await.expect("cancelled terminal");
        match event.payload {
            crate::ipc::EventPayload::DeepAnalyzeComplete(complete) => {
                assert!(complete.inner.cancelled);
                assert_eq!(complete.inner.processed, 0);
                assert_eq!(complete.inner.failed, 0);
            }
            other => panic!("expected only DeepAnalyzeComplete, got {other:?}"),
        }
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn selected_limit_and_zero_selection_have_bounded_terminal_sequences() {
        let db = in_memory_db();

        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);
        super::handle_deep_analyze_all(
            sink,
            db.clone(),
            crate::ipc::DeepAnalyzeAllPayload {
                model_kind: "gemma_3_4b".into(),
                skip_existing: false,
                file_ids: Some(Vec::new()),
                tags_only: false,
                propose_renames: true,
                excluded_folders: None,
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        let empty = events.recv().await.expect("empty selection terminal");
        assert!(matches!(
            empty.payload,
            crate::ipc::EventPayload::DeepAnalyzeComplete(_)
        ));
        assert!(events.try_recv().is_err());

        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);
        super::handle_deep_analyze_all(
            sink,
            db,
            crate::ipc::DeepAnalyzeAllPayload {
                model_kind: "gemma_3_4b".into(),
                skip_existing: false,
                file_ids: Some(vec![1; super::MAX_SELECTED_FILE_IDS + 1]),
                tags_only: false,
                propose_renames: true,
                excluded_folders: None,
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        let error = events.recv().await.expect("oversized selection error");
        match error.payload {
            crate::ipc::EventPayload::Error(error) => {
                assert_eq!(error.inner.kind, "deep_analyze_selection_too_large");
            }
            other => panic!("expected selection error, got {other:?}"),
        }
        let terminal = events.recv().await.expect("oversized selection terminal");
        assert!(matches!(
            terminal.payload,
            crate::ipc::EventPayload::DeepAnalyzeComplete(_)
        ));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn unsupported_only_selection_is_not_silently_reported_as_success() {
        let db = in_memory_db();
        let unsupported =
            insert_file(&db, r"C:\lib\archive.bin", "other", 0, None, None);
        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);

        super::handle_deep_analyze_all(
            sink,
            db,
            crate::ipc::DeepAnalyzeAllPayload {
                model_kind: "gemma_3_4b".into(),
                skip_existing: false,
                file_ids: Some(vec![unsupported]),
                tags_only: false,
                propose_renames: true,
                excluded_folders: None,
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await;

        let error = events.recv().await.expect("unsupported selection error");
        match error.payload {
            crate::ipc::EventPayload::Error(error) => {
                assert_eq!(error.inner.kind, "deep_analyze_no_supported_files");
            }
            other => panic!("expected unsupported selection error, got {other:?}"),
        }
        let terminal = events.recv().await.expect("unsupported selection terminal");
        match terminal.payload {
            crate::ipc::EventPayload::DeepAnalyzeComplete(complete) => {
                assert_eq!(complete.inner.processed, 0);
                assert_eq!(complete.inner.failed, 0);
                assert!(!complete.inner.cancelled);
            }
            other => panic!("expected DeepAnalyzeComplete, got {other:?}"),
        }
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn caption_chunks_trim_trailing_whitespace() {
        // CLI emits trailing space / padding on some lines — must not
        // produce double-spaces.
        let out = run_caption_chunks(&["A", "dog ", " sits  ", "on", "  a couch"]);
        assert_eq!(out, "A dog sits on a couch");
    }

    #[test]
    fn caption_chunks_drop_blank_lines() {
        // CLI emits blank lines between tokens occasionally — must be ignored.
        let out = run_caption_chunks(&["A", "", "dog", "   ", "sits"]);
        assert_eq!(out, "A dog sits");
    }

    #[test]
    fn caption_chunks_handle_multi_word_lines() {
        // Some prompts produce whole sentences per line — keep internal
        // spacing intact, single-space at line boundary.
        let out = run_caption_chunks(&["A dog sits", "on a couch"]);
        assert_eq!(out, "A dog sits on a couch");
    }

    fn in_memory_db() -> Arc<Mutex<rusqlite::Connection>> {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).expect("migrations apply");
        Arc::new(Mutex::new(conn))
    }

    /// Insert a minimal `files` row, returning its id. `vlm_model` /
    /// `vlm_description` are set only when provided so the skip predicate
    /// tests can model "captioned by a specific model" vs "never analyzed".
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

    fn mark_full(db: &Arc<Mutex<rusqlite::Connection>>, file_id: i64, model_kind: &str) {
        db.lock()
            .execute(
                "UPDATE files SET vlm_model=?1, vlm_full_model=?1, vlm_analyzed_at=1 \
                 WHERE id=?2",
                rusqlite::params![model_kind, file_id],
            )
            .unwrap();
    }

    #[test]
    fn folder_scope_is_absolute_normalized_and_path_boundary_safe() {
        assert_eq!(
            super::normalize_folder_scope(" C:/Library/People/ "),
            Ok(r"C:\Library\People\".into())
        );
        assert!(super::normalize_folder_scope("").is_err());
        assert!(super::normalize_folder_scope(r"Library\People").is_err());
        assert!(super::normalize_folder_scope(r"C:\Library\..\Other").is_err());
        assert!(super::normalize_folder_scope(r"\\?\C:\Library").is_err());

        let db = in_memory_db();
        let inside = insert_file(
            &db,
            r"C:\LIBRARY\People\a.jpg",
            "image",
            0,
            None,
            None,
        );
        let nested = insert_file(
            &db,
            r"C:\Library\PEOPLE\Trips\b.jpg",
            "image",
            0,
            None,
            None,
        );
        let sibling = insert_file(
            &db,
            r"C:\Library\PeopleArchive\c.jpg",
            "image",
            0,
            None,
            None,
        );
        let lo = super::normalize_folder_scope(r"c:\library\people").unwrap();
        let ids = super::collect_folder_file_ids(&db, &lo).unwrap();
        assert_eq!(ids, vec![inside, nested]);
        assert!(!ids.contains(&sibling));
    }

    #[tokio::test]
    async fn empty_folder_scope_emits_typed_error_and_terminal_failure() {
        let db = in_memory_db();
        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);
        super::handle_deep_analyze_folder(
            sink,
            db,
            crate::ipc::DeepAnalyzeFolderPayload {
                path_prefix: " ".into(),
                model_kind: "gemma_3_4b".into(),
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await;

        let error = events.recv().await.expect("invalid scope error");
        match error.payload {
            crate::ipc::EventPayload::Error(error) => {
                assert_eq!(error.inner.kind, "deep_analyze_invalid_scope");
            }
            other => panic!("expected invalid scope error, got {other:?}"),
        }
        let terminal = events.recv().await.expect("invalid scope terminal");
        match terminal.payload {
            crate::ipc::EventPayload::DeepAnalyzeComplete(complete) => {
                assert_eq!(complete.inner.failed, 1);
                assert!(!complete.inner.cancelled);
            }
            other => panic!("expected DeepAnalyzeComplete, got {other:?}"),
        }
        assert!(events.try_recv().is_err());
    }

    /// F-C1-005 + F-C1-022: the shared target filter selects renderable PDFs
    /// (when the pdf-analyze render path is compiled in) and excludes rows a
    /// prior GPU death marked failed=1 — parity with the macOS reference.
    #[test]
    fn target_filter_includes_pdfs_and_excludes_failed() {
        let db = in_memory_db();
        let img = insert_file(&db, r"C:\lib\a.jpg", "image", 0, None, None);
        let vid = insert_file(&db, r"C:\lib\b.mp4", "video", 0, None, None);
        let pdf = insert_file(&db, r"C:\lib\c.pdf", "pdf", 0, None, None);
        // failed=1 image (GPU-death-marked) must NOT be a target.
        let dead = insert_file(&db, r"C:\lib\d.jpg", "image", 1, None, None);
        // A non-renderable, non-metadata-nameable kind (doc) is never a target.
        let _doc = insert_file(&db, r"C:\lib\e.docx", "doc", 0, None, None);
        // Audio IS a target now — named from embedded tags (no VLM).
        let aud = insert_file(&db, r"C:\lib\f.mp3", "audio", 0, None, None);
        let obj = insert_file(&db, r"C:\lib\g.obj", "model", 0, None, None);
        let stl = insert_file(&db, r"C:\lib\h.stl", "model", 0, None, None);

        let ids = super::collect_file_ids(
            &db,
            &format!("WHERE {}", super::deep_analyze_target_filter()),
            &[],
        )
        .unwrap();

        assert!(ids.contains(&img), "image must be a target");
        assert!(ids.contains(&vid), "video must be a target");
        assert!(ids.contains(&aud), "audio must be a target (metadata-named)");
        assert!(ids.contains(&obj), "OBJ is the supported model format");
        assert!(!ids.contains(&stl), "unsupported model formats must not be queued");
        #[cfg(feature = "pdf-analyze")]
        assert!(ids.contains(&pdf), "pdf must be a target when render ships");
        #[cfg(not(feature = "pdf-analyze"))]
        assert!(!ids.contains(&pdf), "pdf excluded without the render feature");
        assert!(!ids.contains(&dead), "failed=1 row must be excluded");
    }

    /// F-C1-020: the full-pass skip predicate keys on (file, vlm_full_model). A file
    /// captioned by an OLD model is NOT "already done" for a NEW model, so a
    /// VLM switch re-analyzes instead of skipping every prior file.
    #[test]
    fn vlm_aliases_resolve_to_canonical_completion_model_ids() {
        for (model_kind, expected) in [
            ("qwen2_5_vl_7b", "qwen2_5_vl_7b"),
            ("qwen2.5-vl-7b", "qwen2_5_vl_7b"),
            ("gemma_3_4b", "gemma_3_4b"),
            ("gemma-3-4b", "gemma_3_4b"),
            ("mistral_small_3_2", "mistral_small_3_2"),
            ("mistral-small-3.2", "mistral_small_3_2"),
        ] {
            assert_eq!(
                super::canonical_vlm_model_kind(model_kind),
                Some(expected),
                "alias {model_kind}"
            );
        }
    }

    #[tokio::test]
    async fn skip_existing_treats_alias_as_same_canonical_model() {
        let db = in_memory_db();
        let file_id = insert_file(
            &db,
            r"C:\lib\done.jpg",
            "image",
            0,
            None,
            Some("already complete"),
        );
        mark_full(&db, file_id, "mistral_small_3_2");
        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);

        super::handle_deep_analyze_all(
            sink,
            db,
            crate::ipc::DeepAnalyzeAllPayload {
                model_kind: "mistral-small-3.2".into(),
                skip_existing: true,
                file_ids: Some(vec![file_id]),
                tags_only: false,
                propose_renames: true,
                excluded_folders: None,
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await;

        let event = events.recv().await.expect("canonical alias completion");
        match event.payload {
            crate::ipc::EventPayload::DeepAnalyzeComplete(complete) => {
                assert_eq!(complete.inner.processed, 0);
                assert_eq!(complete.inner.failed, 0);
                assert_eq!(complete.inner.model_kind, "mistral_small_3_2");
                assert!(!complete.inner.cancelled);
            }
            other => panic!("expected DeepAnalyzeComplete, got {other:?}"),
        }
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn full_pass_skip_is_model_aware() {
        let db = in_memory_db();
        let fid = insert_file(
            &db,
            r"C:\lib\a.jpg",
            "image",
            0,
            None,
            Some("a dog on a couch"),
        );
        mark_full(&db, fid, "gemma-3-4b");

        // Exercises the real skip predicate shared by both passes.
        let skip_for = |model: &str| -> bool {
            let conn = db.lock();
            super::skip_existing_done(&conn, fid, model)
        };

        // Same model that captioned it → skip (already done by this model).
        assert!(skip_for("gemma-3-4b"), "same-model row is already done");
        // Different model → NOT skipped, so the new model re-analyzes the file.
        assert!(
            !skip_for("qwen2.5-vl-7b"),
            "a model switch must re-analyze, not skip the old model's caption"
        );
    }

    #[test]
    fn legacy_partial_raw_model_without_full_marker_is_not_skipped() {
        let db = in_memory_db();
        let file_id = insert_file(
            &db,
            r"C:\lib\partial.jpg",
            "image",
            0,
            Some("model-a"),
            Some("caption from a no-rename pass"),
        );
        {
            let conn = db.lock();
            assert!(!super::skip_existing_done(&conn, file_id, "model-a"));
        }
        let pending =
            super::filter_pending_file_ids(&db, vec![file_id], "model-a").unwrap();
        assert_eq!(pending, vec![file_id]);
    }

    #[test]
    fn selected_ids_are_bounded_to_valid_targets_and_keep_request_order() {
        let db = in_memory_db();
        let first = insert_file(&db, r"C:\lib\a.jpg", "image", 0, None, None);
        let failed = insert_file(&db, r"C:\lib\b.jpg", "image", 1, None, None);
        let unsupported = insert_file(&db, r"C:\lib\c.bin", "other", 0, None, None);
        let second = insert_file(&db, r"C:\lib\d.mp4", "video", 0, None, None);

        let selected = super::collect_requested_file_ids(
            &db,
            &[second, failed, first, unsupported, second, -1, 999_999],
        )
        .unwrap();
        assert_eq!(selected, vec![second, first]);
    }

    #[tokio::test]
    async fn all_already_analyzed_finishes_without_loading_a_model() {
        let db = in_memory_db();
        let fid = insert_file(
            &db,
            r"C:\lib\done.jpg",
            "image",
            0,
            None,
            Some("done"),
        );
        mark_full(&db, fid, "missing-test-model");
        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);
        super::run_deep_analyze_batch(
            sink,
            db,
            "missing-test-model",
            vec![fid],
            Arc::new(AtomicBool::new(false)),
            true,
            false,
            true,
        )
        .await;

        let event = events.recv().await.expect("no-work completion");
        match event.payload {
            crate::ipc::EventPayload::DeepAnalyzeComplete(complete) => {
                assert_eq!(complete.inner.processed, 0);
                assert_eq!(complete.inner.failed, 0);
                assert!(!complete.inner.cancelled);
            }
            other => panic!("expected DeepAnalyzeComplete, got {other:?}"),
        }
        assert!(events.try_recv().is_err(), "must not emit model-loading lifecycle events");
    }

    #[tokio::test]
    async fn known_model_missing_weights_emits_error_then_terminal_in_isolated_process() {
        const CHILD: &str = "FILEID_DEEP_MISSING_MODEL_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let db = in_memory_db();
            let fid = insert_file(
                &db,
                r"C:\lib\pending.jpg",
                "image",
                0,
                None,
                None,
            );
            let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(2);
            super::run_deep_analyze_batch(
                sink,
                db,
                "gemma_3_4b",
                vec![fid],
                Arc::new(AtomicBool::new(false)),
                false,
                false,
                true,
            )
            .await;

            let error = events.recv().await.expect("missing model error");
            match error.payload {
                crate::ipc::EventPayload::Error(error) => {
                    assert_eq!(error.inner.kind, "vlm_model_missing");
                }
                other => panic!("expected missing model error, got {other:?}"),
            }
            let terminal = events.recv().await.expect("missing model terminal");
            match terminal.payload {
                crate::ipc::EventPayload::DeepAnalyzeComplete(complete) => {
                    assert_eq!(complete.inner.processed, 0);
                    assert_eq!(complete.inner.failed, 1);
                    assert!(!complete.inner.cancelled);
                }
                other => panic!("expected DeepAnalyzeComplete, got {other:?}"),
            }
            assert!(events.try_recv().is_err());
            return;
        }

        let models = std::env::temp_dir().join(format!(
            "fileid-missing-model-test-{}",
            uuid::Uuid::new_v4()
        ));
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("commands::deep_analyze::tests::known_model_missing_weights_emits_error_then_terminal_in_isolated_process")
            .arg("--exact")
            .env(CHILD, "1")
            .env("FILEID_MODELS_DIR", &models)
            .status()
            .expect("launch isolated missing-model test");
        assert!(status.success());
    }

    /// Regression: metadata-named kinds legitimately persist `vlm_full_model` with a
    /// NULL `vlm_description` (audio with no title/artist/album or a silent
    /// transcript; a `.obj` with only generic names). The full-pass skip
    /// predicate previously also demanded `vlm_description IS NOT NULL`, so these
    /// files NEVER counted as done and were re-analyzed on every pass — re-running
    /// Whisper decode+transcribe for audio. Keyed on `vlm_full_model` they are
    /// done, matching the macOS reference.
    #[test]
    fn full_pass_skips_metadata_named_with_null_description() {
        let db = in_memory_db();
        // Analyzed by this run's model_kind, but no caption was produced.
        let aud = insert_file(&db, r"C:\lib\silent.mp3", "audio", 0, None, None);
        let obj = insert_file(&db, r"C:\lib\part.obj", "model", 0, None, None);
        mark_full(&db, aud, "qwen2.5-vl-7b");
        mark_full(&db, obj, "qwen2.5-vl-7b");
        // Never analyzed → still runs. Insert ALL rows BEFORE locking: insert_file
        // takes db.lock() internally, so inserting while `conn` is held would
        // re-enter the (non-reentrant) mutex and deadlock the test.
        let fresh = insert_file(&db, r"C:\lib\new.mp3", "audio", 0, None, None);

        let conn = db.lock();
        assert!(
            super::skip_existing_done(&conn, aud, "qwen2.5-vl-7b"),
            "an audio file analyzed by this model is done even with a NULL caption — \
             it must not re-run Whisper on every full pass"
        );
        assert!(
            super::skip_existing_done(&conn, obj, "qwen2.5-vl-7b"),
            "a .obj analyzed by this model is done even with a NULL caption"
        );
        // Never analyzed → still runs; model switch → still re-analyzes.
        assert!(
            !super::skip_existing_done(&conn, fresh, "qwen2.5-vl-7b"),
            "a never-analyzed file (vlm_full_model NULL) must run"
        );
        assert!(
            !super::skip_existing_done(&conn, aud, "gemma-3-4b"),
            "a different model must re-analyze, not skip"
        );
    }
}
