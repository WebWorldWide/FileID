use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::coordinator::ScanCoordinator;
use crate::ipc::{
    self, sink::Sink, EngineError, EventPayload, IpcEvent, ScanPhase, ScanProgress, Wrap,
};
use crate::models;
use crate::pipeline::scan_models::{self, ModelLoadFailure, ModelSource};
use crate::pipeline::tagging::ModelStack;
use crate::platform;
use crate::scan_session::ScanSession;

enum BlockingWait<T> {
    Completed(Result<T, tokio::task::JoinError>),
    TimedOut(tokio::task::JoinHandle<T>),
}

struct ScanStateGuard(Arc<Mutex<Option<ScanCoordinator>>>, ScanCoordinator);
impl Drop for ScanStateGuard {
    fn drop(&mut self) {
        clear_scan_slot(&self.0, &self.1);
        self.1.release_slot();
    }
}

fn clear_scan_slot(state: &Mutex<Option<ScanCoordinator>>, coord: &ScanCoordinator) {
    let mut guard = state.lock();
    if guard.as_ref().is_some_and(|c| c.same_scan(coord)) {
        *guard = None;
    }
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

async fn reject_stranded_decoders(
    sink: &Sink,
    scan_state: &Arc<Mutex<Option<ScanCoordinator>>>,
    active: usize,
) {
    tracing::warn!(active, "[SCAN] decoder workers from the previous scan are still stopping");
    sink.send(EngineError::event(
        "decoder_shutdown_incomplete",
        "FileID is still finishing the last scan. Wait a moment and try again; if it keeps happening, quit and reopen FileID.",
    ))
    .await;
    sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
        ScanPhase::Failed,
    ))))
    .await;
    *scan_state.lock() = None;
}

async fn reject_gpu_device_removed(
    sink: &Sink,
    scan_state: &Arc<Mutex<Option<ScanCoordinator>>>,
) {
    sink.send(EngineError::event(
        "gpu_device_removed",
        crate::coordinator::GPU_DEVICE_REMOVED_MESSAGE,
    ))
    .await;
    sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
        ScanPhase::Failed,
    ))))
    .await;
    *scan_state.lock() = None;
}

type LateLoader = Arc<Mutex<Option<tokio::task::JoinHandle<ModelStack>>>>;

/// Each model is a pool of N sessions sharing one compiled graph, so inference parallelizes instead of serializing on
/// one Mutex; loads on a blocking thread. The 120 s timeout: on first launch the CLIP scene-label matrix is
/// text-encoded inside load_default (20+ s on a slow CPU), cached by scene_vocab.rs; 120 s still catches a hung or
/// corrupt model file.
async fn load_scan_models(
    coord: ScanCoordinator,
    key: scan_models::StackKey,
    worker_count: usize,
    missing_models: Vec<&'static str>,
    late_loader: LateLoader,
    sink: Sink,
) -> Result<Arc<ModelStack>, ModelLoadFailure> {
    if let Some(stack) = scan_models::resident(&key) {
        tracing::info!("[SCAN] models are still loaded from the last scan");
        return Ok(stack);
    }
    let started = std::time::Instant::now();
    let load_result = wait_for_blocking(
        tokio::task::spawn_blocking(move || ModelStack::load_for_scan(worker_count)),
        Duration::from_secs(120),
    )
    .await;
    let models = match load_result {
        BlockingWait::Completed(Ok(m)) => Arc::new(m),
        BlockingWait::Completed(Err(err)) => {
            tracing::error!(?err, "model stack load panicked");
            return Err(ModelLoadFailure::new(
                "model_load_failed",
                "FileID couldn't load its AI models. Reinstall them in Settings, under Local AI.",
            ));
        }
        BlockingWait::TimedOut(load_task) => {
            tracing::error!("model stack load timed out after 120s; retaining scan reservation until the blocking loader exits");
            *late_loader.lock() = Some(load_task);
            return Err(ModelLoadFailure::new(
                "model_load_timeout",
                "FileID's AI models took more than two minutes to load. FileID can't start \
                     another scan until they finish loading; if it stays stuck, quit and reopen FileID.\n\nTry:\n\
                     • Close other apps that use a lot of memory or graphics power.\n\
                     • Reinstall the models in Settings, under Local AI.\n\
                     • Pick a smaller model for Describe Files.",
            ));
        }
    };
    tracing::info!(seconds = started.elapsed().as_secs_f64(), "[SCAN] models loaded");

    if coord.is_gpu_dead() {
        return Err(ModelLoadFailure::new("gpu_device_removed", crate::coordinator::GPU_DEVICE_REMOVED_MESSAGE));
    }

    // An installed model whose weights fail to load (corrupt or AV-quarantined .onnx, or a graph tract cannot compile)
    // must abort the scan, not skip its stage: a skipped stage still stamps every file failed=0 with a fresh
    // scanned_at, the incremental skip-set (scan_session.rs) never retries them, and the face/embedding stage stays
    // empty even after repair. Aborting leaves files un-stamped for a later scan. Workers wait for this result, so no
    // file is written without its stages.
    let failed_to_load = models_that_failed_to_load(&models, &missing_models);
    if !failed_to_load.is_empty() {
        tracing::error!(
            models = ?failed_to_load,
            "[SCAN] installed models failed to load; aborting so files aren't stranded face-less"
        );
        return Err(ModelLoadFailure::new(
            "model_load_failed",
            format!(
                "These AI models couldn't be loaded, so the scan was stopped: {}.\n\n\
                 Their files are probably incomplete or blocked by antivirus. \
                 Reinstall them in Settings, under Local AI, then scan again. \
                 (FileID stopped the scan so your files aren't marked as done \
                 without these features.)",
                failed_to_load
                    .iter()
                    .map(|model| if *model == "image embedding" { "picture search" } else { "faces" })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    scan_models::keep(key, models.clone());
    if let Some(notice) = slower_path_notice(&models, &missing_models) {
        sink.send(notice).await;
    }
    Ok(models)
}

/// On the Mac, picture search runs on the graphics chip through MLX. When that cannot start (its Metal library is
/// missing or unloadable) the scan falls back to the CPU and takes about eleven times as long, which should not be silent.
fn slower_path_notice(models: &ModelStack, missing_models: &[&str]) -> Option<IpcEvent> {
    (models.apple_scan && !models.apple_clip && !missing_models.contains(&"mobileclip_s2")).then(|| {
        EngineError::event(
            "scan_slower_path",
            "Picture search can't use this Mac's graphics chip, so this scan will take much longer than usual. \
             Reinstalling FileID usually fixes it.",
        )
    })
}

fn models_that_failed_to_load(models: &ModelStack, missing_models: &[&str]) -> Vec<&'static str> {
    let mut failed_to_load = Vec::new();
    if (models.scrfd.is_none() || models.arcface.is_none()) && !missing_models.contains(&"arcface") && !models.apple_scan {
        failed_to_load.push("face detection + recognition");
    }
    if models.mobileclip_pool.is_none()
        && models.mobileclip_batch.is_none()
        && !models.apple_clip
        && !missing_models.contains(&"mobileclip_s2")
    {
        failed_to_load.push("image embedding");
    }
    failed_to_load
}

fn is_usable_scan_root(path: &str) -> bool {
    Path::new(path).is_absolute() && !path.contains('\0')
}

pub(crate) async fn handle_start_scan(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    scan_state: Arc<Mutex<Option<ScanCoordinator>>>,
    payload: ipc::StartScanPayload,
    cancel_requested: Arc<std::sync::atomic::AtomicBool>,
    slot: Option<Box<dyn Send>>,
    other_heavy_work: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
) -> bool {
    tracing::info!(root_path = %platform::redact_path_for_log(&payload.root_path), "[SCAN] handle_start_scan entered");

    // A relative or empty path would resolve against the engine's own folder and a NUL would cut it short: either scans
    // a folder the person never chose.
    if !is_usable_scan_root(&payload.root_path) {
        sink.send(EngineError::event("folder_access_denied", super::folder_access::SCAN_DENIED)).await;
        sink.send(IpcEvent::now(EventPayload::ScanComplete(Wrap::new(ipc::ScanComplete {
            session_id: String::new(),
            total_files: 0,
            processed_files: 0,
            failed_files: 0,
            total_seconds: 0.0,
        }))))
        .await;
        return false;
    }

    // Atomic check-and-reserve under one lock, or two back-to-back StartScan commands can both pass the gate. The REAL
    // coordinator is built and parked here so a pause/cancel after reservation lands on the one the session uses. A
    // caller finding the slot held bounces with `scan_already_running`.
    let coord = ScanCoordinator::new();
    let finished_scan = coord.clone();
    let after_scan_db = db.clone();
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
        sink.send(EngineError::event(
            "scan_already_running",
            "A scan is already running. Stop it first, or wait for it to finish.",
        ))
        .await;
        tracing::warn!("[SCAN] handle_start_scan exiting: scan_already_running");
        return false;
    }
    let _state_guard = ScanStateGuard(scan_state.clone(), coord.clone());
    {
        let state = scan_state.clone();
        let this = coord.clone();
        coord.on_release(move || {
            clear_scan_slot(&state, &this);
            drop(slot);
        });
    }

    let Some(_root_access) =
        super::folder_access::scan_access(&payload.root_path, payload.root_bookmark.as_deref())
    else {
        sink.send(EngineError::event("folder_access_denied", super::folder_access::SCAN_DENIED)).await;
        coord.release_slot();
        sink.send(IpcEvent::now(EventPayload::ScanComplete(Wrap::new(ipc::ScanComplete {
            session_id: String::new(),
            total_files: 0,
            processed_files: 0,
            failed_files: 0,
            total_seconds: 0.0,
        }))))
        .await;
        return false;
    };

    if cancel_requested.load(std::sync::atomic::Ordering::Acquire) {
        coord.request_cancel();
        coord.release_slot();
        sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
            ScanPhase::Cancelled,
        ))))
        .await;
        *scan_state.lock() = None;
        tracing::info!("[SCAN] queued scan cancelled before model loading");
        return false;
    }

    let active_decoders = crate::pipeline::tagging::active_decoder_threads();
    if active_decoders > 0 {
        reject_stranded_decoders(&sink, &scan_state, active_decoders).await;
        tracing::warn!(active_decoders, "[SCAN] previous decoder workers are still active");
        return false;
    }

    if coord.is_gpu_dead() {
        reject_gpu_device_removed(&sink, &scan_state).await;
        tracing::warn!("[SCAN] handle_start_scan exiting: process GPU failure is latched");
        return false;
    }

    // Missing models never block a scan: every file is still catalogued (kind, size, dates, hashes, text) and only
    // stages whose weights are absent skip, each logging once at load. A later rescan fills in embeddings and faces.
    let missing_models: Vec<&str> = models::required::missing_required()
        .into_iter()
        .map(|missing| missing.kind)
        .collect();
    if !missing_models.is_empty() {
        tracing::warn!(missing = ?missing_models, "[SCAN] required models missing; their stages will skip");
    }
    // Emit Discovering immediately so the UI leaves IdlePanel however long ModelStack takes; .await, not try_send, so
    // it can't be dropped under sink load.
    sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
        ScanPhase::Discovering,
    ))))
    .await;

    // Baseline Progress so sidebar stats flip from "—" to "0"; otherwise a scan where no file reaches DBWriter (empty
    // folder, all filtered, downstream stall) looks like it did nothing.
    let session_id = uuid::Uuid::new_v4().to_string();
    sink.send(IpcEvent::now(EventPayload::Progress(Wrap::new(ScanProgress {
        session_id: session_id.clone(),
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

    // Models load when the scan first has a file to analyse and stay loaded between scans (pipeline/scan_models.rs); a
    // rescan that finds nothing new loads nothing.
    let models_worker_count = platform::default_worker_cap() as usize;
    let stack_key = scan_models::StackKey::current(models_worker_count);
    let evictions_before = scan_models::evictions();
    let late_loader: LateLoader = Arc::new(Mutex::new(None));
    let models = ModelSource::lazy({
        let (coord, key, late, sink) = (coord.clone(), stack_key.clone(), late_loader.clone(), sink.clone());
        let missing: Vec<&'static str> = missing_models.clone();
        move || -> scan_models::LoadFuture {
            Box::pin(load_scan_models(coord, key, models_worker_count, missing, late, sink))
        }
    });

    // The largest live allocation is the full decoded RGB frames each in-flight worker owns: on a low-RAM box
    // (MemoryTier::Low, <8 GB) cap the worker count to bound it. Pool arrays are indexed worker_idx % pool.len(), so
    // worker_count and pool_size stay decoupled.
    let worker_count = match platform::memory_tier() {
        platform::MemoryTier::Low => platform::default_worker_cap().min(6) as usize,
        _ => platform::default_worker_cap() as usize,
    };
    let session = ScanSession::new(
        coord.clone(),
        db.clone(),
        worker_count,
        sink.clone(),
        models.clone(),
        payload.rescan,
        payload.excluded_paths.clone().unwrap_or_default(),
    )
    .with_session_id(session_id.clone())
    .with_root_display(payload.root_display.clone());
    let root = PathBuf::from(payload.root_path.clone());

    let models_for_keep = models.clone();
    let mut outcome = session.run(&root, |_| {}).await;
    let mut round = 0;
    while let Ok(previous) = &outcome
        && previous.needs_follow_up
    {
        round += 1;
        if previous.timed_out > 0 {
            // Files a busy machine timed out fail again straight away; let it settle first.
            let deadline = tokio::time::Instant::now() + crate::scan_session::timeout_retry_backoff(round);
            while tokio::time::Instant::now() < deadline && !coord.is_cancelled() {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
        outcome = ScanSession::new(
            coord.clone(),
            db.clone(),
            worker_count,
            sink.clone(),
            models.clone(),
            false,
            payload.excluded_paths.clone().unwrap_or_default(),
        )
        .with_session_id(session_id.clone())
        .with_root_display(payload.root_display.clone())
        .follow_up(previous, round)
        .run(&root, |_| {})
        .await;
    }
    let result = match outcome {
        Ok(result) => result,
        Err(err) if err.downcast_ref::<ModelLoadFailure>().is_some() => {
            let failure = err.downcast_ref::<ModelLoadFailure>().expect("checked by the guard").clone();
            tracing::warn!(kind = failure.kind, "[SCAN] handle_start_scan exiting: the models did not load");
            if failure.kind == "gpu_device_removed" {
                reject_gpu_device_removed(&sink, &scan_state).await;
            } else {
                sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(ScanPhase::Failed)))).await;
                sink.send(EngineError::event(failure.kind, failure.message)).await;
            }
            // A loader that outlived its two minutes keeps the scan slot until it really exits.
            let late = late_loader.lock().take();
            if let Some(load_task) = late {
                let late_result = load_task.await;
                match late_result {
                    Ok(_) => tracing::warn!("[SCAN] timed-out model loader eventually exited"),
                    Err(err) => tracing::warn!(?err, "[SCAN] timed-out model loader eventually failed"),
                }
            }
            *scan_state.lock() = None;
            return false;
        }
        Err(err) => {
            tracing::warn!(?err, root = %platform::redact_path_for_log(&root), "scan failed");
            sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
                ScanPhase::Failed,
            ))))
            .await;
            sink.send(EngineError::new(
                "scan_failed",
                "FileID couldn't finish scanning this folder. Check that it is still connected, then scan again.",
            )
            .with_path(payload.root_path)
            .into_event())
            .await;
            tracing::warn!("[SCAN] handle_start_scan exiting: scan_failed");
            return false;
        }
    };

    // Keep the stack for the next scan unless another model asked for the memory meanwhile.
    if let Some(stack) = models_for_keep.loaded()
        && scan_models::evictions() == evictions_before
    {
        scan_models::keep(stack_key, stack);
    }

    tracing::info!(
        processed = result.processed,
        failed = result.failed,
        elapsed_seconds = result.elapsed_seconds,
        "[SCAN] handle_start_scan exiting normally"
    );

    if let Some(busy) = other_heavy_work
        && !finished_scan.is_cancelled()
        && !finished_scan.is_gpu_dead()
        && finished_scan.media_analyzed() > 0
    {
        super::take_grouping::start_after_scan(sink, after_scan_db, busy);
    }
    !finished_scan.is_cancelled() && !finished_scan.is_gpu_dead()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn only_the_mac_scan_that_lost_the_graphics_chip_says_it_is_slower() {
        let mut stack = ModelStack::empty();
        assert!(slower_path_notice(&stack, &[]).is_none(), "not the Mac scan path");
        stack.apple_scan = true;
        assert!(slower_path_notice(&stack, &[]).is_some());
        assert!(slower_path_notice(&stack, &["mobileclip_s2"]).is_none(), "no picture model to run at all");
        stack.apple_clip = true;
        assert!(slower_path_notice(&stack, &[]).is_none());
    }

    #[test]
    fn a_scan_root_must_be_absolute_and_free_of_nul() {
        assert!(is_usable_scan_root(if cfg!(windows) { r"C:\photos" } else { "/photos" }));
        assert!(!is_usable_scan_root(""));
        assert!(!is_usable_scan_root("photos"));
        assert!(!is_usable_scan_root("./photos"));
        assert!(!is_usable_scan_root("../photos"));
        assert!(!is_usable_scan_root(if cfg!(windows) { "C:\\photos\0x" } else { "/photos\0x" }));
    }

    #[test]
    fn scan_state_guard_clears_a_preflight_panic() {
        let coord = ScanCoordinator::new();
        let scan_state = Arc::new(Mutex::new(Some(coord.clone())));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let scan_state = scan_state.clone();
            move || {
                let _guard = ScanStateGuard(scan_state, coord);
                panic!("injected preflight panic");
            }
        }));
        assert!(result.is_err());
        assert!(scan_state.lock().is_none());
    }

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

    #[tokio::test]
    async fn preset_pending_cancel_stops_scan_before_model_loading() {
        let db = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
        let scan_state = Arc::new(Mutex::new(None));
        let cancel_requested = Arc::new(AtomicBool::new(true));
        let (sink, mut events) = Sink::channel_for_test(2);
        let payload = ipc::StartScanPayload {
            root_path: std::env::temp_dir().to_string_lossy().into_owned(),
            root_bookmark: None,
            root_display: None,
            rescan: false,
            excluded_paths: None,
        };

        handle_start_scan(
            sink,
            db,
            scan_state.clone(),
            payload,
            cancel_requested,
            None,
            None,
        )
        .await;

        let event = events.recv().await.expect("cancelled phase event");
        assert!(matches!(
            event.payload,
            EventPayload::PhaseChanged(Wrap {
                inner: ScanPhase::Cancelled
            })
        ));
        assert!(scan_state.lock().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_scan_is_one_session_and_one_discovering_phase() {
        let folder = std::env::temp_dir().join(format!("fileid-one-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        for name in ["a.txt", "b.txt", "sub/c.txt"] {
            std::fs::write(folder.join(name), name).unwrap();
        }
        let db = Arc::new(Mutex::new(crate::db::test_support::memory_db()));
        let scan_state = Arc::new(Mutex::new(None));
        let (sink, mut events) = Sink::channel_for_test(4096);
        let payload = ipc::StartScanPayload {
            root_path: folder.to_string_lossy().into_owned(),
            root_bookmark: None,
            root_display: None,
            rescan: false,
            excluded_paths: None,
        };

        let completed = handle_start_scan(sink, db, scan_state, payload, Arc::new(AtomicBool::new(false)), None, None).await;
        assert!(completed);

        let mut discovering = 0;
        let mut session_ids = std::collections::BTreeSet::new();
        let mut progress: Vec<ScanProgress> = Vec::new();
        let mut completed_session = None;
        while let Ok(event) = events.try_recv() {
            match event.payload {
                EventPayload::PhaseChanged(Wrap { inner: ScanPhase::Discovering }) => discovering += 1,
                EventPayload::Progress(Wrap { inner }) => {
                    session_ids.insert(inner.session_id.clone());
                    progress.push(inner);
                }
                EventPayload::ScanComplete(Wrap { inner }) => {
                    session_ids.insert(inner.session_id.clone());
                    completed_session = Some(inner.session_id);
                }
                _ => {}
            }
        }
        let _ = std::fs::remove_dir_all(&folder);

        assert_eq!(discovering, 1, "phaseChanged(discovering) once per scan");
        assert!(completed_session.is_some(), "scanComplete was sent");
        assert_eq!(session_ids.len(), 1, "one session id for every event of the scan: {session_ids:?}");
        assert!(!progress.is_empty());
        let mut floor = (0, 0);
        for p in &progress {
            assert!(p.total >= floor.0 && p.processed >= floor.1, "counters only grow: {floor:?} then ({}, {})", p.total, p.processed);
            floor = (p.total, p.processed);
        }
        assert_eq!(progress.last().unwrap().phase, ScanPhase::Completed);
    }

    #[tokio::test]
    async fn gpu_failure_rejection_is_terminal_and_releases_scan_slot() {
        let coordinator = ScanCoordinator::new();
        let scan_state = Arc::new(Mutex::new(Some(coordinator)));
        let (sink, mut events) = Sink::channel_for_test(2);

        reject_gpu_device_removed(&sink, &scan_state).await;

        let error = events.recv().await.expect("GPU error event");
        let phase = events.recv().await.expect("failed phase event");
        match error.payload {
            EventPayload::Error(error) => {
                assert_eq!(error.inner.kind, "gpu_device_removed");
                assert_eq!(error.inner.message, crate::coordinator::GPU_DEVICE_REMOVED_MESSAGE);
            }
            other => panic!("expected GPU error, got {other:?}"),
        }
        assert!(matches!(
            phase.payload,
            EventPayload::PhaseChanged(Wrap { inner: ScanPhase::Failed })
        ));
        assert!(scan_state.lock().is_none());
    }
}

/// Removes cataloged rows under the excluded folders; files on disk are untouched. Sent when the user adds an exclusion, and the same purge runs at every scan start as the durable backstop.
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
                command_id: None,
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
