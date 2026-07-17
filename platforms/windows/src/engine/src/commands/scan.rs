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

enum BlockingWait<T> {
    Completed(Result<T, tokio::task::JoinError>),
    TimedOut(tokio::task::JoinHandle<T>),
}

async fn wait_for_blocking<T: Send + 'static>(
    mut task: tokio::task::JoinHandle<T>,
    timeout: Duration,
) -> BlockingWait<T> {
    match tokio::time::timeout(timeout, &mut task).await {
        Ok(result) => BlockingWait::Completed(result),
        Err(_) => BlockingWait::TimedOut(task),
    }
}

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
    //
    // ONLY the models the SCAN pipeline actually consumes belong here. `arcface`
    // (the YuNet detector + SFace embedder bundle) drives faces; `mobileclip_s2`
    // drives per-file image embedding. `clip_text` (the CLIP *text* encoder) is
    // a QUERY-TIME-only model — it is loaded lazily on the first semantic search
    // (commands/embed.rs) and is never touched by load_default or any
    // scan→detect→embed→cluster stage. Requiring it here was a bug: a stalled or
    // incomplete clip_text install (its sentinel never written) aborted the
    // ENTIRE scan with models_not_installed, so face scanning produced zero
    // faces even though YuNet+SFace+MobileCLIP were fully installed. clip_text's
    // absence must only degrade query-time search, never block a scan.
    let missing_models: Vec<&str> = ["mobileclip_s2", "arcface"]
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

    // macOS pre-flight: the engine's `load-dynamic` ONNX Runtime build needs an
    // installed `libonnxruntime.dylib` (ORT's `download-binaries` ships only a
    // STATIC lib for arm64). If it isn't resolvable, fail fast with a distinct,
    // actionable `runtime_not_installed` error — pointing at the ONE-TIME
    // runtime install, NOT the model download — instead of wedging ORT for the
    // 120 s model-load timeout and then surfacing an opaque dlopen panic as
    // `model_load_failed`. No-op on Windows/Linux (`is_available()` is always
    // true there). main.rs already pinned `ORT_DYLIB_PATH` when resolvable.
    #[cfg(target_os = "macos")]
    if !crate::ort_runtime::is_available() {
        tracing::warn!("[SCAN] ONNX Runtime dylib not installed; aborting scan");
        sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
            ScanPhase::Failed,
        ))))
        .await;
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "runtime_not_installed".into(),
            message: crate::ort_runtime::missing_runtime_message(),
            path: None,
            model_kind: None,
        }))))
        .await;
        *scan_state.lock() = None;
        tracing::warn!("[SCAN] handle_start_scan exiting: runtime_not_installed");
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
    // is where a bad GPU pack DLL crashes the process). A timed-out blocking load
    // keeps both this guard and the scan reservation until the loader really exits.
    // Arm the override-aware EP that will actually attempt the first native
    // GPU bind (honors gpuExecutionProviderOverride), not the auto-detected
    // active_provider() which ignores the override — see runtime::armed_provider.
    //
    // The guard disarms on drop too, so a GRACEFUL shutdown that aborts this
    // task mid-load (before the `.await` returns) still clears the breadcrumb —
    // otherwise the next launch would false-poison this healthy EP. A real
    // native crash skips the drop (process gone) and keeps the breadcrumb.
    let armed_ep = models::runtime::armed_provider();
    let ep_guard = models::ep_guard::ArmGuard::arm(armed_ep.as_str());
    let load_result = wait_for_blocking(
        tokio::task::spawn_blocking(move || ModelStack::load_default(models_worker_count)),
        Duration::from_secs(120),
    )
    .await;
    let models = match load_result {
        BlockingWait::Completed(Ok(m)) => {
            drop(ep_guard);
            Arc::new(m)
        }
        BlockingWait::Completed(Err(err)) => {
            drop(ep_guard);
            tracing::error!(?err, "model stack load panicked");
            // macOS: a dylib that resolved at pre-flight but fails to load is
            // almost always the wrong architecture or older than the required
            // ONNX Runtime 1.22 (`ort` hard-panics on a lower minor version) —
            // point at the one-time runtime reinstall in addition to the models.
            #[cfg(target_os = "macos")]
            let runtime_hint = "\nOn macOS this can also mean the ONNX Runtime is the wrong \
                                architecture or older than 1.22 — reinstall it with \
                                `fileid runtime install`.";
            #[cfg(not(target_os = "macos"))]
            let runtime_hint = "";
            sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
                ScanPhase::Failed,
            ))))
            .await;
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "model_load_failed".into(),
                message: format!(
                    "The inference engine couldn't load its models: {err}.\n\
                     Try reinstalling models from Settings → Local AI.{runtime_hint}"
                ),
                path: None,
                model_kind: None,
            }))))
            .await;
            *scan_state.lock() = None;
            tracing::warn!("[SCAN] handle_start_scan exiting: model_load_failed");
            return;
        }
        BlockingWait::TimedOut(load_task) => {
            tracing::error!("model stack load timed out after 120s; retaining scan reservation until the blocking loader exits");
            sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
                ScanPhase::Failed,
            ))))
            .await;
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "model_load_timeout".into(),
                message:
                    "Loading inference models took longer than 120 seconds. The native \
                     loader cannot be cancelled safely, so FileID will reject another scan \
                     until it finishes. Restart the engine before retrying if it remains stuck.\n\nTry:\n\
                     • Close other memory- or GPU-heavy apps.\n\
                     • Reinstall the models from Settings → Local AI.\n\
                     • Pick a smaller Deep Analyze model in Settings → Local AI."
                        .into(),
                path: None,
                model_kind: None,
            }))))
            .await;
            let late_result = load_task.await;
            drop(ep_guard);
            match late_result {
                Ok(_) => tracing::warn!("[SCAN] timed-out model loader eventually exited"),
                Err(err) => tracing::warn!(?err, "[SCAN] timed-out model loader eventually failed"),
            }
            *scan_state.lock() = None;
            tracing::warn!("[SCAN] handle_start_scan exiting: model_load_timeout");
            return;
        }
    };

    // A model whose install SENTINEL passed the pre-flight above but whose
    // weights won't actually LOAD (corrupt/incomplete .onnx, AV-quarantined
    // file, or an EP bind failure on every provider) must ABORT the scan — not
    // silently skip its stage. A skipped stage still writes every file row with
    // failed=0 and a fresh scanned_at; the incremental skip-set
    // (scan_session.rs) is purely timestamp-based, so those files are NEVER
    // re-tried on later default scans — the face / image-embedding stage stays
    // permanently empty even after the model is repaired (a real "face scanning
    // is totally broken, and stays broken" trap). Aborting (mirroring the
    // missing-sentinel pre-flight) leaves the files un-stamped so a later scan
    // re-processes them once the model loads. Both checked models are pre-flight
    // requirements, so reaching here with one None means a genuine load failure.
    {
        let mut failed_to_load: Vec<&str> = Vec::new();
        if models.scrfd.is_none() || models.arcface.is_none() {
            failed_to_load.push("face detection + recognition");
        }
        if models.mobileclip_pool.is_none() && models.mobileclip_batch.is_none() {
            failed_to_load.push("image embedding");
        }
        if !failed_to_load.is_empty() {
            tracing::error!(
                models = ?failed_to_load,
                "[SCAN] installed models failed to load; aborting so files aren't stranded face-less"
            );
            sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
                ScanPhase::Failed,
            ))))
            .await;
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "model_load_failed".into(),
                message: format!(
                    "These installed AI models failed to load, so the scan was stopped: {}.\n\n\
                     The model files are likely incomplete or blocked by antivirus. \
                     Reinstall them from Settings → Local AI, then scan again. \
                     (The scan was stopped on purpose so your library isn't recorded as \
                     scanned-with-these-features-missing, which would skip those files \
                     on future scans.)",
                    failed_to_load.join(", ")
                ),
                path: None,
                model_kind: None,
            }))))
            .await;
            *scan_state.lock() = None;
            tracing::warn!("[SCAN] handle_start_scan exiting: model_load_failed (post-load)");
            return;
        }
    }

    // The single largest live allocation during a scan is the set of full
    // decoded RGB frames each in-flight worker future owns. On a low-RAM box
    // (MemoryTier::Low, <8 GB) cap the worker count so that working set stays
    // bounded; non-Low tiers keep the full CPU-topology cap unchanged. Pool
    // arrays are indexed worker_idx % pool.len(), so worker_count and pool_size
    // stay decoupled.
    let worker_count = match platform::memory_tier() {
        platform::MemoryTier::Low => platform::default_worker_cap().min(6) as usize,
        _ => platform::default_worker_cap() as usize,
    };
    let session = ScanSession::new_with_options(
        coord,
        db,
        worker_count,
        sink.clone(),
        models,
        payload.rescan,
        payload.excluded_paths.clone().unwrap_or_default(),
    );
    let root = PathBuf::from(payload.root_path.clone());

    // RAII guard: clear scan_state on normal completion OR panic so a faulting
    // task can't permanently block future scans.
    struct ScanStateGuard(Arc<Mutex<Option<ScanCoordinator>>>);
    impl Drop for ScanStateGuard {
        fn drop(&mut self) { *self.0.lock() = None; }
    }
    let _state_guard = ScanStateGuard(scan_state.clone());
    let outcome = session.run(&root, |_| {}).await;

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

/// `purgeExcluded` handler — immediately remove cataloged rows under the
/// given excluded folders (files on disk untouched). Sent when the user adds
/// an exclusion so the Library reflects it without waiting for a rescan; the
/// same purge also runs at every scan start as the durable backstop.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timed_out_blocking_load_remains_joinable() {
        let release = Arc::new(AtomicBool::new(false));
        let release_for_task = release.clone();
        let task = tokio::task::spawn_blocking(move || {
            while !release_for_task.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            7
        });

        let timed_out = wait_for_blocking(task, Duration::from_millis(10)).await;
        let BlockingWait::TimedOut(task) = timed_out else {
            panic!("blocking loader unexpectedly completed before the timeout");
        };
        assert!(!task.is_finished());
        release.store(true, Ordering::Release);
        assert_eq!(task.await.unwrap(), 7);
    }
}

pub(crate) async fn handle_purge_excluded(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    payload: ipc::PurgeExcludedPayload,
) {
    let result = tokio::task::spawn_blocking(
        move || -> anyhow::Result<crate::ipc::BulkActionResult> {
            const MAX_PATHS: usize = 4096;
            let resolved: Vec<_> = payload
                .excluded_paths
                .iter()
                .take(MAX_PATHS)
                .filter_map(|raw| {
                    crate::util::path_safety::resolve_exclusion_unrooted(std::path::Path::new(raw))
                })
                .collect();
            let purged = {
                let conn = db.lock();
                crate::scan_session::purge_excluded_rows(&conn, &resolved)
            };
            Ok(crate::ipc::BulkActionResult {
                action: "purgeExcluded".into(),
                succeeded: purged.min(u32::MAX as u64) as u32,
                failed: 0,
                messages: Vec::new(),
            })
        },
    )
    .await;
    crate::commands::bulk::emit_bulk_result(&sink, "purgeExcluded", result).await;
}
