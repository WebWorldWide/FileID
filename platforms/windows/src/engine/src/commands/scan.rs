//! `startScan` IPC handler — loads ML model weights, registers the scan
//! coordinator in the shared state slot, runs the pipeline, clears the slot
//! when done. Pre-flight checks that required models are installed and
//! that no scan is already running.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::coordinator::ScanCoordinator;
use crate::ipc::{
    self, sink::Sink, EngineError, EventPayload, IpcEvent, ScanPhase, ScanProgress, Wrap,
};
use crate::models::{self, registry::LookupResult};
use crate::pipeline::tagging::ModelStack;
use crate::platform;
use crate::scan_session::ScanSession;

pub(crate) async fn handle_start_scan(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    scan_state: Arc<Mutex<Option<ScanCoordinator>>>,
    payload: ipc::StartScanPayload,
) {
    tracing::info!(root_path = %platform::redact_path_for_log(&payload.root_path), "[SCAN] handle_start_scan entered");

    let already_running = scan_state.lock().is_some();
    if already_running {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "scan_already_running".into(),
            message: "A scan is already running. Cancel it before starting a new one.".into(),
            path: None,
            model_kind: None,
        }))))
        .await;
        tracing::warn!("[SCAN] handle_start_scan exiting: scan_already_running");
        return;
    }

    // Pre-flight before ModelStack::load_default. Without this, a user who
    // clicked Scan before completing Welcome would wedge ORT for the full
    // timeout window with no actionable feedback.
    let missing_models: Vec<&str> = ["mobileclip_s2", "arcface", "clip_text"]
        .iter()
        .filter_map(|kind| {
            let model = match models::registry::lookup_full(kind) {
                LookupResult::Found(m) => m,
                _ => return Some(*kind),
            };
            match models::registry::sentinel_path(&model) {
                Some(p) if p.exists() => None,
                _ => Some(*kind),
            }
        })
        .collect();
    if !missing_models.is_empty() {
        tracing::warn!(missing = ?missing_models, "[SCAN] required models missing; aborting scan");
        sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
            ScanPhase::Failed,
        ))))
        .await;
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "models_not_installed".into(),
            message: format!(
                "Install the AI models from the Welcome screen (or Settings → Local AI) before scanning. Missing: {}.",
                missing_models.join(", ")
            ),
            path: None,
            model_kind: None,
        }))))
        .await;
        tracing::warn!("[SCAN] handle_start_scan exiting: models_not_installed");
        return;
    }

    // Emit Discovering immediately so the UI flips out of IdlePanel within
    // microseconds, regardless of how long ModelStack takes to load. Use
    // .await (not try_send) so the event can't be silently dropped under
    // sink load.
    sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
        ScanPhase::Discovering,
    ))))
    .await;

    // Baseline Progress so the sidebar stats flip from "—" to "0"
    // immediately. Without it, if no file ever reaches DBWriter (empty
    // folder, all-filtered, or a downstream stall), every stat row stays
    // at "—" indefinitely — identical to "scan did nothing".
    let session_id_baseline = uuid::Uuid::new_v4().to_string();
    sink.send(IpcEvent::now(EventPayload::Progress(Wrap::new(ScanProgress {
        session_id: session_id_baseline.clone(),
        phase: ScanPhase::Discovering,
        total: 0,
        discovered: 0,
        processed: 0,
        failed: 0,
        files_per_second: 0.0,
        eta_seconds: None,
        resident_mb: 0,
        available_mb: 0,
    }))))
    .await;

    let coord = ScanCoordinator::new();
    *scan_state.lock() = Some(coord.clone());

    // Load ML model weights once per session. Heavy enough to belong on a
    // blocking thread (ORT session create can take 100-500ms per model).
    // Each model is a pool of N Sessions (one per worker slot) so ML
    // inference parallelizes across the GPU command queue instead of
    // serializing on a single Mutex.
    let models_worker_count = platform::default_worker_cap() as usize;
    let models = match tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || ModelStack::load_default(models_worker_count)),
    )
    .await
    {
        Ok(Ok(m)) => Arc::new(m),
        Ok(Err(err)) => {
            tracing::error!(?err, "model stack load panicked");
            sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
                ScanPhase::Failed,
            ))))
            .await;
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "model_load_failed".into(),
                message: format!(
                    "The inference engine couldn't load its models: {err}.\n\
                     Try reinstalling models from Settings → Local AI."
                ),
                path: None,
                model_kind: None,
            }))))
            .await;
            *scan_state.lock() = None;
            tracing::warn!("[SCAN] handle_start_scan exiting: model_load_failed");
            return;
        }
        Err(_elapsed) => {
            tracing::error!("model stack load timed out after 30s");
            sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
                ScanPhase::Failed,
            ))))
            .await;
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "model_load_timeout".into(),
                message:
                    "Loading inference models took longer than 30 seconds — \
                     a model file may be corrupted. Reinstall from Settings \
                     → Local AI."
                        .into(),
                path: None,
                model_kind: None,
            }))))
            .await;
            *scan_state.lock() = None;
            tracing::warn!("[SCAN] handle_start_scan exiting: model_load_timeout");
            return;
        }
    };

    // One banner per scan beats N per-file toasts when models are absent.
    {
        let mut missing_stages: Vec<&str> = Vec::new();
        if models.scrfd.is_none() || models.arcface.is_none() {
            missing_stages.push("face_detection");
        }
        if models.mobileclip_pool.is_none() && models.mobileclip_batch.is_none() {
            missing_stages.push("image_embedding");
        }
        if !missing_stages.is_empty() {
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "stages_skipped_missing_models".into(),
                message: format!(
                    "Some pipeline stages will be skipped this scan because their models didn't load: {}. \
                     Reinstall from Settings → Local AI to populate those features.",
                    missing_stages.join(", ")
                ),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
    }

    let worker_count = platform::default_worker_cap() as usize;
    let session = ScanSession::new_with_options(
        coord,
        db,
        worker_count,
        sink.clone(),
        models,
        payload.rescan,
    );
    let root = PathBuf::from(payload.root_path.clone());

    let scan_state_release = scan_state.clone();
    let outcome = session.run(&root, |_| {}).await;
    *scan_state_release.lock() = None;

    if let Err(err) = outcome {
        tracing::warn!(?err, root = %root.display(), "scan failed");
        sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
            ScanPhase::Failed,
        ))))
        .await;
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "scan_failed".into(),
            message: format!("Scan failed: {err}"),
            path: Some(payload.root_path),
            model_kind: None,
        }))))
        .await;
        tracing::warn!("[SCAN] handle_start_scan exiting: scan_failed");
        return;
    }

    tracing::info!("[SCAN] handle_start_scan exiting normally");
}
