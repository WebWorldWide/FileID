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

    // Atomic check-and-reserve under one lock: gate + slot reservation must
    // be a single critical section, or two StartScan commands queued back-to-
    // back can both pass the gate before either reserves the slot. Construct
    // the REAL coordinator now and park its clone, so a pause/cancel arriving
    // after reservation lands on the coordinator the session actually uses —
    // not a throwaway placeholder that the old code swapped out post-model-load
    // (#20). If anyone else reaches here while we hold the slot, they bounce
    // with `scan_already_running`.
    let coord = ScanCoordinator::new();
    let already_running = {
        let mut guard = scan_state.lock();
        if guard.is_some() {
            true
        } else {
            *guard = Some(coord.clone());
            false
        }
    };
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
                LookupResult::Unknown => return Some(*kind),
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
        *scan_state.lock() = None;
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

    // (The real coordinator was reserved into scan_state above, before the
    // first .await — no placeholder swap, so no pause/cancel can be lost #20.)

    // Load ML model weights once per session. Heavy enough to belong on a
    // blocking thread (ORT session create can take 100-500ms per model).
    // Each model is a pool of N Sessions (one per worker slot) so ML
    // inference parallelizes across the GPU command queue instead of
    // serializing on a single Mutex.
    //
    // Timeout is 120 s (not 30 s): on the FIRST launch the CLIP scene-label
    // matrix is text-encoded inside load_default, which on a slow EP (DirectML
    // / CPU) takes 20+ s on its own — a 30 s budget tripped a false "model file
    // corrupted" on real 4 GB-VRAM hardware. The matrix is disk-cached after the
    // first build (scene_vocab.rs), so later launches load in a few seconds;
    // 120 s still catches a genuinely hung or corrupt model file.
    let models_worker_count = platform::default_worker_cap() as usize;
    // EP crash-safety: arm a breadcrumb around the first ORT session bind (this
    // is where a bad GPU pack DLL crashes the process). If we get past the
    // `.await` at all — success, Rust error, or timeout — the process survived,
    // so disarm; only a hard native crash leaves the breadcrumb for the next
    // launch's ep_guard to disable the EP. See models::ep_guard.
    // Arm the override-aware EP that will actually attempt the first native
    // GPU bind (honors gpuExecutionProviderOverride), not the auto-detected
    // active_provider() which ignores the override — see runtime::armed_provider.
    let armed_ep = models::runtime::armed_provider();
    models::ep_guard::arm(armed_ep.as_str());
    let load_result = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::task::spawn_blocking(move || ModelStack::load_default(models_worker_count)),
    )
    .await;
    models::ep_guard::disarm();
    let models = match load_result {
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
            tracing::error!("model stack load timed out after 120s");
            sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
                ScanPhase::Failed,
            ))))
            .await;
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "model_load_timeout".into(),
                message:
                    "Loading inference models took longer than 120 seconds — \
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
        tracing::warn!(?err, root = %platform::redact_path_for_log(&root), "scan failed");
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
