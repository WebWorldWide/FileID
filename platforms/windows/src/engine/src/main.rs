//! FileIDEngine — Windows native engine binary.
//!
//! Spawned by the FileID app as a child process. Reads `IPCCommand` JSON
//! frames from stdin (one per line), emits `IPCEvent` JSON frames to stdout
//! (one per line). Local-only logs go to %LOCALAPPDATA%/FileID/logs/ via
//! tracing; nothing leaves the machine.
//!
//! Lifetime is bound to the parent: parent-stdin EOF or the parent process
//! disappearing (detected by a periodic OpenProcess poll on Windows) triggers
//! a clean shutdown — drain the WAL, flush the sink, exit zero.

#![allow(clippy::needless_return)]

mod commands;
mod coordinator;
mod db;
mod downloader;
mod ipc;
mod job_queue;
mod logging;
mod models;
mod paths;
mod pipeline;
mod platform;
mod scan_session;
mod shell;
mod util;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::io::BufReader;
use tokio::sync::Notify;

use ipc::{
    bounded_read::{self, BoundedRead},
    sink::Sink,
    CommandPayload, EngineError, EventPayload, IpcCommand, IpcEvent, JobCategory, Wrap,
};

const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Max inbound IPC frame size. 32 MiB, symmetric with the app's outbound
/// command cap (MaxIpcFrameBytes) and its inbound read cap (MaxFrameChars).
/// A large applyRestructure carries the same multi-MB move set the engine
/// emitted in restructurePlan; the old 1 MiB cap silently rejected + drained
/// it. Still bounded vs a runaway line; the engine already holds the whole
/// plan in memory. (audit E10)
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// Human-readable rejection message for an oversized inbound frame. Derives the
/// cap from MAX_FRAME_BYTES so the text can never drift from the real limit
/// (F-C1-010: the old message hardcoded "1 MB" while the cap was 32 MiB).
fn oversized_frame_message(seen: usize) -> String {
    format!(
        "Command frame exceeded {} MiB cap (saw {} bytes before reject).",
        MAX_FRAME_BYTES / (1024 * 1024),
        seen,
    )
}

fn main() -> Result<()> {
    // SEC-3: lock DLL search FIRST. Must run before tokio::runtime spins
    // worker threads, before logging::init() opens tracing-appender file
    // handles, and before anything else that might trigger an implicit
    // LoadLibrary. Default Windows search includes CWD + every PATH
    // entry — an attacker dropping `onnxruntime_providers_*.dll` in any
    // of those gets code exec when we later register the EP. The
    // SetDefaultDllDirectories call restricts to System32 + the engine
    // binary's directory + AddDllDirectory-registered Performance Pack
    // dirs only.
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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(async_main());
    // Abandon any still-running spawn_blocking tasks (e.g. the parent
    // watchdog's INFINITE WaitForSingleObject). The OS reaps them on
    // process exit; without this, a long-lived blocking task can hold
    // the runtime drop and prevent the process from exiting.
    rt.shutdown_timeout(std::time::Duration::from_millis(0));
    result
}

async fn async_main() -> Result<()> {
    logging::init()?;
    let _ = paths::ensure_state_dirs()?; // create %LOCALAPPDATA%/FileID/{logs,Models,...}
    logging::install_panic_hook();

    tracing::info!(version = ENGINE_VERSION, "FileIDEngine starting");

    // VRAM probe at startup. The ML session-pool sizer clamps pool size to
    // fit available video memory — without this clamp, a larger pool
    // exhausts VRAM on a 6 GB RTX 2060 and wedges the DirectML driver
    // requiring a hard reboot.
    if let Some(vram_mb) = platform::dedicated_vram_mb() {
        tracing::info!(dedicated_vram_mb = vram_mb, "[VRAM] dedicated video memory probed");
    } else {
        tracing::info!("[VRAM] no dedicated GPU detected; ML pool will run at minimum");
    }

    // EP crash-safety gate: if the previous run died while binding a GPU
    // execution provider (a bad/mismatched pack DLL), disable that EP now so
    // this run falls back to DirectML instead of crash-looping. Must run
    // before the ORT_DYLIB_PATH pin below and before the first ML session.
    if let Some(ep) = models::ep_guard::resolve_poison_at_startup() {
        tracing::warn!(ep = %ep, "[EP-GUARD] execution provider disabled after a prior crash; using DirectML until re-enabled");
    }

    // Replay AddDllDirectory for any Performance Packs already extracted from
    // a prior install. Packs land in %LOCALAPPDATA%\FileID\Models\packs\<vendor>\
    // and the llama.cpp runtime lands in Models\llama.cpp\. After SEC-3 locked
    // the default search path, those dirs are invisible until explicitly added.
    // Without this replay step, packs only work on the install run, not on
    // subsequent app launches.
    if let Ok(models_dir) = paths::models_dir() {
        let _ = platform::register_dll_dirs_under(&models_dir.join("packs").join("cuda"));
        let _ = platform::register_dll_dirs_under(&models_dir.join("packs").join("openvino"));
        let _ = platform::register_dll_dirs_under(&models_dir.join("packs").join("qnn"));
        let _ = platform::register_dll_dirs_under(&models_dir.join("llama.cpp"));
        let _ = platform::register_dll_dirs_under(&models_dir.join("llama.cpp-cuda"));
        // Private cuDNN drop (installed on demand via the GPU acceleration pack).
        // The archive extracts a versioned dir containing bin/, so register the
        // parent — register_dll_dirs_under walks subdirs for DLLs.
        let _ = platform::register_dll_dirs_under(&models_dir.join("cudnn"));

        // Accelerator pack: pyke's `download-binaries` ships only the base
        // onnxruntime.dll + onnxruntime_providers_shared.dll (DirectML/CPU) —
        // NOT the vendor provider DLL (onnxruntime_providers_{cuda,openvino}.dll)
        // — so the vendor EP can't bind and we fall through to DirectML
        // (~3-5x slower). The pack supplies a COMPLETE matched ORT runtime for
        // this GPU's vendor (NVIDIA→cuda, Intel→openvino); pin ORT's
        // load-dynamic loader at the pack's onnxruntime.dll so the provider
        // binds against the SAME ORT build (mismatched base vs provider =
        // silent fallback or crash). Guarded on file presence + no pre-existing
        // override + not crash-disabled (ep_guard) → INERT until a matching
        // pack is installed. Must run before the first ORT session.
        if std::env::var_os("ORT_DYLIB_PATH").is_none() {
            if let Some((ep, pack_dir)) = models::runtime::active_pack_dir() {
                if !models::ep_guard::is_disabled(ep) {
                    if let Some(dll) = platform::find_file_under(&pack_dir, "onnxruntime.dll", 4) {
                        tracing::info!(ep, path = %dll.display(), "[EP] accelerator pack present; pinning ORT_DYLIB_PATH to matched runtime");
                        std::env::set_var("ORT_DYLIB_PATH", &dll);
                    }
                }
            }
        }
    }

    // If a system-wide NVIDIA CUDA Toolkit + cuDNN is present, register the
    // toolkit bin dir so ORT's CUDA EP can LoadLibrary the runtime DLLs.
    // SEC-3 locked the default search path to System32 + app dir, so without
    // this step the toolkit on PATH is invisible to the loader. Capture any
    // AddDllDirectory failures so we can surface them via the sink — a silent
    // fallback to DirectML would leave the user wondering why their NVIDIA
    // card isn't being used.
    let cuda_dll_failures: Vec<std::path::PathBuf> =
        if let Some(cuda_bin) = models::runtime::system_cuda_toolkit_dir() {
            tracing::info!(dir = %cuda_bin.display(), "[EP] registering system CUDA toolkit bin dir");
            platform::register_dll_dirs_under(&cuda_bin)
        } else {
            Vec::new()
        };

    // Open the DB up front so migrations apply (and any failure surfaces
    // before we tell the app we're ready). Checkpoint + close on shutdown.
    // Wrapped in Arc<Mutex<…>> so handlers (planRestructure, future
    // applyRestructure, scan_session) can share the single writer.
    let db_path = paths::db_path()?;
    let db_conn: Option<std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>> =
        match db::open_writer(&db_path) {
            Ok(c) => Some(std::sync::Arc::new(parking_lot::Mutex::new(c))),
            Err(err) => {
                tracing::error!(?err, ?db_path, "failed to open database");
                None
            }
        };

    let (sink, sink_writer) = Sink::spawn();

    // Sent before emit_ready so the app can react while still .starting.
    if !cuda_dll_failures.is_empty() {
        let dirs = cuda_dll_failures
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("; ");
        let msg = format!(
            "CUDA Toolkit detected but {} DLL search dir registration(s) failed: {}. Scanning will fall back to DirectML.",
            cuda_dll_failures.len(),
            dirs
        );
        tracing::error!(message = %msg, "[EP] CUDA DLL registration failed");
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "cuda_dll_registration_failed".into(),
            message: msg,
            path: None,
            model_kind: None,
        }))))
        .await;
    }

    if db_conn.is_none() {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "db_open_failed".into(),
            message: format!(
                "Could not open or migrate the database at {}. The engine cannot scan. Check %LOCALAPPDATA%\\FileID\\logs\\engine.jsonl for the underlying error.",
                db_path.display()
            ),
            path: Some(db_path.display().to_string()),
            model_kind: None,
        }))))
        .await;
        tracing::error!("engine starting without a writable DB — aborting");
        // Drop the sink so the writer task sees EOF, then await it
        // (with a cap) to let the error reach the app before exit.
        drop(sink);
        let _ = tokio::time::timeout(Duration::from_secs(2), sink_writer).await;
        return Ok(());
    }

    // Structural integrity check on the now-open writer. A torn page or a
    // truncated file (power-loss mid-checkpoint, failing disk) would otherwise
    // surface later as opaque per-query failures; quick_check catches it once,
    // up front, with actionable guidance. Non-fatal — the engine keeps running
    // so the user can wipe + rescan to rebuild (the DB may be partly readable).
    if let Some(conn) = db_conn.as_ref() {
        // Bind in its own statement so the lock guard drops at the semicolon —
        // a temporary in an `if let` scrutinee would otherwise be held across
        // the `.await` below (clippy::await_holding_lock).
        let verdict = db::quick_check(&conn.lock());
        if let Err(detail) = verdict {
            tracing::error!(%detail, "database failed PRAGMA quick_check");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "db_integrity_check_failed".into(),
                message: format!(
                    "The library database failed an integrity check ({detail}). It may be \
                     corrupted. Open Settings and wipe the library, then run a scan to rebuild it."
                ),
                path: Some(db_path.display().to_string()),
                model_kind: None,
            }))))
            .await;
        }
    }

    // Emit `ready` first thing so the app sidebar can transition out of
    // .starting. The handshake is one-way; the app doesn't ack.
    commands::hardware::emit_ready(&sink).await;

    // Coordinated shutdown signal. set() once, awaited by the stdio loop +
    // the parent watchdog so they cooperate on exit.
    let shutdown = Arc::new(Notify::new());

    // Register the main-task waiter NOW, before any task that could
    // call `notify_waiters()` is spawned. `Notified::enable()` adds
    // the future to Notify's waiter list explicitly; without it, a
    // pinned-but-not-yet-polled Notified is NOT a waiter and an early
    // notify_waiters() fired by the stdio loop on empty-stdin EOF is
    // lost — the engine then hangs forever on .notified().await.
    let main_shutdown = shutdown.notified();
    tokio::pin!(main_shutdown);
    main_shutdown.as_mut().enable();

    // Parent watchdog: wait on a handle to the parent process. When the
    // parent exits, signal shutdown. The parent's IDENTITY is pinned by an
    // OpenProcess handle + creation-time captured here at startup, NOT by the
    // bare PID — a PID can be reused by an unrelated process the instant the
    // real parent exits, and a watchdog bound to a bare PID would then wait on
    // a stranger (F-C1-015 TOCTOU).
    let parent_pid = platform::get_parent_pid();
    if let Some(ppid) = parent_pid {
        let s = shutdown.clone();
        tokio::spawn(async move {
            watch_parent_pinned(ppid, s).await;
        });
    } else {
        tracing::warn!("could not determine parent PID; running without watchdog");
    }

    // Stdio loop: read commands line-by-line, dispatch them.
    //
    // SEC: we DON'T use `BufReader::lines()` because `next_line()` buffers
    // the whole line before returning, which means a hostile no-newline
    // blob can OOM the engine before any cap fires. Instead we use
    // `read_until(b'\n', ...)` against an in-loop `Vec<u8>` and reject
    // anything that crosses the MAX_FRAME_BYTES ceiling mid-read.
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);

    let dispatch_sink = sink.clone();
    let dispatch_shutdown = shutdown.clone();
    let dispatch_db = db_conn.clone();

    // Active scan coordinator. None when no scan is running; populated by
    // StartScan and consulted by PauseScan / ResumeScan / CancelScan.
    let scan_state: Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>> =
        Arc::new(parking_lot::Mutex::new(None));
    let dispatch_scan_state = scan_state.clone();

    // Deep Analyze cancel flag. Single-in-flight; deepAnalyzeCancel sets
    // it; the inner per-file loop polls it between files + during the
    // VLM caption call.
    let deep_analyze_cancel: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dispatch_deep_cancel = deep_analyze_cancel.clone();
    // Single-in-flight gate: a second Deep Analyze command must bounce cleanly
    // rather than re-arm the shared cancel flag (mode A) or cross-cancel the
    // running pass (mode B). Only a successful acquire resets `cancel` (#10).
    let deep_analyze_active: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dispatch_deep_active = deep_analyze_active.clone();
    // Single-flight gate for face clustering: a second runFaceClustering must
    // bounce cleanly rather than race the persist transaction (the writer lock
    // is now released during cluster()+consolidate(), so two runs could overlap).
    let face_cluster_active: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dispatch_face_cluster_active = face_cluster_active.clone();

    // Sidebar job tracker (macOS JobQueue parity): scan / cluster /
    // deep-analyze jobs push on accept and finish via RAII guards;
    // every transition emits a queueState event. Until this wiring the
    // tracker existed but nothing fed it — the app's SidebarQueueList
    // (bound to EngineClient.QueueState) stayed permanently empty.
    let jobs = job_queue::JobQueue::new();
    {
        let qs_sink = sink.clone();
        jobs.on_change(move |state| {
            let _ = qs_sink.try_send(IpcEvent::now(EventPayload::QueueState(Wrap::new(state))));
        });
    }
    let dispatch_jobs = jobs.clone();
    let dispatch_db_path = db_path.clone();

    // Shared HTTP client (HTTP/2 + connection pool) for the 12-way parallel
    // downloader. Built once at engine startup; cloned cheaply via Arc into
    // every prewarm task.
    let http_client = match crate::downloader::build_shared_client() {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(?err, "failed to build shared HTTP client; downloads will fail");
            // Fail CLOSED: a trust-store-empty client refuses every TLS
            // handshake, so a broken pinned-client build can never degrade
            // into unpinned egress. Builder does no I/O; cannot fail here.
            Arc::new(
                reqwest::Client::builder()
                    .tls_built_in_root_certs(false)
                    .build()
                    .expect("no-roots fallback client"),
            )
        }
    };
    let dispatch_http_client = http_client.clone();

    // Prewarm cancellation is per-model-kind, owned by a static registry inside
    // commands::prewarm (see prewarm_cancel_flag / cancel_prewarm) — no global
    // flag to thread through here.
    let stdio_loop = tokio::spawn(async move {
        let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
        loop {
            tokio::select! {
                biased;
                _ = dispatch_shutdown.notified() => {
                    tracing::info!("shutdown notified; stdio loop exiting");
                    break;
                }
                read = bounded_read::bounded_read_line(&mut reader, &mut buf, MAX_FRAME_BYTES) => {
                    match read {
                        // Drop empty lines and BOM-only / whitespace-only lines.
                        // .NET's StreamWriter for Process.StandardInput can push
                        // a UTF-8 BOM on first init, which otherwise trips
                        // serde_json with "expected value at line 1 column 1".
                        Ok(BoundedRead::Line(text))
                            if text
                                .trim_start_matches('\u{FEFF}')
                                .trim()
                                .is_empty() =>
                        {
                            continue;
                        }
                        Ok(BoundedRead::Line(text)) => {
                            handle_line(
                                &dispatch_sink,
                                &dispatch_shutdown,
                                dispatch_db.as_ref(),
                                &dispatch_db_path,
                                &dispatch_scan_state,
                                &dispatch_deep_cancel,
                                &dispatch_deep_active,
                                &dispatch_face_cluster_active,
                                &dispatch_jobs,
                                &dispatch_http_client,
                                &text,
                            ).await;
                        }
                        Ok(BoundedRead::Oversized(seen)) => {
                            // SEC: rejected mid-read, never allocated past the cap.
                            tracing::warn!(bytes_seen = seen, "oversized ipc frame; rejecting");
                            dispatch_sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                                kind: "oversized_ipc_frame".into(),
                                message: oversized_frame_message(seen),
                                path: None,
                                model_kind: None,
                            })))).await;
                            // Drain to next newline so we resync with the next frame.
                            let _ = bounded_read::drain_to_newline(&mut reader).await;
                        }
                        Ok(BoundedRead::Eof) => {
                            tracing::info!("stdin EOF; entering shutdown");
                            dispatch_shutdown.notify_waiters();
                            break;
                        }
                        Err(err) => {
                            tracing::error!(%err, "stdin read error");
                            dispatch_shutdown.notify_waiters();
                            break;
                        }
                    }
                }
            }
        }
    });

    // Wait for shutdown signal (from either source).
    main_shutdown.await;

    // Checkpoint before exit so the next opener doesn't need the .wal/.shm
    // sidecars. On failure, surface to the sink before teardown so the
    // app's next launch can warn about stale-read. Capture into a local
    // so the mutex guard drops before any await on the sink.
    let checkpoint_outcome = db_conn.as_ref().map(|conn_arc| {
        let guard = conn_arc.lock();
        db::checkpoint_truncate(&guard)
    });
    if let Some(Err(err)) = checkpoint_outcome {
        tracing::warn!(?err, "WAL checkpoint at shutdown failed");
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "checkpoint_failed_at_shutdown".into(),
            message: "WAL not truncated at shutdown — your data is safe, but a previous read may show stale state on next launch.".into(),
            path: None,
            model_kind: None,
        }))))
        .await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(db_conn);

    // Tear down stdio loop and sink.
    stdio_loop.abort();
    drop(sink);
    let _ = tokio::time::timeout(Duration::from_secs(2), sink_writer).await;

    tracing::info!("FileIDEngine exiting cleanly");
    Ok(())
}

/// 64-bit process creation time (FILETIME packed hi:lo) used to pin parent
/// identity across a PID-reuse window. Two processes that ever share a PID
/// cannot share a creation time, so this disambiguates a reused PID.
#[cfg(windows)]
fn process_creation_time(handle: windows::Win32::Foundation::HANDLE) -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: handle is a live process handle opened with
    // PROCESS_QUERY_LIMITED_INFORMATION; all four out-params are owned locals.
    let ok = unsafe {
        GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user)
    };
    if ok.is_err() {
        return None;
    }
    Some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
}

/// Watch the parent process for exit and signal `shutdown` when it dies.
///
/// Identity is PINNED: we OpenProcess the PID once here, capture the process'
/// creation time, then re-sample our actual parent PID and confirm it is still
/// `ppid` with the same creation time. The returned handle refers to a specific
/// kernel object; even if `ppid` is later reused by an unrelated process, the
/// handle keeps pointing at the original parent — so the wait can never be
/// redirected onto a stranger. If the parent already vanished (and possibly its
/// PID was already reused) by the time we open it, we detect the creation-time
/// mismatch and signal shutdown immediately rather than babysit the reuser.
#[cfg(windows)]
async fn watch_parent_pinned(ppid: u32, shutdown: Arc<Notify>) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    // SAFETY: ppid is a u32 PID; OpenProcess returns a handle or an error.
    // Reduce the OpenProcess Result/HANDLE (both !Send) to a Send-safe usize
    // BEFORE any await — this async fn is `tokio::spawn`ed, so a !Send value
    // living across the fallback `.await` below would make the whole future
    // !Send and fail to compile on Windows.
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
        // Couldn't get a pinning handle (e.g. PROCESS_QUERY_LIMITED_INFORMATION
        // denied). Fall back to the platform's SYNCHRONIZE-only watcher rather
        // than disabling the watchdog outright — no worse than the pre-fix path.
        tracing::warn!(
            "could not open pinning handle (pid={ppid}); falling back to unpinned watch"
        );
        platform::watch_parent(ppid, shutdown).await;
        return;
    };
    let handle = HANDLE(handle_addr as *mut core::ffi::c_void);

    // Pin identity: capture the opened process' creation time, then re-sample
    // who our parent actually is. If the PID we opened is no longer our parent
    // (the real parent exited and the PID was reused between get_parent_pid()
    // and OpenProcess), the handle points at a stranger — bail to shutdown.
    let pinned_creation = process_creation_time(handle);
    let still_parent = platform::get_parent_pid() == Some(ppid);
    if !still_parent || pinned_creation.is_none() {
        tracing::warn!(
            "parent identity could not be pinned (pid={ppid}); treating parent as gone"
        );
        // SAFETY: handle was a live, owned handle from OpenProcess above.
        unsafe { let _ = CloseHandle(handle); }
        shutdown.notify_waiters();
        return;
    }

    // HANDLE wraps *mut c_void which the windows crate doesn't mark Send.
    // We carry it across the closure boundary as the `handle_addr` usize
    // captured above — integers are always Send. SAFETY: a Win32 HANDLE is
    // opaque; CloseHandle and WaitForSingleObject can be called from any thread
    // once the handle exists. This task is the sole owner of the value.
    let wait = tokio::task::spawn_blocking(move || {
        let h = HANDLE(handle_addr as *mut core::ffi::c_void);
        // SAFETY: h is the pinned parent handle smuggled across threads.
        let r = unsafe { WaitForSingleObject(h, u32::MAX) };
        unsafe { let _ = CloseHandle(h); }
        r
    });

    // Race the blocking wait against the shared shutdown signal so EOF /
    // explicit Shutdown can unblock us when the parent stays alive. If we lose
    // the race, the blocking thread keeps its INFINITE wait until process exit;
    // the runtime's shutdown_timeout(0) ensures it does not delay exit.
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

/// Non-Windows watchdog: delegates to the platform getppid poll. There is no
/// PID-reuse TOCTOU here because POSIX guarantees the parent stays a zombie
/// (reaped by us) until waited on, so the bare PID identity holds.
#[cfg(not(windows))]
async fn watch_parent_pinned(ppid: u32, shutdown: Arc<Notify>) {
    platform::watch_parent(ppid, shutdown).await;
}

/// Clears the Deep-Analyze single-in-flight flag on drop, so the slot frees even
/// if the spawned handler panics or early-returns (#10).
struct DeepActiveGuard(Arc<std::sync::atomic::AtomicBool>);
impl Drop for DeepActiveGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Removes a tracked sidebar job on drop — survives panics and early
/// returns (same RAII shape as DeepActiveGuard), so a dying task can't
/// strand a "running" entry in the app's queue list.
struct JobTrackGuard {
    queue: job_queue::JobQueue,
    id: job_queue::JobId,
}
impl JobTrackGuard {
    fn start(queue: &job_queue::JobQueue, category: JobCategory, title: &str) -> Self {
        let id = queue.push(category, title, None);
        queue.promote_next();
        Self { queue: queue.clone(), id }
    }
}
impl Drop for JobTrackGuard {
    fn drop(&mut self) {
        self.queue.finish(&self.id);
    }
}

/// Clears the face-clustering single-flight flag on drop — releases on task
/// success, error, OR panic (same RAII shape as DeepActiveGuard).
struct FaceClusterActiveGuard(Arc<std::sync::atomic::AtomicBool>);
impl Drop for FaceClusterActiveGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

async fn emit_face_clustering_busy(sink: &Sink) {
    sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
        kind: "face_clustering_busy".into(),
        message: "Face clustering is already running.".into(),
        path: None,
        model_kind: None,
    }))))
    .await;
}

async fn emit_deep_analyze_busy(sink: &Sink) {
    sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
        kind: "deep_analyze_already_running".into(),
        message: "A Deep Analyze pass is already running — wait for it to finish or cancel it first.".into(),
        path: None,
        model_kind: None,
    }))))
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_line(
    sink: &Sink,
    shutdown: &Arc<Notify>,
    db: Option<&std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>>,
    db_path: &std::path::Path,
    scan_state: &Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>>,
    deep_analyze_cancel: &Arc<std::sync::atomic::AtomicBool>,
    deep_analyze_active: &Arc<std::sync::atomic::AtomicBool>,
    face_cluster_active: &Arc<std::sync::atomic::AtomicBool>,
    jobs: &job_queue::JobQueue,
    http_client: &Arc<reqwest::Client>,
    line: &str,
) {
    // Strip a leading UTF-8 BOM defensively. The C# side ships BOM-less UTF-8,
    // but legacy installs or third-party wrappers may push `EF BB BF` on the
    // first byte of stdin. Trim before the deserializer sees it.
    let line = line.trim_start_matches('\u{FEFF}').trim_start();
    if line.is_empty() {
        return;
    }
    let cmd: IpcCommand = match serde_json::from_str(line) {
        Ok(c) => c,
        Err(err) => {
            // IPC parity with macOS: emit command_decode_failed so a
            // slightly-newer app learns its command was dropped. The C#
            // app classifies the kind as a non-fatal warning, so this
            // never paints the red toast the old silencing avoided.
            // serde's error carries position info, not the line content,
            // so no user path can leak through the message.
            tracing::warn!(%err, "ipc decode failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "command_decode_failed".into(),
                message: format!("Command frame could not be decoded: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
            return;
        }
    };

    match cmd.payload {
        CommandPayload::RequestStatus(_) => {
            // Re-emit ready so the app can rebuild its EngineInfo snapshot.
            commands::hardware::emit_ready(sink).await;
        }
        CommandPayload::VerifyCudaPack(_) => {
            commands::hardware::handle_verify_cuda_pack(sink).await;
        }
        CommandPayload::Shutdown(_) => {
            tracing::info!("shutdown command received");
            shutdown.notify_waiters();
        }
        CommandPayload::PrewarmModel(payload) => {
            // Reset THIS kind's cancel flag SYNCHRONOUSLY, before the spawn, so a
            // CancelPrewarm dispatched right after lands on an existing flag (the
            // serial stdio loop runs this before reading the next frame) — closing
            // the lazy-create race. Resets only this kind; others are untouched.
            commands::prewarm::reset_prewarm_cancel(&payload.model_kind);
            let sink = sink.clone();
            let model_kind = payload.model_kind.clone();
            let http_client = http_client.clone();
            tokio::spawn(async move {
                commands::prewarm::handle_prewarm_model(sink, model_kind, http_client).await;
            });
        }
        CommandPayload::CancelPrewarm(payload) => {
            // Per-model when modelKind is set (one row's Cancel), all in-flight
            // when None (the legacy payload-less "cancel everything").
            commands::prewarm::cancel_prewarm(payload.model_kind.as_deref());
        }
        CommandPayload::PlanRestructure(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "planRestructure").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::restructure::handle_plan_restructure(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::ApplyRestructure(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "applyRestructure").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::restructure::handle_apply_restructure(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::StartScan(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "startScan").await;
                return;
            };
            let track = JobTrackGuard::start(jobs, JobCategory::Scan, "Scan library");
            let sink_c = sink.clone();
            let db_c = db.clone();
            let state_c = scan_state.clone();
            tokio::spawn(async move {
                let _track = track;
                commands::scan::handle_start_scan(sink_c, db_c, state_c, payload).await;
            });
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
            if let Some(coord) = scan_state.lock().as_ref() {
                coord.request_cancel();
                tracing::info!("scan cancel requested");
            }
        }
        CommandPayload::ApplyTags(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "applyTags").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::bulk::handle_apply_tags(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::RenameFiles(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "renameFiles").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::bulk::handle_rename_files(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::TrashFiles(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "trashFiles").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::bulk::handle_trash_files(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::MergeClusters(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "mergeClusters").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::bulk::handle_merge_clusters(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::EmbedTextQuery(payload) => {
            let sink_c = sink.clone();
            tokio::spawn(async move {
                commands::embed::handle_embed_text_query(sink_c, payload).await;
            });
        }
        CommandPayload::RenamePerson(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "renamePerson").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::bulk::handle_rename_person(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::MarkPersonsAsUnknown(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "markPersonsAsUnknown").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::bulk::handle_mark_persons_as_unknown(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::FindMergeSuggestions(_) => {
            // Availability guard preserved (no DB → no read connection), but the
            // handler opens its own read-only connection from db_path so it never
            // contends on the writer mutex.
            let Some(_db) = db else {
                emit_db_unavailable(sink, "findMergeSuggestions").await;
                return;
            };
            let sink_c = sink.clone();
            let path_c = db_path.to_path_buf();
            tokio::spawn(async move {
                commands::bulk::handle_find_merge_suggestions(sink_c, path_c).await;
            });
        }
        CommandPayload::MarkPersonsDifferent(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "markPersonsDifferent").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::bulk::handle_mark_persons_different(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::EmbedImageQuery(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "embedImageQuery").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::embed::handle_embed_image_query(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::DeepAnalyzeFile(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "deepAnalyzeFile").await;
                return;
            };
            if deep_analyze_active
                .compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Relaxed)
                .is_err()
            {
                emit_deep_analyze_busy(sink).await;
                return;
            }
            deep_analyze_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
            let guard = DeepActiveGuard(deep_analyze_active.clone());
            let track = JobTrackGuard::start(jobs, JobCategory::DeepAnalyze, "Deep Analyze 1 file");
            let sink_c = sink.clone();
            let db_c = db.clone();
            let cancel = deep_analyze_cancel.clone();
            tokio::spawn(async move {
                let _guard = guard;
                let _track = track;
                commands::deep_analyze::handle_deep_analyze_file(sink_c, db_c, payload, cancel).await;
            });
        }
        CommandPayload::DeepAnalyzeFolder(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "deepAnalyzeFolder").await;
                return;
            };
            if deep_analyze_active
                .compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Relaxed)
                .is_err()
            {
                emit_deep_analyze_busy(sink).await;
                return;
            }
            deep_analyze_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
            let guard = DeepActiveGuard(deep_analyze_active.clone());
            let track = JobTrackGuard::start(jobs, JobCategory::DeepAnalyze, "Deep Analyze folder");
            let sink_c = sink.clone();
            let db_c = db.clone();
            let cancel = deep_analyze_cancel.clone();
            tokio::spawn(async move {
                let _guard = guard;
                let _track = track;
                commands::deep_analyze::handle_deep_analyze_folder(sink_c, db_c, payload, cancel).await;
            });
        }
        CommandPayload::DeepAnalyzeAll(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "deepAnalyzeAll").await;
                return;
            };
            if deep_analyze_active
                .compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Relaxed)
                .is_err()
            {
                emit_deep_analyze_busy(sink).await;
                return;
            }
            deep_analyze_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
            let guard = DeepActiveGuard(deep_analyze_active.clone());
            let track = JobTrackGuard::start(jobs, JobCategory::DeepAnalyze, "Deep Analyze entire library");
            let sink_c = sink.clone();
            let db_c = db.clone();
            let cancel = deep_analyze_cancel.clone();
            tokio::spawn(async move {
                let _guard = guard;
                let _track = track;
                commands::deep_analyze::handle_deep_analyze_all(sink_c, db_c, payload, cancel).await;
            });
        }
        CommandPayload::DeepAnalyzeCancel(_) => {
            deep_analyze_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("deep analyze cancel requested");
        }
        CommandPayload::RestoreFromTrash(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "restoreFromTrash").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::trash::handle_restore_from_trash(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::RevertMerge(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "revertMerge").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                commands::trash::handle_revert_merge(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::RunFaceClustering(_) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "runFaceClustering").await;
                return;
            };
            // Single-flight: the handler now drops the writer lock during
            // cluster()+consolidate(), so two overlapping runs could race the
            // persist transaction. Bounce a second request rather than spawn it.
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
            let track = JobTrackGuard::start(jobs, JobCategory::FaceCluster, "Cluster faces");
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                let _guard = guard;
                let _track = track;
                commands::face_clustering::handle_run_face_clustering(sink_c, db_c).await;
            });
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
            tokio::spawn(async move {
                commands::wipe::handle_wipe_library(sink_c, db_c, state_c, fca_c).await;
            });
        }
        CommandPayload::GenerateVideoThumbnail(payload) => {
            let sink_c = sink.clone();
            tokio::spawn(async move {
                commands::thumbnail::handle_generate_video_thumbnail(sink_c, payload).await;
            });
        }
    }
}

async fn emit_db_unavailable(sink: &Sink, command: &str) {
    sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
        kind: "db_unavailable".into(),
        message: format!(
            "Cannot run '{command}': the engine couldn't open the SQLite database \
             at startup. Check %LOCALAPPDATA%\\FileID\\logs\\ for the open error."
        ),
        path: None,
        model_kind: None,
    }))))
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    // F-C1-010: the oversize-frame rejection text must report the REAL cap
    // (MAX_FRAME_BYTES = 32 MiB), not the stale hardcoded "1 MB". Pre-fix this
    // said "exceeded 1 MB cap"; post-fix it is derived from MAX_FRAME_BYTES.
    #[test]
    fn oversize_frame_error_text_matches_max_frame_bytes() {
        let msg = oversized_frame_message(99_999);

        let expected_mib = MAX_FRAME_BYTES / (1024 * 1024);
        assert_eq!(expected_mib, 32, "cap is 32 MiB");

        assert!(
            msg.contains(&format!("{expected_mib} MiB")),
            "message must state the real cap ({expected_mib} MiB): {msg}"
        );
        // The pre-fix bug: a literal "1 MB" cap that no longer matches reality.
        assert!(
            !msg.contains("1 MB"),
            "message must not report the stale 1 MB cap: {msg}"
        );
        // Still reports the observed byte count so logs stay actionable.
        assert!(msg.contains("99999"), "message must include the seen byte count: {msg}");
    }

    // F-C1-015: parent identity is pinned by an OpenProcess handle + the
    // process creation time, not a bare PID. A creation time is a per-process-
    // object value: two processes that ever share a PID cannot share it, so
    // capturing it at startup closes the PID-reuse TOCTOU. Here we prove the
    // pinning primitive works and is stable for a known-live process (ours).
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

        // Re-opening the SAME pid yields the SAME creation time: identity is
        // bound to the process object, so the pin is stable and reusable.
        let h2 = open();
        let t2 = process_creation_time(h2);
        unsafe {
            let _ = CloseHandle(h2);
        }
        assert_eq!(t1, t2, "creation time pins a stable identity for a given PID");
    }
}



