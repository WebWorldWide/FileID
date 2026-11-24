//! FileIDEngine stdio server, spawned by the FileID app as a child: reads `IPCCommand` JSON frames from stdin (one per line) and emits `IPCEvent` frames to stdout. Local-only logs go to %LOCALAPPDATA%/FileID/logs/ via tracing; nothing leaves the machine.
//!
//! Parent exit or stdin EOF cancels active work, quiesces mutations, checkpoints the WAL when safe, flushes the sink, and exits.

use crate::{
    catalog_index, commands, coordinator, db, job_queue, logging, models, paths, pipeline,
    platform,
};

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::io::BufReader;
use tokio::sync::Notify;

use crate::ipc::{
    self,
    bounded_read::{self, BoundedRead},
    sink::Sink,
    CommandPayload, EngineError, EventPayload, IpcCommand, IpcEvent, JobCategory, ScanPhase, Wrap,
};

const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Max inbound IPC frame, 64 MiB, symmetric with the app's outbound command cap (MaxIpcFrameBytes) and inbound read cap (MaxFrameBytes). A large applyRestructure carries the multi-MB move set the engine emitted in restructurePlan (~200k moves). Still bounded against a runaway line.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Human-readable rejection for an oversized inbound frame, derived from MAX_FRAME_BYTES so the text can't drift from the real limit.
fn oversized_frame_message(seen: usize) -> String {
    format!(
        "Command frame exceeded {} MiB cap (saw {} bytes before reject).",
        MAX_FRAME_BYTES / (1024 * 1024),
        seen,
    )
}

pub fn run() -> Result<()> {
    // SEC-3: lock the DLL search FIRST, before tokio spins worker threads, before logging::init() opens file handles, and before anything else that might implicitly LoadLibrary. The default Windows search includes CWD + every PATH entry, so a dropped DLL there gets code exec when a native runtime later loads. SetDefaultDllDirectories restricts to System32 + the engine's directory + AddDllDirectory-registered runtime dirs.
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::LibraryLoader::{
            SetDefaultDllDirectories, LOAD_LIBRARY_FLAGS,
        };
        const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
        const LOAD_LIBRARY_SEARCH_APPLICATION_DIR: u32 = 0x200;
        const LOAD_LIBRARY_SEARCH_USER_DIRS: u32 = 0x400;
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_FLAGS(
            LOAD_LIBRARY_SEARCH_SYSTEM32
                | LOAD_LIBRARY_SEARCH_APPLICATION_DIR
                | LOAD_LIBRARY_SEARCH_USER_DIRS,
        ));
    }

    // Suppress hard-error dialogs process-wide (headless engine): a llama.cpp binary loading against a missing/mismatched CUDA DLL would pop a modal box that blocks Deep Analyze forever. Children inherit the mode. No-op on non-Windows (called unconditionally so the stub isn't dead code).
    platform::suppress_hard_error_dialogs();

    // Pin the COM MTA for the process lifetime. Shell/WinRT call sites (OCR, HEIC, video) CoInitializeEx/CoUninitialize around their work on blocking-pool threads; when the LAST uninit dropped the process MTA to zero, Windows unloaded the WinRT stack while the `windows` crate's process-wide factory cache still held pointers into it, and the next OCR call died with a native 0xC0000005 (minidump: fault inside unloaded WinTypes.dll; reproduced 4× mid-scan on a 71k-file corpus). CoIncrementMTAUsage keeps the MTA, loaded WinRT DLLs and cached factories alive without initializing this thread's apartment. The cookie is intentionally leaked: decrementing at exit would only re-open the teardown race.
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Com::CoIncrementMTAUsage;
        // The cookie is only for CoDecrementMTAUsage, which we never call; dropping it does not release the pin.
        if let Err(e) = CoIncrementMTAUsage() {
            eprintln!("CoIncrementMTAUsage failed: {e} — WinRT unload race unmitigated");
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(async_main());
    // Abandon still-running spawn_blocking tasks (e.g. the parent watchdog's INFINITE wait); the OS reaps them, and otherwise they'd hold the runtime drop and block process exit.
    rt.shutdown_timeout(std::time::Duration::from_millis(0));
    result
}

async fn async_main() -> Result<()> {
    logging::init()?;
    let _ = paths::ensure_state_dirs()?; // create %LOCALAPPDATA%/FileID/{logs,Models,...}
    logging::install_panic_hook();

    tracing::info!(version = ENGINE_VERSION, "FileIDEngine starting");
    tracing::info!(backend = models::runtime::inference_backend(), "[ML] inference backend");

    // VRAM probe at startup; llama.cpp launch tuning sizes its GPU offload from it.
    if let Some(vram_mb) = platform::dedicated_vram_mb() {
        tracing::info!(dedicated_vram_mb = vram_mb, "[VRAM] dedicated video memory probed");
    } else {
        tracing::info!("[VRAM] no dedicated GPU detected");
    }

    // Replay AddDllDirectory for llama.cpp runtimes extracted by a prior install (Models\llama.cpp\, Models\llama.cpp-cuda\): after SEC-3 locked the default search path those dirs are invisible until added, so without this they only work on the install run.
    if let Ok(models_dir) = paths::models_dir() {
        let _ = platform::register_dll_dirs_under(&models_dir.join("llama.cpp"));
        let _ = platform::register_dll_dirs_under(&models_dir.join("llama.cpp-cuda"));
        if let Ok(executable) = std::env::current_exe()
            && let Some(application_dir) = executable.parent() {
                let bundled_runtimes = application_dir.join("RuntimeBundles");
                let _ = platform::register_dll_dirs_under(&bundled_runtimes.join("llama.cpp"));
            }
    }

    // Open the DB up front so migrations apply and any failure surfaces before we tell the app we're ready. Shared as Arc<Mutex<…>> so handlers use the single writer; checkpointed and closed on shutdown.
    let db_path = paths::db_path()?;
    let (db_conn, db_open_error): (Option<std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>>, _) =
        match db::open_writer(&db_path) {
            Ok(c) => (Some(std::sync::Arc::new(parking_lot::Mutex::new(c))), None),
            Err(err) => {
                tracing::error!(?err, path = %platform::redact_path_for_log(&db_path), "failed to open database");
                (None, Some(err))
            }
        };

    // Consume any restructure path-update recovery record left by an apply that moved a file but couldn't update its DB row (a live UNIQUE conflict, or a kill in the move→update window). Realigns the stale path_text before the app reads the library so tags aren't stranded when rename-heal can't fire (exFAT/network volumes with no file_ref). Runs even if no scan follows.
    if let Some(db) = db_conn.as_ref() {
        pipeline::restructure_apply::reconcile_pending_path_updates(db);
    }
    let reclaimed = commands::trash_log::recover_at_startup(db_conn.as_ref());
    if reclaimed > 0 {
        tracing::warn!(reclaimed, "put back files a killed trashFiles left under a claim name");
    }

    let (sink, sink_writer) = Sink::spawn();

    if let Some(err) = &db_open_error {
        sink.send(db_open_error_event(err, &db_path)).await;
        tracing::error!("engine starting without a writable DB — aborting");
        // Drop the sink so the writer task sees EOF, then await it (with a cap) so the error reaches the app before exit.
        drop(sink);
        let _ = tokio::time::timeout(Duration::from_secs(2), sink_writer).await;
        return Ok(());
    }

    // Emit `ready` first so the sidebar can leave .starting; the handshake is one-way.
    commands::hardware::emit_ready(&sink).await;

    // The first search after launch should not wait for the CLIP text model; a few seconds' grace keeps launch itself quick.
    if let Some(db) = db_conn.clone() {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let _ = tokio::task::spawn_blocking(move || commands::embed::prewarm_clip_text(&db)).await;
        });
    }

    // Structural integrity check after `ready`, on its own read-only connection so a large library never delays launch. A torn page or truncated file (power loss mid-checkpoint, failing disk) would otherwise surface as opaque per-query failures. Non-fatal: the engine keeps running so the user can wipe + rescan.
    let integrity_check = Arc::new(db::IntegrityCheck::default());
    if db_conn.is_some() {
        let check = integrity_check.clone();
        let sink = sink.clone();
        let db_path = db_path.clone();
        tokio::spawn(async move {
            let path = db_path.clone();
            let Ok(Some(detail)) = tokio::task::spawn_blocking(move || check.run(&path)).await else {
                return;
            };
            tracing::error!(%detail, "database failed PRAGMA quick_check");
            sink.send(EngineError::new(
                "db_integrity_check_failed",
                "FileID's library file looks damaged. Open Settings and choose Start over, then scan again to rebuild it.",
            )
            .with_path(db_path.display().to_string())
            .into_event())
            .await;
        });
    } else {
        integrity_check.skip();
    }

    // Coordinated shutdown signal, awaited by the stdio loop and the parent watchdog.
    let shutdown = Arc::new(Notify::new());

    // Register the main-task waiter NOW, before any task that could `notify_waiters()` is spawned. A pinned-but-unpolled `Notified` is NOT a waiter, so an early notify_waiters() on empty-stdin EOF would be lost and the engine would hang on .notified().await.
    let main_shutdown = shutdown.notified();
    tokio::pin!(main_shutdown);
    main_shutdown.as_mut().enable();

    // Parent watchdog: waits on a handle to the parent. The parent's IDENTITY is pinned by an OpenProcess handle + creation time captured at startup, NOT the bare PID, which an unrelated process can reuse the instant the real parent exits (TOCTOU).
    let parent_pid = platform::get_parent_pid();
    if let Some(ppid) = parent_pid {
        let s = shutdown.clone();
        tokio::spawn(async move {
            watch_parent_pinned(ppid, s).await;
        });
    } else {
        tracing::warn!("could not determine parent PID; running without watchdog");
    }

    // Stdio loop. SEC: not `BufReader::lines()`, whose `next_line()` buffers the whole line so a hostile no-newline blob could OOM the engine before any cap fires. `read_until(b'\n', ...)` into an in-loop Vec rejects anything crossing MAX_FRAME_BYTES mid-read.
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);

    let command_shutdown_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let catalog_index = db_conn.as_ref().map(|writer| catalog_index::CatalogIndex::new(writer.clone(), db_path.clone(), db_path.with_extension("visual-index")));
    if let Some(writer) = db_conn.as_ref()
        && let Err(error) = commands::timeline::recover(&writer.lock()) {
            tracing::warn!(?error, "timeline job recovery failed");
        }

    // Active scan coordinator: None when idle; set by StartScan, consulted by PauseScan / ResumeScan / CancelScan.
    let scan_state: Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>> =
        Arc::new(parking_lot::Mutex::new(None));
    // Reserve scan admission before the task waits on the mutation gate; the coordinator is installed by handle_start_scan right after gate acquisition, and this flag rejects duplicate queued scans.
    let scan_active: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));
    // CancelScan/wipe set this even when a reserved scan hasn't reached the coordinator slot, so cancellation can't be lost in that queue gap.
    let scan_cancel_requested: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Deep Analyze cancel flag: single-in-flight; deepAnalyzeCancel sets it and the per-file loop polls it between files and during the VLM caption call.
    let deep_analyze_cancel: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Single-in-flight gate: a second Deep Analyze command must bounce cleanly rather than re-arm the shared cancel flag or cross-cancel the running pass. Only a successful acquire resets `cancel`.
    let deep_analyze_active: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Single-flight gate for face clustering: the writer lock is released during cluster()+consolidate(), so a second runFaceClustering must bounce rather than race the persist transaction.
    let face_cluster_active: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));
    let restructure_cancellations = RestructureCancelRegistry::default();
    // Single-flight for apply/undo: overlapping runs would race on-disk moves and the undo journal. Same compare-exchange + RAII-guard shape as Deep Analyze.
    let restructure_active: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Every filesystem/DB mutation is serialized. Wipe requests cancellation before taking the same permit, so it sees a fully quiescent library or fails without deleting anything.
    let mutation_gate = Arc::new(tokio::sync::Mutex::new(()));

    // Sidebar job tracker (macOS JobQueue parity): scan / cluster / deep-analyze jobs push on accept and finish via RAII guards, each transition emitting a queueState event for the app's SidebarQueueList.
    let jobs = job_queue::JobQueue::new();
    jobs.forward_to(sink.clone());

    // Shared HTTP client (HTTP/2 + connection pool) for the parallel downloader, built once and cloned via Arc into every prewarm task.
    let http_client = match crate::downloader::build_shared_client() {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(?err, "failed to build shared HTTP client; downloads will fail");
            // Fail CLOSED: a trust-store-empty client refuses every TLS handshake, so a broken pinned-client build can never degrade into unpinned egress. The builder does no I/O and can't fail here.
            crate::downloader::fail_closed_client()
        }
    };

    // Prewarm cancellation is per-model-kind in a static registry inside commands::prewarm; no global flag to thread through.
    let dispatcher = Dispatcher {
        sink: sink.clone(),
        shutdown: shutdown.clone(),
        command_shutdown_requested,
        db: db_conn.clone(),
        catalog_index: catalog_index.clone(),
        db_path,
        scan_state: scan_state.clone(),
        scan_active,
        scan_cancel_requested: scan_cancel_requested.clone(),
        deep_analyze_cancel: deep_analyze_cancel.clone(),
        deep_analyze_active,
        face_cluster_active,
        restructure_cancellations: restructure_cancellations.clone(),
        restructure_active,
        mutation_gate: mutation_gate.clone(),
        jobs: jobs.clone(),
        http_client,
    };
    let mut stdio_loop = tokio::spawn(async move {
        let d = dispatcher;
        let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
        loop {
            tokio::select! {
                biased;
                _ = d.shutdown.notified() => {
                    tracing::info!("shutdown notified; stdio loop exiting");
                    break;
                }
                read = bounded_read::bounded_read_line(&mut reader, &mut buf, MAX_FRAME_BYTES) => {
                    match read {
                        // Drop empty and BOM-only/whitespace-only lines: .NET's StreamWriter for Process.StandardInput can push a UTF-8 BOM on first init, which trips serde_json ("expected value at line 1 column 1").
                        Ok(BoundedRead::Line(text))
                            if text
                                .trim_start_matches('\u{FEFF}')
                                .trim()
                                .is_empty() =>
                        {
                            continue;
                        }
                        Ok(BoundedRead::Line(text)) => {
                            // Panic firewall: handle_line and the serde decode run INSIDE this spawned loop, whose JoinHandle is only abort()ed, never awaited. An uncaught panic in an inline-dispatched arm would silently kill the loop (no more commands, no error frame, the engine a zombie holding the DB writer). Catch it, emit a structured error, keep serving. parking_lot locks don't poison, so continuing is sound.
                            let handled = futures_util::FutureExt::catch_unwind(
                                std::panic::AssertUnwindSafe(handle_line(&d, &text)),
                            )
                            .await;
                            if handled.is_err() {
                                emit_handler_panic(&d.sink).await;
                            }
                            if d.command_shutdown_requested
                                .load(std::sync::atomic::Ordering::Acquire)
                            {
                                tracing::info!("shutdown command handled; stdio loop exiting");
                                break;
                            }
                        }
                        Ok(BoundedRead::Oversized(seen)) => {
                            // SEC: rejected mid-read, never allocated past the cap.
                            tracing::warn!(bytes_seen = seen, "oversized ipc frame; rejecting");
                            d.sink.send(EngineError::event(
                                "oversized_ipc_frame",
                                oversized_frame_message(seen),
                            )).await;
                            let _ = bounded_read::drain_to_newline(&mut reader).await;
                        }
                        Ok(BoundedRead::Eof) => {
                            tracing::info!("stdin EOF; entering shutdown");
                            d.shutdown.notify_waiters();
                            break;
                        }
                        Err(err) => {
                            tracing::error!(%err, "stdin read error");
                            d.shutdown.notify_waiters();
                            break;
                        }
                    }
                }
            }
        }
    });

    main_shutdown.await;
    scan_cancel_requested.store(true, std::sync::atomic::Ordering::Release);
    if let Some(coordinator) = scan_state.lock().as_ref() {
        coordinator.request_cancel();
    }
    deep_analyze_cancel.store(true, std::sync::atomic::Ordering::Release);
    restructure_cancellations.request_cancel();
    commands::prewarm::cancel_prewarm(None);

    if tokio::time::timeout(Duration::from_secs(2), &mut stdio_loop)
        .await
        .is_err()
    {
        tracing::warn!("stdio command loop did not stop within the shutdown deadline");
        stdio_loop.abort();
    }

    let mutation_guard = match tokio::time::timeout(
        Duration::from_secs(30),
        mutation_gate.lock(),
    )
    .await
    {
        Ok(guard) => Some(guard),
        Err(_) => {
            tracing::error!("active mutation did not quiesce before shutdown deadline");
            sink.send(EngineError::event(
                "shutdown_quiescence_timeout",
                "FileID couldn't finish stopping its work before closing. Your library is safe, and unfinished work will be recovered the next time you open FileID.",
            ))
            .await;
            None
        }
    };

    let resident_cleanup_error = models::resident_service::ResidentService::shared().shutdown().await.err();
    let tracker_cleanup_error = models::native_process::ProcessTracker::shared().shutdown().await.err();
    let native_cleanup_error = match (resident_cleanup_error, tracker_cleanup_error) {
        (None, error) | (error, None) => error,
        (Some(resident), Some(tracker)) => Some(anyhow::anyhow!("Resident cleanup: {resident:#}; native tracker: {tracker:#}")),
    };
    if let Some(error) = &native_cleanup_error {
        tracing::error!(%error, "Native cleanup failed at shutdown");
        sink.send(EngineError::event(
            "native_cleanup_failed_at_shutdown",
            "FileID couldn't close one of its AI helpers. If your computer feels slow after quitting FileID, restart it.",
        )).await;
    }

    if let Some(index) = catalog_index {
        index.close();
    }
    {
        let check = integrity_check.clone();
        let _ = tokio::task::spawn_blocking(move || check.cancel()).await;
    }
    db::close_readers();
    if mutation_guard.is_some() {
        let checkpoint_outcome = db_conn.as_ref().map(|conn_arc| {
            let guard = conn_arc.lock();
            db::checkpoint_truncate(&guard)
        });
        if let Some(Err(err)) = checkpoint_outcome {
            tracing::warn!(?err, "WAL checkpoint at shutdown failed");
            sink.send(EngineError::event(
                "checkpoint_failed_at_shutdown",
                "FileID couldn't finish tidying its library before closing. Your data is safe.",
            ))
            .await;
        }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(db_conn);

    jobs.clear_listeners();
    drop(sink);
    let _ = tokio::time::timeout(Duration::from_secs(2), sink_writer).await;

    if let Some(error) = native_cleanup_error {
        return Err(error);
    }
    if mutation_guard.is_some() {
        tracing::info!("FileIDEngine exiting cleanly after a quiescent checkpoint");
    } else {
        tracing::warn!("FileIDEngine exiting without a shutdown checkpoint");
    }
    Ok(())
}

/// 64-bit process creation time (FILETIME packed hi:lo) pinning parent identity across a PID-reuse window: two processes that ever share a PID cannot share a creation time.
#[cfg(windows)]
fn process_creation_time(handle: windows::Win32::Foundation::HANDLE) -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: handle is a live process handle opened with PROCESS_QUERY_LIMITED_INFORMATION; all four out-params are owned locals.
    let ok = unsafe {
        GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user)
    };
    if ok.is_err() {
        return None;
    }
    Some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
}

/// Watches the parent process for exit and signals `shutdown`. Identity is PINNED: we OpenProcess the PID once, capture the creation time, then re-sample our actual parent PID and confirm it is still `ppid` with the same creation time. The handle refers to a specific kernel object, so even if `ppid` is later reused the wait can't be redirected onto a stranger. If the parent already vanished by the time we open it, the creation-time mismatch signals shutdown immediately.
#[cfg(windows)]
async fn watch_parent_pinned(ppid: u32, shutdown: Arc<Notify>) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    // SAFETY: ppid is a u32 PID; OpenProcess returns a handle or an error. Reduce the (!Send) Result/HANDLE to a Send-safe usize BEFORE any await: this async fn is `tokio::spawn`ed, so a !Send value across the fallback `.await` would make the future !Send and fail to compile on Windows.
    let pinned_addr: Option<usize> = match unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            ppid,
        )
    } {
        Ok(h) if !h.is_invalid() => Some(h.0 as usize),
        _ => None,
    };
    let Some(handle_addr) = pinned_addr else {
        // Couldn't get a pinning handle (e.g. PROCESS_QUERY_LIMITED_INFORMATION denied): fall back to the SYNCHRONIZE-only watcher rather than disabling the watchdog.
        tracing::warn!(
            "could not open pinning handle (pid={ppid}); falling back to unpinned watch"
        );
        platform::watch_parent(ppid, shutdown).await;
        return;
    };
    let handle = HANDLE(handle_addr as *mut core::ffi::c_void);

    // Pin identity: capture the opened process' creation time, then re-sample who our parent is. Three distinct cases so a TRANSIENT re-sample miss can't shut a live parent down:
    // * Some(other != ppid): proof the parent exited and its PID was reused between get_parent_pid() and OpenProcess; the handle points at a stranger. Shut down.
    // * None, or the creation-time probe failed: INCONCLUSIVE (get_parent_pid() uses CreateToolhelp32Snapshot, which can transiently fail, e.g. ERROR_BAD_LENGTH under process churn). Do NOT shut down; fall back to the unpinned SYNCHRONIZE watcher.
    // * Some(ppid): still our parent; continue with the pinned wait.
    let pinned_creation = process_creation_time(handle);
    let resampled = platform::get_parent_pid();
    if let Some(current) = resampled && current != ppid {
        tracing::warn!(
            "parent PID changed (was {ppid}, now {current}); treating original parent as gone"
        );
        // SAFETY: handle was a live, owned handle from OpenProcess above.
        unsafe { let _ = CloseHandle(handle); }
        shutdown.notify_waiters();
        return;
    }
    if resampled.is_none() || pinned_creation.is_none() {
        tracing::warn!(
            "parent identity re-sample inconclusive (pid={ppid}); falling back to unpinned watch"
        );
        // SAFETY: handle was a live, owned handle from OpenProcess above.
        unsafe { let _ = CloseHandle(handle); }
        platform::watch_parent(ppid, shutdown).await;
        return;
    }

    // HANDLE wraps *mut c_void, which the windows crate doesn't mark Send, so we carry it across the closure as the `handle_addr` usize. SAFETY: a Win32 HANDLE is opaque and CloseHandle / WaitForSingleObject may run on any thread; this task is its sole owner.
    let wait = tokio::task::spawn_blocking(move || {
        let h = HANDLE(handle_addr as *mut core::ffi::c_void);
        // SAFETY: h is the pinned parent handle smuggled across threads.
        let r = unsafe { WaitForSingleObject(h, u32::MAX) };
        unsafe { let _ = CloseHandle(h); }
        r
    });

    // Race the blocking wait against the shared shutdown signal so EOF / explicit Shutdown can unblock us when the parent stays alive. If we lose, the blocking thread keeps its INFINITE wait until exit; the runtime's shutdown_timeout(0) keeps it from delaying exit.
    tokio::select! {
        _ = shutdown.notified() => {
            tracing::info!("watch_parent cancelled by shutdown signal");
        }
        result = wait => {
            if result.is_ok() {
                tracing::info!("parent process exited; signaling shutdown");
                shutdown.notify_waiters();
            } else {
                tracing::warn!("parent watchdog task aborted; falling back to stdin EOF detection");
            }
        }
    }
}

/// Non-Windows watchdog: the platform getppid poll. No PID-reuse TOCTOU here since POSIX keeps the parent a zombie until waited on.
#[cfg(not(windows))]
async fn watch_parent_pinned(ppid: u32, shutdown: Arc<Notify>) {
    platform::watch_parent(ppid, shutdown).await;
}

/// One job's hold on a single-in-flight flag (Deep Analyze, Organize Folders moves), released once: when the job queues its terminal event, or when the guard drops.
struct DeepSlot {
    active: Flag,
    released: std::sync::atomic::AtomicBool,
}

impl DeepSlot {
    fn hold(active: &Flag) -> Arc<Self> {
        Arc::new(Self { active: active.clone(), released: std::sync::atomic::AtomicBool::new(false) })
    }

    fn release(&self) {
        if !self.released.swap(true, std::sync::atomic::Ordering::AcqRel) {
            self.active.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

struct DeepActiveGuard(Arc<DeepSlot>);
impl Drop for DeepActiveGuard {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn request_scan_cancel(cancel: &std::sync::atomic::AtomicBool) {
    cancel.store(true, std::sync::atomic::Ordering::Release);
}

#[derive(Clone, Default)]
struct RestructureCancelRegistry {
    tokens: Arc<parking_lot::Mutex<Vec<Arc<std::sync::atomic::AtomicBool>>>>,
}

impl RestructureCancelRegistry {
    fn register(&self) -> RestructureCancelGuard {
        let token = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.tokens.lock().push(token.clone());
        RestructureCancelGuard {
            registry: self.clone(),
            token,
        }
    }

    fn request_cancel(&self) {
        for token in self.tokens.lock().iter() {
            token.store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

struct RestructureCancelGuard {
    registry: RestructureCancelRegistry,
    token: Arc<std::sync::atomic::AtomicBool>,
}

impl RestructureCancelGuard {
    fn token(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.token.clone()
    }
}

impl Drop for RestructureCancelGuard {
    fn drop(&mut self) {
        self.registry
            .tokens
            .lock()
            .retain(|token| !Arc::ptr_eq(token, &self.token));
    }
}

struct ScanActiveGuard(Arc<std::sync::atomic::AtomicBool>);
impl Drop for ScanActiveGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

fn reserve_scan(
    active: &Arc<std::sync::atomic::AtomicBool>,
    cancel_requested: &Arc<std::sync::atomic::AtomicBool>,
) -> Option<ScanActiveGuard> {
    active
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .ok()?;
    cancel_requested.store(false, std::sync::atomic::Ordering::Release);
    Some(ScanActiveGuard(active.clone()))
}

struct JobTrackGuard {
    queue: job_queue::JobQueue,
    id: job_queue::JobId,
    title: String,
}
impl JobTrackGuard {
    fn start(queue: &job_queue::JobQueue, category: JobCategory, title: &str) -> Self {
        let id = queue.push(category, title, None);
        queue.promote_next();
        Self { queue: queue.clone(), id, title: title.to_string() }
    }
}
impl JobTrackGuard {
    /// Shows how far finding the faces has got as the job's title and remaining time.
    fn finding_faces(&self) -> commands::face_clustering::FindingFaces {
        let (queue, id, started) = (self.queue.clone(), self.id.clone(), std::time::Instant::now());
        let title = self.title.clone();
        Arc::new(move |progress| match progress {
            Some((done, total)) if total > 0 => {
                let eta = (done > 0).then(|| started.elapsed().as_secs_f64() * (total - done.min(total)) as f64 / done as f64);
                queue.retitle(&id, format!("Finding faces… {} of {}", done.min(total), total), eta);
            }
            _ => queue.retitle(&id, title.as_str(), None),
        })
    }
}
impl Drop for JobTrackGuard {
    fn drop(&mut self) {
        self.queue.finish(&self.id);
    }
}

struct FaceClusterActiveGuard(Arc<std::sync::atomic::AtomicBool>);
impl Drop for FaceClusterActiveGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Files this scan could not read in time are scanned again at the next idle moment, as a normal incremental scan of the same folder.
fn retry_timed_out_files_when_idle(d: Dispatcher, payload: ipc::StartScanPayload, busy: Arc<dyn Fn() -> bool + Send + Sync>) {
    let (Some(db), Some(claim)) = (d.db.clone(), commands::timeout_retry::claim(&payload.root_path)) else {
        return;
    };
    tokio::spawn(async move {
        let _claim = claim;
        let root = std::path::PathBuf::from(&payload.root_path);
        let owed = commands::timeline::with_db(&db, move |conn| Ok(commands::timeout_retry::owed(conn, &root))).await;
        if !matches!(owed, Ok(true)) {
            return;
        }
        let scan_active = d.scan_active.clone();
        let idle = commands::timeout_retry::wait_until_idle(commands::timeout_retry::IDLE_POLL, commands::timeout_retry::IDLE_ATTEMPTS, || {
            !scan_active.load(std::sync::atomic::Ordering::Acquire) && !busy()
        })
        .await;
        if !idle {
            return;
        }
        tracing::info!("[SCAN] reading timed-out files again now that the machine is idle");
        let command = IpcCommand { id: uuid::Uuid::new_v4().to_string(), payload: CommandPayload::StartScan(ipc::StartScanPayload { rescan: false, ..payload }) };
        let Ok(line) = serde_json::to_string(&command) else { return };
        handle_scan_line(d, line).await;
    });
}

fn handle_scan_line(d: Dispatcher, line: String) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move { handle_line(&d, &line).await })
}

async fn emit_scan_busy(sink: &Sink) {
    sink.send(EngineError::event(
        "scan_already_running",
        "A scan is already running. Stop it first, or wait for it to finish.",
    ))
    .await;
}

/// What the Mac-hosted engine does after a scan ends normally: the Swift engine starts "Group faces" itself (`autoEnqueueFaceClusteringIfNeeded`) and the Mac app has no trigger; the Windows app sends the command after `scanComplete`.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
struct AfterScan {
    sink: Sink,
    db: Writer,
    jobs: job_queue::JobQueue,
    gate: Arc<tokio::sync::Mutex<()>>,
    scan_state: Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>>,
    active: Flag,
}

#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
impl AfterScan {
    /// The same job, flag and events as the Group faces button: queues behind the scan (which holds the mutation gate), and is skipped when a pass is already running or faces have nothing new to group by.
    async fn group_faces(self) {
        let db = self.db.clone();
        if !tokio::task::spawn_blocking(move || commands::face_clustering::needed_after_scan(&db)).await.unwrap_or(false) {
            return;
        }
        if self.active.compare_exchange(false, true, std::sync::atomic::Ordering::Acquire, std::sync::atomic::Ordering::Relaxed).is_err() {
            return;
        }
        let guard = FaceClusterActiveGuard(self.active.clone());
        let terminal_active = self.active.clone();
        let track = JobTrackGuard::start(&self.jobs, JobCategory::FaceCluster, "Group faces");
        tracing::info!("[CLUSTER] grouping faces after the scan");
        let (sink, db) = (self.sink.clone(), self.db.clone());
        spawn_mutation_with_rejection(
            &self.gate,
            &self.sink,
            &self.scan_state,
            face_clustering_rejection_event("FileID can't group faces while the scan is paused. Resume or stop the scan, then try again."),
            async move {
                let _guard = guard;
                let finding = track.finding_faces();
                let _track = track;
                commands::face_clustering::handle_run_face_clustering(sink, db, terminal_active, finding).await;
            },
        )
        .await;
    }
}

async fn emit_face_clustering_busy(sink: &Sink) {
    sink.send(EngineError::event(
        "face_clustering_busy",
        "FileID is already grouping faces.",
    ))
    .await;
}

async fn emit_restructure_busy(sink: &Sink) {
    sink.send(EngineError::event(
        "restructure_already_running",
        "Organize Folders is already moving files. Wait for it to finish.",
    ))
    .await;
}

async fn emit_deep_analyze_busy(sink: &Sink) {
    sink.send(EngineError::event(
        "deep_analyze_already_running",
        "Describe Files is already running. Wait for it to finish, or stop it first.",
    ))
    .await;
}

async fn emit_deep_analyze_gpu_blocked(sink: &Sink, model_kind: String) {
    commands::deep_analyze::send_gpu_failure_complete(sink, &model_kind, 0, 1, 0.0).await;
}

async fn spawn_mutation<F>(
    gate: &Arc<tokio::sync::Mutex<()>>,
    sink: &Sink,
    scan_state: &Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>>,
    operation: F,
) where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    spawn_mutation_with_interval(
        gate,
        sink,
        scan_state,
        Some(scan_rejection_event()),
        operation,
        std::time::Duration::from_secs(2),
    )
    .await;
}

async fn spawn_mutation_with_rejection<F>(
    gate: &Arc<tokio::sync::Mutex<()>>,
    sink: &Sink,
    scan_state: &Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>>,
    rejection_event: IpcEvent,
    operation: F,
) where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    spawn_mutation_with_interval(
        gate,
        sink,
        scan_state,
        Some(rejection_event),
        operation,
        std::time::Duration::from_secs(2),
    )
    .await;
}

async fn spawn_mutation_with_interval<F>(
    gate: &Arc<tokio::sync::Mutex<()>>,
    sink: &Sink,
    scan_state: &Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>>,
    rejection_event: Option<IpcEvent>,
    operation: F,
    status_interval: std::time::Duration,
) where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let gate = gate.clone();
    let sink = sink.clone();
    let scan_state = scan_state.clone();
    let (registered_tx, registered_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut gate_wait = Box::pin(gate.lock_owned());
        let mut registered_tx = Some(registered_tx);
        let initial_permit = std::future::poll_fn(|cx| {
            let result = std::future::Future::poll(gate_wait.as_mut(), cx);
            if let Some(tx) = registered_tx.take() {
                let _ = tx.send(());
            }
            std::task::Poll::Ready(match result {
                std::task::Poll::Ready(permit) => Some(permit),
                std::task::Poll::Pending => None,
            })
        })
        .await;

        // The registration handshake below stops dispatch processing a later wipe until this lock future has entered the mutex FIFO. Timeouts borrow that same pinned future, keeping its position while checking whether an unbounded paused scan should reject the action.
        let _permit = match initial_permit {
            Some(permit) => permit,
            None => loop {
                match tokio::time::timeout(status_interval, gate_wait.as_mut()).await {
                    Ok(permit) => break permit,
                    Err(_) => {
                        let scan_paused = scan_state
                            .lock()
                            .as_ref()
                            .is_some_and(|coord| coord.is_paused());
                        if scan_paused {
                            drop(operation);
                            sink.send(EngineError::event(
                                "library_busy",
                                "The library scan is paused, so this action can't run yet. \
                                              Resume or cancel the scan, then try again.",
                            ))
                            .await;
                            if let Some(event) = rejection_event {
                                sink.send(event).await;
                            }
                            return;
                        }
                    }
                }
            },
        };
        if futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(operation))
            .await
            .is_err()
        {
            emit_handler_panic(&sink).await;
            if let Some(event) = rejection_event {
                sink.send(event).await;
            }
        }
    });
    let _ = registered_rx.await;
}

async fn emit_handler_panic(sink: &Sink) {
    tracing::error!("panic in IPC command handler; engine continuing");
    sink.send(EngineError::event(
        "command_handler_panic",
        "Something went wrong inside FileID's helper, but it is still running. Try that again.",
    ))
    .await;
}

/// `tokio::spawn` for a command handler: a panic still answers the app with a `command_handler_panic` error instead of dying silently in a JoinHandle nobody awaits.
fn spawn_guarded<F>(sink: &Sink, handler: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let sink = sink.clone();
    tokio::spawn(async move {
        if futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(handler))
            .await
            .is_err()
        {
            emit_handler_panic(&sink).await;
        }
    });
}

fn bulk_rejection_event(action: &str, file_ids: &[i64], message: &str) -> IpcEvent {
    IpcEvent::now(EventPayload::BulkActionResult(Wrap::new(
        ipc::BulkActionResult {
            command_id: None,
            action: action.into(),
            succeeded: 0,
            failed: file_ids.len().max(1).min(u32::MAX as usize) as u32,
            messages: file_ids
                .iter()
                .map(|file_id| ipc::BulkActionItem {
                    file_id: Some(*file_id),
                    ok: false,
                    message: Some(message.into()),
                    reason: None,
                    operation_id: None,
                })
                .chain(file_ids.is_empty().then(|| ipc::BulkActionItem {
                    file_id: None,
                    ok: false,
                    message: Some(message.into()),
                    reason: None,
                    operation_id: None,
                }))
                .collect(),
        },
    )))
}

fn scan_rejection_event() -> IpcEvent {
    IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(ScanPhase::Failed)))
}

fn deep_analyze_rejection_event(model_kind: &str) -> IpcEvent {
    IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
        ipc::DeepAnalyzeComplete {
            processed: 0,
            failed: 1,
            total_seconds: 0.0,
            model_kind: model_kind.into(),
            cancelled: false,
        },
    )))
}

fn face_clustering_rejection_event(message: &str) -> IpcEvent {
    EngineError::event("face_clustering_failed", message)
}

fn idle_scan_rejection_event(
    active: &std::sync::atomic::AtomicBool,
) -> Option<IpcEvent> {
    (!active.load(std::sync::atomic::Ordering::Acquire)).then(scan_rejection_event)
}

fn idle_deep_analyze_rejection_event(
    active: &std::sync::atomic::AtomicBool,
    model_kind: &str,
) -> Option<IpcEvent> {
    (!active.load(std::sync::atomic::Ordering::Acquire))
        .then(|| deep_analyze_rejection_event(model_kind))
}

type Writer = Arc<parking_lot::Mutex<rusqlite::Connection>>;
type Flag = Arc<std::sync::atomic::AtomicBool>;

#[derive(Clone)]
struct Dispatcher {
    sink: Sink,
    shutdown: Arc<Notify>,
    command_shutdown_requested: Flag,
    db: Option<Writer>,
    catalog_index: Option<Arc<catalog_index::CatalogIndex>>,
    db_path: std::path::PathBuf,
    scan_state: Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>>,
    scan_active: Flag,
    scan_cancel_requested: Flag,
    deep_analyze_cancel: Flag,
    deep_analyze_active: Flag,
    face_cluster_active: Flag,
    restructure_cancellations: RestructureCancelRegistry,
    restructure_active: Flag,
    mutation_gate: Arc<tokio::sync::Mutex<()>>,
    jobs: job_queue::JobQueue,
    http_client: Arc<reqwest::Client>,
}

impl Dispatcher {
    fn for_command(&self, id: &str) -> Self {
        Self { sink: self.sink.for_command(id), ..self.clone() }
    }

    async fn dispatch_mutation<P, F, Fut>(&self, action: &str, ids: &[i64], phrase: &str, payload: P, handler: F)
    where
        P: Send + 'static,
        F: FnOnce(Sink, Writer, P) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let Some(db) = &self.db else {
            emit_db_unavailable(&self.sink, action).await;
            let message = format!("{phrase} because FileID couldn't open its library. Quit and reopen FileID.");
            self.sink.send(bulk_rejection_event(action, ids, &message)).await;
            return;
        };
        let rejection = bulk_rejection_event(action, ids, &format!("{phrase} while the library scan is paused."));
        let (sink, db) = (self.sink.clone(), db.clone());
        spawn_mutation_with_rejection(&self.mutation_gate, &self.sink, &self.scan_state, rejection, async move {
            handler(sink, db, payload).await;
        })
        .await;
    }

    async fn restructure_mutation<P, F, Fut>(&self, command: &str, kind: &str, phrase: &str, payload: P, handler: F)
    where
        P: Send + 'static,
        F: FnOnce(Sink, Writer, P, Flag) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let Some(db) = &self.db else {
            emit_db_unavailable(&self.sink, command).await;
            let message = format!("{phrase} because FileID couldn't open its library. Quit and reopen FileID.");
            self.sink.send(EngineError::event(kind, message)).await;
            return;
        };
        // Single-flight: a second apply (or an undo) while one runs would share the cancel flag and race on-disk moves and the undo journal; reject it loudly.
        if self
            .restructure_active
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            emit_restructure_busy(&self.sink).await;
            let message = format!("{phrase} because Organize Folders is already moving files.");
            self.sink.send(EngineError::event(kind, message)).await;
            return;
        }
        let slot = DeepSlot::hold(&self.restructure_active);
        let guard = DeepActiveGuard(slot.clone());
        let cancel_guard = self.restructure_cancellations.register();
        let sink = self.sink.releasing_before(|payload| matches!(payload, EventPayload::RestructureApplyResult(_)), move || slot.release());
        let (db, cancel) = (db.clone(), cancel_guard.token());
        let rejection = EngineError::event(kind, format!("{phrase} while the library scan is paused."));
        spawn_mutation_with_rejection(&self.mutation_gate, &self.sink, &self.scan_state, rejection, async move {
            let _guard = guard;
            let _cancel_guard = cancel_guard;
            handler(sink, db, payload, cancel).await;
        })
        .await;
    }

    async fn deep_analyze_arm<P, F, Fut>(&self, command: &str, model_kind: String, label: &str, payload: P, handler: F)
    where
        P: Send + 'static,
        F: FnOnce(Sink, Writer, P, Flag) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let Some(db) = &self.db else {
            emit_db_unavailable(&self.sink, command).await;
            self.sink.send(deep_analyze_rejection_event(&model_kind)).await;
            return;
        };
        if coordinator::process_gpu_device_removed() {
            emit_deep_analyze_gpu_blocked(&self.sink, model_kind).await;
            return;
        }
        if self
            .deep_analyze_active
            .compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Relaxed)
            .is_err()
        {
            emit_deep_analyze_busy(&self.sink).await;
            return;
        }
        self.deep_analyze_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        let slot = DeepSlot::hold(&self.deep_analyze_active);
        let guard = DeepActiveGuard(slot.clone());
        let track = JobTrackGuard::start(&self.jobs, JobCategory::DeepAnalyze, label);
        let sink = self.sink.releasing_before(|payload| matches!(payload, EventPayload::DeepAnalyzeComplete(_)), move || slot.release());
        let (db, cancel) = (db.clone(), self.deep_analyze_cancel.clone());
        let rejection = deep_analyze_rejection_event(&model_kind);
        spawn_mutation_with_rejection(&self.mutation_gate, &self.sink, &self.scan_state, rejection, async move {
            let _guard = guard;
            let _track = track;
            handler(sink, db, payload, cancel).await;
        })
        .await;
    }
}

async fn handle_line(d: &Dispatcher, line: &str) {
    // Strip a leading UTF-8 BOM defensively: the C# side ships BOM-less UTF-8, but legacy installs or wrappers may push `EF BB BF`.
    let line = line.trim_start_matches('\u{FEFF}').trim_start();
    if line.is_empty() {
        return;
    }
    let mut cmd: IpcCommand = match serde_json::from_str(line) {
        Ok(c) => c,
        Err(err) => {
            // IPC parity with macOS: emit command_decode_failed so a slightly-newer app learns its command was dropped. The C# app treats it as a non-fatal warning. serde's error carries position info, not line content, so no user path can leak.
            tracing::warn!(%err, "ipc decode failed");
            d.sink.send(EngineError::event(
                "command_decode_failed",
                "FileID's helper got a request it didn't understand. Quit and reopen FileID; if it keeps happening, reinstall FileID.",
            ))
            .await;
            return;
        }
    };
    let scoped = d.for_command(&cmd.id);
    let d = &scoped;
    let Dispatcher {
        sink,
        shutdown,
        command_shutdown_requested,
        db_path,
        scan_state,
        scan_active,
        scan_cancel_requested,
        deep_analyze_cancel,
        deep_analyze_active,
        face_cluster_active,
        restructure_cancellations,
        restructure_active,
        mutation_gate,
        jobs,
        http_client,
        ..
    } = d;
    let db = d.db.as_ref();
    let catalog_index = d.catalog_index.as_ref();
    if let Err(message) = ipc::normalize_and_validate_command(&mut cmd.payload) {
        tracing::warn!(%message, "IPC command rejected by semantic resource limits");
        let terminal = match &cmd.payload {
            CommandPayload::StartScan(_) => idle_scan_rejection_event(scan_active),
            CommandPayload::DeepAnalyzeFile(ipc::DeepAnalyzeFilePayload { model_kind, .. })
            | CommandPayload::DeepAnalyzeFolder(ipc::DeepAnalyzeFolderPayload { model_kind, .. })
            | CommandPayload::DeepAnalyzeAll(ipc::DeepAnalyzeAllPayload { model_kind, .. }) => {
                idle_deep_analyze_rejection_event(deep_analyze_active, model_kind)
            }
            CommandPayload::PlanRestructure(_) => {
                Some(EngineError::event("plan_restructure_invalid", &message))
            }
            CommandPayload::ApplyRestructure(_) => Some(EngineError::event("apply_restructure", &message)),
            CommandPayload::UndoRestructure(_) => Some(EngineError::event("undo_restructure", &message)),
            payload => payload.action_name().map(|action| {
                let detail = match payload {
                    CommandPayload::TrashFiles(_) => format!("Trash command was rejected: {message}"),
                    _ => message.clone(),
                };
                bulk_rejection_event(action, &payload.target_ids(), &detail)
            }),
        };
        sink.send(EngineError::event("command_limit_exceeded", message)).await;
        if let Some(event) = terminal {
            sink.send(event).await;
        }
        return;
    }

    let action = cmd.payload.action_name().unwrap_or_default();
    let ids = cmd.payload.target_ids();
    match cmd.payload {
        CommandPayload::ChatRequest(payload) => {
            spawn_guarded(sink, commands::chat::handle(sink.clone(), db.cloned(), payload.request));
        }
        CommandPayload::ToolRequest(payload) => {
            let sink = sink.clone();
            let database = db.cloned();
            spawn_guarded(&sink.clone(), commands::tools::handle(sink, database, payload.request));
        }
        CommandPayload::CatalogRequest(payload) => {
            let sink = sink.clone();
            let database = db.cloned();
            let index = catalog_index.cloned();
            spawn_guarded(&sink.clone(), commands::catalog::handle(sink, database, index, payload.request));
        }
        CommandPayload::ListRoots(payload) => {
            let database = db.cloned();
            spawn_guarded(sink, commands::roots::handle(sink.clone(), database, payload));
        }
        CommandPayload::ListModels(payload) => {
            spawn_guarded(sink, commands::models::handle(sink.clone(), payload));
        }
        CommandPayload::BenchmarkModel(payload) => {
            spawn_guarded(sink, commands::benchmark::handle(sink.clone(), payload));
        }
        CommandPayload::CancelBenchmark(_) => commands::benchmark::cancel(),
        CommandPayload::HealthCheck(payload) => {
            let response = if payload.is_valid() {
                EventPayload::HealthCheckResult(Wrap::new(ipc::HealthCheckResult {
                    request_id: payload.request_id,
                    pid: std::process::id() as i32,
                }))
            } else {
                EventPayload::Error(Wrap::new(EngineError::new(
                    "invalid_health_request",
                    "Health request ID must contain 1–128 ASCII letters, digits, underscores or hyphens.",
                )))
            };
            sink.send(IpcEvent::now(response)).await;
        }
        CommandPayload::RequestStatus(_) => {
            commands::hardware::emit_ready(sink).await;
        }
        CommandPayload::VerifyCudaPack(_) => {
            commands::hardware::handle_verify_cuda_pack(sink).await;
        }
        CommandPayload::Shutdown(_) => {
            tracing::info!("shutdown command received");
            command_shutdown_requested.store(true, std::sync::atomic::Ordering::Release);
            shutdown.notify_waiters();
        }
        CommandPayload::PrewarmModel(payload) => {
            // Reset THIS kind's cancel flag SYNCHRONOUSLY before the spawn, so a CancelPrewarm dispatched right after (the serial stdio loop runs this before the next frame) lands on an existing flag, closing the lazy-create race.
            commands::prewarm::reset_prewarm_cancel(&payload.model_kind);
            let sink = sink.clone();
            let model_kind = payload.model_kind.clone();
            let http_client = http_client.clone();
            spawn_guarded(&sink.clone(), commands::prewarm::handle_prewarm_model(sink, model_kind, http_client));
        }
        CommandPayload::CancelPrewarm(payload) => {
            // Per-model when modelKind is set (one row's Cancel), all in-flight when None.
            commands::prewarm::cancel_prewarm(payload.model_kind.as_deref());
        }
        CommandPayload::PlanRestructure(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "planRestructure").await;
                sink.send(EngineError::event(
                    "plan_restructure_db_unavailable",
                    "FileID couldn't open its library, so Organize Folders can't make a plan. Quit and reopen FileID.",
                ))
                .await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            let cancel_guard = restructure_cancellations.register();
            let cancel_c = cancel_guard.token();
            spawn_mutation_with_rejection(
                mutation_gate,
                sink,
                scan_state,
                EngineError::event(
                    "plan_restructure_library_busy",
                    "Organize Folders can't make a plan while the scan is paused. Resume or stop the scan, then try again.",
                ),
                async move {
                    let _cancel_guard = cancel_guard;
                    commands::restructure::handle_plan_restructure(
                        sink_c,
                        db_c,
                        payload,
                        cancel_c,
                    )
                    .await;
                },
            )
            .await;
        }
        CommandPayload::ApplyRestructure(payload) => {
            d.restructure_mutation("applyRestructure", "apply_restructure", "Organize Folders couldn't move the files", payload, commands::restructure::handle_apply_restructure)
                .await;
        }
        CommandPayload::UndoRestructure(payload) => {
            d.restructure_mutation("undoRestructure", "undo_restructure", "Organize Folders couldn't undo the move", payload, commands::restructure::handle_undo_restructure)
                .await;
        }
        CommandPayload::GrantFolderAccess(payload) => commands::folder_access::grant(sink, payload).await,
        CommandPayload::StartScan(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "startScan").await;
                sink.send(scan_rejection_event()).await;
                return;
            };
            let Some(active_guard) = reserve_scan(scan_active, scan_cancel_requested) else {
                emit_scan_busy(sink).await;
                return;
            };
            let track = JobTrackGuard::start(jobs, JobCategory::Scan, "Scan library");
            let sink_c = sink.clone();
            let db_c = db.clone();
            let state_c = scan_state.clone();
            let pending_cancel = scan_cancel_requested.clone();
            let retry_payload = payload.clone();
            let retry_dispatcher = d.clone();
            let other_heavy_work: Arc<dyn Fn() -> bool + Send + Sync> = {
                let (jobs, deep, faces, restructure) =
                    (jobs.clone(), deep_analyze_active.clone(), face_cluster_active.clone(), restructure_active.clone());
                Arc::new(move || {
                    jobs.has_active_besides(JobCategory::Scan)
                        || deep.load(std::sync::atomic::Ordering::Acquire)
                        || faces.load(std::sync::atomic::Ordering::Acquire)
                        || restructure.load(std::sync::atomic::Ordering::Acquire)
                        || commands::prewarm::any_download_in_flight()
                })
            };
            let prewarm_db = db.clone();
            #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
            let after_scan = AfterScan {
                sink: sink.clone(),
                db: db.clone(),
                jobs: jobs.clone(),
                gate: mutation_gate.clone(),
                scan_state: scan_state.clone(),
                active: face_cluster_active.clone(),
            };
            spawn_mutation(mutation_gate, sink, scan_state, async move {
                let _track = track;
                let completed = commands::scan::handle_start_scan(
                    sink_c,
                    db_c,
                    state_c,
                    payload,
                    pending_cancel,
                    Some(Box::new(active_guard)),
                    Some(other_heavy_work.clone()),
                )
                .await;
                if completed {
                    retry_timed_out_files_when_idle(retry_dispatcher, retry_payload, other_heavy_work);
                    drop(tokio::task::spawn_blocking(move || commands::embed::prewarm_clip_text(&prewarm_db)));
                }
                #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
                if completed {
                    after_scan.group_faces().await;
                }
                #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
                let _ = completed;
            })
            .await;
        }
        CommandPayload::PauseScan(_) => {
            if let Some(coord) = scan_state.lock().as_ref() {
                coord.request_pause();
                tracing::info!("scan pause requested");
            }
        }
        CommandPayload::ResumeScan(_) => {
            if let Some(coord) = scan_state.lock().as_ref() {
                coord.request_resume();
                tracing::info!("scan resume requested");
            }
        }
        CommandPayload::CancelScan(_) => {
            request_scan_cancel(scan_cancel_requested);
            if let Some(coord) = scan_state.lock().as_ref() {
                coord.request_cancel();
                tracing::info!("scan cancel requested");
            }
        }
        CommandPayload::CancelRestructure(_) => {
            restructure_cancellations.request_cancel();
            tracing::info!("restructure cancel requested");
        }
        CommandPayload::ApplyTags(payload) => {
            d.dispatch_mutation(action, &ids, "Tags could not be applied", payload, commands::bulk::handle_apply_tags).await;
        }
        CommandPayload::RenameFiles(payload) => {
            d.dispatch_mutation(action, &ids, "Files could not be renamed", payload, commands::bulk::handle_rename_files).await;
        }
        CommandPayload::TrashFiles(payload) => {
            d.dispatch_mutation(action, &ids, "Trash could not run", payload, commands::bulk::handle_trash_files).await;
        }
        CommandPayload::MergeClusters(payload) => {
            d.dispatch_mutation(action, &ids, "Could not merge people", payload, commands::bulk::handle_merge_clusters).await;
        }
        CommandPayload::EmbedTextQuery(payload) => {
            spawn_guarded(sink, commands::embed::handle_embed_text_query(sink.clone(), payload));
        }
        CommandPayload::RenamePerson(payload) => {
            d.dispatch_mutation(action, &ids, "The person could not be renamed", payload, commands::bulk::handle_rename_person).await;
        }
        CommandPayload::ReassignFace(payload) => {
            d.dispatch_mutation(action, &ids, "The face could not be reassigned", payload, commands::bulk::handle_reassign_face).await;
        }
        CommandPayload::MarkPersonsAsUnknown(payload) => {
            d.dispatch_mutation(action, &ids, "People could not be marked unknown", payload, commands::bulk::handle_mark_persons_as_unknown).await;
        }
        CommandPayload::FindMergeSuggestions(_) => {
            // Availability guard (no DB → no read connection); the handler opens its own read-only connection from db_path so it never contends on the writer mutex.
            let Some(_db) = db else {
                emit_db_unavailable(sink, "findMergeSuggestions").await;
                sink.send(EngineError::event(
                    "find_merge_suggestions_failed",
                    "FileID couldn't open its library, so it can't suggest people to merge.",
                ))
                .await;
                return;
            };
            let sink_c = sink.clone();
            let path_c = db_path.clone();
            spawn_guarded(sink, commands::bulk::handle_find_merge_suggestions(sink_c, path_c));
        }
        CommandPayload::MarkPersonsDifferent(payload) => {
            d.dispatch_mutation(action, &ids, "The verdict could not be saved", payload, commands::bulk::handle_mark_persons_different).await;
        }
        CommandPayload::EmbedImageQuery(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "embedImageQuery").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            spawn_guarded(sink, commands::embed::handle_embed_image_query(sink_c, db_c, payload));
        }
        CommandPayload::DeepAnalyzeFile(payload) => {
            let model_kind = payload.model_kind.clone();
            d.deep_analyze_arm("deepAnalyzeFile", model_kind, "Describe 1 file", payload, commands::deep_analyze::handle_deep_analyze_file)
                .await;
        }
        CommandPayload::DeepAnalyzeFolder(payload) => {
            let model_kind = payload.model_kind.clone();
            d.deep_analyze_arm("deepAnalyzeFolder", model_kind, "Describe a folder", payload, commands::deep_analyze::handle_deep_analyze_folder)
                .await;
        }
        CommandPayload::DeepAnalyzeAll(payload) => {
            let model_kind = payload.model_kind.clone();
            d.deep_analyze_arm("deepAnalyzeAll", model_kind, "Describe the whole library", payload, commands::deep_analyze::handle_deep_analyze_all)
                .await;
        }
        CommandPayload::DeepAnalyzeCancel(_) => {
            deep_analyze_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("deep analyze cancel requested");
        }
        CommandPayload::RestoreFromTrash(payload) => {
            d.dispatch_mutation(action, &ids, "Restore could not run", payload, commands::trash::handle_restore_from_trash).await;
        }
        CommandPayload::RevertMerge(payload) => {
            d.dispatch_mutation(action, &ids, "The merge could not be reverted", payload, commands::trash::handle_revert_merge).await;
        }
        CommandPayload::RunFaceClustering(_) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "runFaceClustering").await;
                sink.send(face_clustering_rejection_event(
                    "FileID couldn't open its library, so it can't group faces. Quit and reopen FileID.",
                ))
                .await;
                return;
            };
            // Single-flight: the handler drops the writer lock during cluster()+consolidate(), so overlapping runs could race the persist transaction; bounce a second request.
            if face_cluster_active
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::Acquire,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_err()
            {
                emit_face_clustering_busy(sink).await;
                return;
            }
            let guard = FaceClusterActiveGuard(face_cluster_active.clone());
            let terminal_active = face_cluster_active.clone();
            let track = JobTrackGuard::start(jobs, JobCategory::FaceCluster, "Group faces");
            let sink_c = sink.clone();
            let db_c = db.clone();
            spawn_mutation_with_rejection(
                mutation_gate,
                sink,
                scan_state,
                face_clustering_rejection_event(
                    "FileID can't group faces while the scan is paused. Resume or stop the scan, then try again.",
                ),
                async move {
                    let _guard = guard;
                    let finding = track.finding_faces();
                    let _track = track;
                    commands::face_clustering::handle_run_face_clustering(
                        sink_c,
                        db_c,
                        terminal_active,
                        finding,
                    )
                    .await;
                },
            )
            .await;
        }
        CommandPayload::WipeLibrary(_) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "wipeLibrary").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            let state_c = scan_state.clone();
            let fca_c = face_cluster_active.clone();
            let da_cancel_c = deep_analyze_cancel.clone();
            let da_active_c = deep_analyze_active.clone();
            restructure_cancellations.request_cancel();
            commands::wipe::handle_wipe_library(
                sink_c,
                db_c,
                state_c,
                scan_cancel_requested.clone(),
                fca_c,
                da_cancel_c,
                da_active_c,
                restructure_active.clone(),
                mutation_gate.clone(),
            )
            .await;
        }
        CommandPayload::GenerateVideoThumbnail(payload) => {
            spawn_guarded(sink, commands::thumbnail::handle_generate_video_thumbnail(sink.clone(), payload));
        }
        CommandPayload::PurgeExcluded(payload) => {
            d.dispatch_mutation(action, &ids, "Excluded folders could not be removed from your library", payload, commands::scan::handle_purge_excluded).await;
        }
        CommandPayload::ListFaceReview(payload) => {
            spawn_guarded(sink, commands::face_review::handle_list(sink.clone(), db.cloned(), payload));
        }
        CommandPayload::ResolveFaceReview(payload) => {
            let rejection = commands::face_review::rejection(&payload.request_id, "resolve");
            spawn_mutation_with_rejection(mutation_gate, sink, scan_state, rejection, commands::face_review::handle_resolve(sink.clone(), db.cloned(), payload)).await;
        }
        CommandPayload::UndoFaceReview(payload) => {
            let rejection = commands::face_review::rejection(&payload.request_id, "undo");
            spawn_mutation_with_rejection(mutation_gate, sink, scan_state, rejection, commands::face_review::handle_undo(sink.clone(), db.cloned(), payload)).await;
        }
    }
}

fn db_open_error_event(err: &anyhow::Error, db_path: &std::path::Path) -> IpcEvent {
    let path = db_path.display().to_string();
    if let Some(newer) = err.downcast_ref::<db::migrations::NewerThanEngine>() {
        tracing::error!(%newer, "database schema is newer than this engine");
        return EngineError::new(
            "db_newer_than_engine",
            "This library was made by a newer version of FileID. Update FileID to open it.",
        )
        .with_path(path)
        .into_event();
    }
    if crate::util::user_message::io_cause(err) == Some(crate::util::user_message::IoCause::DiskFull) {
        return EngineError::new(
            "db_disk_full",
            "FileID has no room to save its library. Free some space (about 1 GB is plenty), then reopen FileID.",
        )
        .with_path(path)
        .into_event();
    }
    let message = if err.downcast_ref::<db::ReadOnlyDatabase>().is_some() {
        "FileID's library file is read-only, so it can't scan. Make the FileID data folder writable, then restart FileID."
    } else {
        "FileID couldn't open its library, so it can't scan. Quit and reopen FileID; if it keeps happening, reinstall FileID."
    };
    EngineError::new("db_open_failed", message).with_path(path).into_event()
}

async fn emit_db_unavailable(sink: &Sink, command: &str) {
    tracing::warn!(command, "command refused: the database did not open at startup");
    sink.send(EngineError::event(
        "db_unavailable",
        "FileID couldn't open its library, so this can't run. Quit and reopen FileID.",
    ))
    .await;
}

#[cfg(test)]
#[path = "server_fuzz_tests.rs"]
pub(crate) mod fuzz_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dispatcher(db: Option<Writer>, sink: Sink) -> Dispatcher {
        Dispatcher {
            sink,
            shutdown: Arc::new(Notify::new()),
            command_shutdown_requested: Arc::default(),
            db,
            catalog_index: None,
            db_path: std::path::PathBuf::new(),
            scan_state: Arc::default(),
            scan_active: Arc::default(),
            scan_cancel_requested: Arc::default(),
            deep_analyze_cancel: Arc::default(),
            deep_analyze_active: Arc::default(),
            face_cluster_active: Arc::default(),
            restructure_cancellations: RestructureCancelRegistry::default(),
            restructure_active: Arc::default(),
            mutation_gate: Arc::new(tokio::sync::Mutex::new(())),
            jobs: job_queue::JobQueue::new(),
            http_client: crate::downloader::fail_closed_client(),
        }
    }

    async fn bulk_results(
        events: &mut tokio::sync::mpsc::Receiver<IpcEvent>,
        wanted: usize,
    ) -> Vec<ipc::BulkActionResult> {
        let mut found = Vec::new();
        while found.len() < wanted {
            let event = tokio::time::timeout(std::time::Duration::from_secs(20), events.recv())
                .await
                .expect("a bulk result arrives")
                .expect("sink open");
            if let EventPayload::BulkActionResult(result) = event.payload {
                found.push(result.inner);
            }
        }
        found
    }

    fn deep_complete() -> IpcEvent {
        IpcEvent::now(EventPayload::DeepAnalyzeComplete(ipc::Wrap::new(ipc::DeepAnalyzeComplete {
            processed: 0,
            failed: 0,
            total_seconds: 0.0,
            model_kind: "m".into(),
            cancelled: false,
        })))
    }

    async fn next_event(events: &mut tokio::sync::mpsc::Receiver<IpcEvent>) -> EventPayload {
        tokio::time::timeout(std::time::Duration::from_secs(20), events.recv())
            .await
            .expect("an event arrives")
            .expect("sink open")
            .payload
    }

    #[tokio::test]
    async fn the_deep_analyze_slot_is_free_when_the_terminal_event_can_be_read() {
        let dir = std::env::temp_dir().join(format!("fileid-deep-slot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let writer = db::open_writer(&dir.join("fileid.sqlite")).expect("temp catalog");
        let (sink, mut events) = Sink::channel_for_test(64);
        let d = test_dispatcher(Some(Arc::new(parking_lot::Mutex::new(writer))), sink);
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();

        d.deep_analyze_arm("deepAnalyzeAll", "m".into(), "first", (), move |sink: Sink, _db, _payload: (), _cancel| async move {
            sink.send(deep_complete()).await;
            let _ = finished.await;
        })
        .await;
        assert!(matches!(next_event(&mut events).await, EventPayload::DeepAnalyzeComplete(_)));
        assert!(!d.deep_analyze_active.load(std::sync::atomic::Ordering::Acquire), "slot still held after the terminal event");

        d.deep_analyze_arm("deepAnalyzeAll", "m".into(), "second", (), |sink: Sink, _db, _payload: (), _cancel| async move {
            sink.send(deep_complete()).await;
        })
        .await;
        assert!(d.deep_analyze_active.load(std::sync::atomic::Ordering::Acquire), "the second job took the slot");
        finish.send(()).unwrap();
        assert!(
            matches!(next_event(&mut events).await, EventPayload::DeepAnalyzeComplete(_)),
            "the second command ran instead of being refused as already running"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_organize_folders_move_frees_its_slot_with_its_result() {
        let dir = std::env::temp_dir().join(format!("fileid-restructure-slot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let writer = db::open_writer(&dir.join("fileid.sqlite")).expect("temp catalog");
        let (sink, mut events) = Sink::channel_for_test(64);
        let d = test_dispatcher(Some(Arc::new(parking_lot::Mutex::new(writer))), sink);
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let result = || {
            IpcEvent::now(EventPayload::RestructureApplyResult(ipc::Wrap::new(ipc::RestructureApplyResult {
                applied: 0,
                failed: 0,
                privilege_error: None,
                cancelled: false,
                planned: None,
                remaining: None,
                shortcut_undo_token: None,
            })))
        };

        d.restructure_mutation("applyRestructure", "apply_restructure", "first", (), move |sink: Sink, _db, _payload: (), _cancel| async move {
            sink.send(result()).await;
            let _ = finished.await;
        })
        .await;
        assert!(matches!(next_event(&mut events).await, EventPayload::RestructureApplyResult(_)));
        assert!(!d.restructure_active.load(std::sync::atomic::Ordering::Acquire), "slot still held after the result");

        d.restructure_mutation("undoRestructure", "undo_restructure", "second", (), move |sink: Sink, _db, _payload: (), _cancel| async move {
            sink.send(result()).await;
        })
        .await;
        finish.send(()).unwrap();
        assert!(
            matches!(next_event(&mut events).await, EventPayload::RestructureApplyResult(_)),
            "the undo ran instead of being refused as already moving files"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_running_deep_analyze_still_refuses_a_second_one() {
        let dir = std::env::temp_dir().join(format!("fileid-deep-busy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let writer = db::open_writer(&dir.join("fileid.sqlite")).expect("temp catalog");
        let (sink, mut events) = Sink::channel_for_test(64);
        let d = test_dispatcher(Some(Arc::new(parking_lot::Mutex::new(writer))), sink);
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();

        d.deep_analyze_arm("deepAnalyzeAll", "m".into(), "first", (), move |sink: Sink, _db, _payload: (), _cancel| async move {
            let _ = finished.await;
            sink.send(deep_complete()).await;
        })
        .await;
        d.deep_analyze_arm("deepAnalyzeAll", "m".into(), "second", (), |_sink: Sink, _db, _payload: (), _cancel| async move {}).await;
        match next_event(&mut events).await {
            EventPayload::Error(error) => assert_eq!(error.inner.kind, "deep_analyze_already_running"),
            other => panic!("expected the busy refusal, got {other:?}"),
        }
        finish.send(()).unwrap();
        assert!(matches!(next_event(&mut events).await, EventPayload::DeepAnalyzeComplete(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn merge_line(id: &str) -> String {
        format!(r#"{{"id":"{id}","payload":{{"mergeClusters":{{"sourcePersonID":1,"destinationPersonID":2}}}}}}"#)
    }

    #[tokio::test]
    async fn a_refused_bulk_command_echoes_its_id() {
        let (sink, mut events) = Sink::channel_for_test(64);
        let d = test_dispatcher(None, sink);
        handle_line(&d, &merge_line("cmd-refused")).await;
        let results = bulk_results(&mut events, 1).await;
        assert!(results.iter().all(|r| r.command_id.as_deref() == Some("cmd-refused")), "{results:?}");
        assert!(results.iter().all(|r| r.action == "mergeClusters" && r.succeeded == 0));
    }

    #[tokio::test]
    async fn a_bulk_result_from_a_spawned_task_echoes_its_id() {
        let dir = std::env::temp_dir().join(format!("fileid-command-id-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let writer = db::open_writer(&dir.join("fileid.sqlite")).expect("temp catalog");
        let (sink, mut events) = Sink::channel_for_test(64);
        let d = test_dispatcher(Some(Arc::new(parking_lot::Mutex::new(writer))), sink);
        handle_line(&d, &merge_line("cmd-first")).await;
        handle_line(&d, &merge_line("cmd-second")).await;
        let mut results = bulk_results(&mut events, 2).await;
        results.sort_by(|a, b| a.command_id.cmp(&b.command_id));
        let ids: Vec<_> = results.iter().map(|r| r.command_id.as_deref()).collect();
        assert_eq!(ids, [Some("cmd-first"), Some("cmd-second")]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_sink_keeps_a_result_id_it_already_has_and_leaves_other_events_alone() {
        let (sink, mut events) = Sink::channel_for_test(8);
        let scoped = sink.for_command("outer");
        let own = ipc::BulkActionResult {
            action: "trashFiles".into(),
            command_id: Some("inner".into()),
            succeeded: 0,
            failed: 0,
            messages: vec![],
        };
        scoped.send(IpcEvent::now(EventPayload::BulkActionResult(Wrap::new(own)))).await;
        scoped.send(EngineError::event("probe", "probe")).await;
        sink.send(bulk_rejection_event("trashFiles", &[1], "unscoped")).await;
        let kept = bulk_results(&mut events, 1).await;
        assert_eq!(kept[0].command_id.as_deref(), Some("inner"));
        let unscoped = bulk_results(&mut events, 1).await;
        assert_eq!(unscoped[0].command_id, None);
    }

    #[test]
    fn a_fresh_restructure_operation_is_not_poisoned_by_an_old_cancel() {
        let registry = RestructureCancelRegistry::default();
        {
            let cancelled = registry.register();
            registry.request_cancel();
            assert!(cancelled
                .token()
                .load(std::sync::atomic::Ordering::Acquire));
        }

        let fresh = registry.register();
        assert!(!fresh.token().load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn scan_and_restructure_cancel_signals_are_isolated() {
        let scan = std::sync::atomic::AtomicBool::new(false);
        let registry = RestructureCancelRegistry::default();
        let restructure = registry.register();

        request_scan_cancel(&scan);
        assert!(scan.load(std::sync::atomic::Ordering::Acquire));
        assert!(!restructure
            .token()
            .load(std::sync::atomic::Ordering::Acquire));

        scan.store(false, std::sync::atomic::Ordering::Release);
        registry.request_cancel();
        assert!(!scan.load(std::sync::atomic::Ordering::Acquire));
        assert!(restructure
            .token()
            .load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn cancel_restructure_reaches_active_and_queued_operations_only() {
        let registry = RestructureCancelRegistry::default();
        let active = registry.register();
        let queued = registry.register();

        registry.request_cancel();

        assert!(active.token().load(std::sync::atomic::Ordering::Acquire));
        assert!(queued.token().load(std::sync::atomic::Ordering::Acquire));
        drop(active);
        drop(queued);

        let later = registry.register();
        assert!(!later.token().load(std::sync::atomic::Ordering::Acquire));
    }

    // The oversize-frame rejection text must report the REAL cap (MAX_FRAME_BYTES, 64 MiB), not a stale hardcoded "1 MB".
    #[test]
    fn oversize_frame_error_text_matches_max_frame_bytes() {
        let msg = oversized_frame_message(99_999);

        let expected_mib = MAX_FRAME_BYTES / (1024 * 1024);
        assert_eq!(expected_mib, 64, "cap is 64 MiB");

        assert!(
            msg.contains(&format!("{expected_mib} MiB")),
            "message must state the real cap ({expected_mib} MiB): {msg}"
        );
        assert!(
            !msg.contains("1 MB"),
            "message must not report the stale 1 MB cap: {msg}"
        );
        assert!(msg.contains("99999"), "message must include the seen byte count: {msg}");
    }

    #[test]
    fn trash_rejection_event_is_correlated_per_file_and_terminal() {
        let event = bulk_rejection_event("trashFiles", &[4, 9], "busy");
        let EventPayload::BulkActionResult(result) = event.payload else {
            panic!("expected BulkActionResult");
        };
        assert_eq!(result.inner.action, "trashFiles");
        assert_eq!(result.inner.succeeded, 0);
        assert_eq!(result.inner.failed, 2);
        assert_eq!(
            result
                .inner
                .messages
                .iter()
                .filter_map(|item| item.file_id)
                .collect::<Vec<_>>(),
            vec![4, 9]
        );
        assert!(result.inner.messages.iter().all(|item| !item.ok));
    }

    #[test]
    fn merge_and_restructure_rejections_have_awaited_terminal_shapes() {
        let merge = bulk_rejection_event("mergeClusters", &[], "busy");
        let EventPayload::BulkActionResult(result) = merge.payload else {
            panic!("expected BulkActionResult");
        };
        assert_eq!(result.inner.action, "mergeClusters");
        assert_eq!(result.inner.succeeded, 0);
        assert_eq!(result.inner.failed, 1);
        assert_eq!(result.inner.messages.len(), 1);
        assert!(!result.inner.messages[0].ok);

        for kind in [
            "plan_restructure_library_busy",
            "apply_restructure",
            "undo_restructure",
        ] {
            let event = EngineError::event(kind, "busy");
            assert!(matches!(
                event.payload,
                EventPayload::Error(Wrap {
                    inner: EngineError { kind: ref actual, .. }
                }) if actual == kind
            ));
        }
    }

    #[test]
    fn scan_and_deep_analyze_rejections_have_lifecycle_terminals() {
        assert!(matches!(
            scan_rejection_event().payload,
            EventPayload::PhaseChanged(Wrap {
                inner: ScanPhase::Failed
            })
        ));

        let event = deep_analyze_rejection_event("qwen2_5_vl_7b");
        let EventPayload::DeepAnalyzeComplete(result) = event.payload else {
            panic!("expected DeepAnalyzeComplete");
        };
        assert_eq!(result.inner.processed, 0);
        assert_eq!(result.inner.failed, 1);
        assert_eq!(result.inner.model_kind, "qwen2_5_vl_7b");
        assert!(!result.inner.cancelled);

        let face = face_clustering_rejection_event("busy");
        assert!(matches!(
            face.payload,
            EventPayload::Error(Wrap {
                inner: EngineError { ref kind, .. }
            }) if kind == "face_clustering_failed"
        ));

        let active = std::sync::atomic::AtomicBool::new(true);
        assert!(idle_scan_rejection_event(&active).is_none());
        assert!(idle_deep_analyze_rejection_event(&active, "qwen2_5_vl_7b").is_none());
        active.store(false, std::sync::atomic::Ordering::Release);
        assert!(idle_scan_rejection_event(&active).is_some());
        assert!(idle_deep_analyze_rejection_event(&active, "qwen2_5_vl_7b").is_some());
    }

    #[tokio::test]
    async fn gpu_blocked_deep_analyze_gets_error_and_terminal_completion() {
        let (sink, mut events) = Sink::channel_for_test(2);

        emit_deep_analyze_gpu_blocked(&sink, "qwen2_5_vl_7b".into()).await;

        let error = events.recv().await.expect("GPU error event");
        let complete = events.recv().await.expect("Deep Analyze completion");
        assert!(matches!(
            error.payload,
            EventPayload::Error(Wrap { inner: EngineError { ref kind, .. } })
                if kind == "gpu_device_removed"
        ));
        assert!(matches!(
            complete.payload,
            EventPayload::DeepAnalyzeComplete(Wrap {
                inner: ipc::DeepAnalyzeComplete {
                    processed: 0,
                    failed: 1,
                    cancelled: false,
                    ..
                }
            })
        ));
    }

    #[test]
    fn database_open_errors_name_their_cause() {
        let kind_and_message = |err: anyhow::Error| match db_open_error_event(&err, std::path::Path::new("/state/fileid.sqlite")).payload {
            EventPayload::Error(Wrap { inner }) => (inner.kind, inner.message),
            _ => panic!("expected an error event"),
        };
        let newer = anyhow::Error::from(db::migrations::NewerThanEngine { unknown: vec!["v99".into()] })
            .context("applying migrations");
        let (kind, message) = kind_and_message(newer);
        assert_eq!(kind, "db_newer_than_engine");
        assert_eq!(message, "This library was made by a newer version of FileID. Update FileID to open it.");
        let (kind, message) = kind_and_message(anyhow::Error::from(db::ReadOnlyDatabase));
        assert_eq!(kind, "db_open_failed");
        assert!(message.contains("read-only"), "{message}");
        let full = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::StorageFull)).context("opening the library");
        let (kind, message) = kind_and_message(full);
        assert_eq!(kind, "db_disk_full");
        assert!(message.starts_with("FileID has no room to save its library."), "{message}");
        let (kind, message) = kind_and_message(anyhow::anyhow!("file is not a database"));
        assert_eq!(kind, "db_open_failed");
        assert!(message.starts_with("FileID couldn't open its library, so it can't scan."), "{message}");
    }

    #[tokio::test]
    async fn panicking_spawned_handler_still_answers_with_an_error() {
        let (sink, mut events) = Sink::channel_for_test(4);
        spawn_guarded(&sink, async {
            panic!("injected handler panic");
        });
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .expect("panic must be answered")
            .expect("event");
        assert!(matches!(
            event.payload,
            EventPayload::Error(Wrap { inner: EngineError { ref kind, .. } }) if kind == "command_handler_panic"
        ));
    }

    #[tokio::test]
    async fn panicking_mutation_emits_its_terminal_and_releases_the_gate() {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let (sink, mut events) = Sink::channel_for_test(8);
        let scan_state: Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>> =
            Arc::new(parking_lot::Mutex::new(None));
        spawn_mutation_with_rejection(
            &gate,
            &sink,
            &scan_state,
            bulk_rejection_event("applyTags", &[7], "interrupted"),
            async {
                panic!("injected mutation panic");
            },
        )
        .await;
        let first = events.recv().await.expect("panic error");
        let second = events.recv().await.expect("terminal");
        assert!(matches!(
            first.payload,
            EventPayload::Error(Wrap { inner: EngineError { ref kind, .. } }) if kind == "command_handler_panic"
        ));
        assert!(matches!(second.payload, EventPayload::BulkActionResult(_)));

        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran_c = ran.clone();
        spawn_mutation(&gate, &sink, &scan_state, async move {
            ran_c.store(true, std::sync::atomic::Ordering::Release);
        })
        .await;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !ran.load(std::sync::atomic::Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the mutation gate must be free after a panic");
    }

    #[tokio::test]
    async fn queued_mutation_does_not_block_later_control_frames() {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let held = gate.lock().await;
        let (sink, _rx) = Sink::channel_for_test(4);
        let scan_state: Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran_operation = ran.clone();

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            spawn_mutation(&gate, &sink, &scan_state, async move {
                ran_operation.store(true, std::sync::atomic::Ordering::Release);
            }),
        )
        .await
        .expect("queuing a mutation must not wait for the active operation");
        assert!(!ran.load(std::sync::atomic::Ordering::Acquire));

        drop(held);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !ran.load(std::sync::atomic::Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("queued mutation should run after the permit is released");
    }

    #[tokio::test]
    async fn queued_mutation_keeps_fifo_position_across_status_timeouts() {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let held = gate.clone().lock_owned().await;
        let (sink, _rx) = Sink::channel_for_test(4);
        let scan_state: Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let order = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let operation_order = order.clone();
        let (operation_done_tx, operation_done_rx) = tokio::sync::oneshot::channel();

        spawn_mutation_with_interval(
            &gate,
            &sink,
            &scan_state,
            None,
            async move {
                operation_order.lock().await.push("accepted");
                let _ = operation_done_tx.send(());
            },
            std::time::Duration::from_millis(10),
        )
        .await;

        let later_gate = gate.clone();
        let later_order = order.clone();
        let later = tokio::spawn(async move {
            let _permit = later_gate.lock_owned().await;
            later_order.lock().await.push("later");
        });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        drop(held);

        tokio::time::timeout(std::time::Duration::from_secs(1), operation_done_rx)
            .await
            .expect("accepted mutation should complete")
            .expect("accepted mutation completion sender");
        tokio::time::timeout(std::time::Duration::from_secs(1), later)
            .await
            .expect("later waiter should complete")
            .expect("later waiter task");
        assert_eq!(*order.lock().await, ["accepted", "later"]);
    }

    #[test]
    fn scan_reservation_rejects_overlap_without_rearming_cancellation() {
        let active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let guard = reserve_scan(&active, &cancel).expect("first scan reservation");
        assert!(!cancel.load(std::sync::atomic::Ordering::Acquire));

        cancel.store(true, std::sync::atomic::Ordering::Release);
        assert!(reserve_scan(&active, &cancel).is_none());
        assert!(cancel.load(std::sync::atomic::Ordering::Acquire));

        drop(guard);
        let next = reserve_scan(&active, &cancel).expect("reservation released on drop");
        assert!(!cancel.load(std::sync::atomic::Ordering::Acquire));
        drop(next);
    }

    #[tokio::test]
    async fn queued_mutation_observes_cancellation_requested_while_waiting() {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let held = gate.lock().await;
        let (sink, _rx) = Sink::channel_for_test(4);
        let scan_state: Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let operation_cancelled = cancelled.clone();
        let operation_observed = observed.clone();

        spawn_mutation(&gate, &sink, &scan_state, async move {
            operation_observed.store(
                operation_cancelled.load(std::sync::atomic::Ordering::Acquire),
                std::sync::atomic::Ordering::Release,
            );
        })
        .await;
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        drop(held);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !observed.load(std::sync::atomic::Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("queued mutation should retain cancellation set after enqueue");
    }

    // Parent identity is pinned by an OpenProcess handle + creation time, not a bare PID: two processes that ever share a PID cannot share a creation time, closing the PID-reuse TOCTOU. This proves the primitive is stable for a known-live process (ours).
    #[cfg(windows)]
    #[test]
    fn parent_identity_pinned_by_creation_time_not_bare_pid() {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let pid = unsafe { GetCurrentProcessId() };

        let open = || unsafe {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .expect("open self process")
        };

        let h1 = open();
        let t1 = process_creation_time(h1);
        unsafe {
            let _ = CloseHandle(h1);
        }
        assert!(t1.is_some(), "creation time must be capturable for a live process");
        assert_ne!(t1, Some(0), "creation time must be a concrete non-zero value");

        let h2 = open();
        let t2 = process_creation_time(h2);
        unsafe {
            let _ = CloseHandle(h2);
        }
        assert_eq!(t1, t2, "creation time pins a stable identity for a given PID");
    }
}

