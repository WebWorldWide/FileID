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
//!
//! This is the Phase 0 cut: it stands up the IPC loop, the parent watchdog,
//! settings/state directories, structured logging, and the response shell
//! for `ready` / `requestStatus` / `shutdown`. The real ML pipeline,
//! database, scan coordinator, and downloader land in subsequent phases.

#![allow(clippy::needless_return)]

mod coordinator;
mod db;
mod downloader;
mod ipc;
mod job_queue;
mod models;
mod paths;
mod pipeline;
mod platform;
mod scan_session;
mod shell;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::sync::Notify;

use ipc::{
    sink::Sink, CommandPayload, EngineError, EngineInfo, EventPayload, IpcCommand,
    IpcEvent, Wrap,
};

const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> Result<()> {
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
    init_tracing()?;
    let _ = paths::ensure_state_dirs()?; // create %LOCALAPPDATA%/FileID/{logs,Models,...}

    // F2 (V14.8.3): capture every panic in app.log. Without this hook, a
    // panic anywhere in the scan pipeline (ORT session create on a corrupt
    // model, an unwrap() in a worker, anything) crashes the engine silently
    // — the C# app sees a broken pipe and the user sees "the app crashed"
    // with no traceable cause. The hook leaves default unwinding behavior
    // intact (engine still exits on panic, which is correct); it just
    // forces a tracing::error line first so the next crash report has a
    // file:line + backtrace to point at.
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "(unknown location)".to_string());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(
            location = %location,
            message = %message,
            backtrace = %backtrace,
            "engine panic"
        );
    }));

    // SEC-3: lock down DLL search path before any LoadLibrary. Default
    // search includes the engine's CWD (which may be writable user space)
    // + every PATH entry. An attacker who drops `onnxruntime_providers_*.dll`
    // in any of those gets code execution next time we load the EP.
    // SetDefaultDllDirectories restricts to System32 + the engine binary's
    // directory only.
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
        // USER_DIRS is included so AddDllDirectory()'d Performance Pack
        // dirs work -- but the default no-PATH posture defends against
        // PATH-based DLL planting.
    }

    tracing::info!(version = ENGINE_VERSION, "FileIDEngine starting");

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
    }

    // F3b (V14.8.3): if the user has NVIDIA CUDA Toolkit + cuDNN installed
    // system-wide (common on ML/research/dev machines), register the toolkit
    // bin directory so ORT's CUDA EP can LoadLibrary the runtime DLLs. SEC-3
    // locked the default search path to System32 + app dir, so without this
    // step the toolkit on PATH would be invisible to the loader. Falls back
    // silently if no toolkit is present.
    //
    // V14.9 (2.1): capture any AddDllDirectory failures so we can surface
    // them via the IPC sink once it's built. Without this, the CUDA EP would
    // silently fall back to DirectML and the user would have no idea why
    // their NVIDIA card isn't being used.
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

    // Emit `ready` first thing so the app sidebar can transition out of
    // .starting. The handshake is one-way; the app doesn't ack.
    emit_ready(&sink).await;

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

    // Parent watchdog: poll OpenProcess(parent_pid) every 5 s. If parent is
    // gone or our handle to it is invalid, set the shutdown notifier.
    let parent_pid = platform::get_parent_pid();
    if let Some(ppid) = parent_pid {
        let s = shutdown.clone();
        tokio::spawn(async move {
            platform::watch_parent(ppid, s).await;
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
    // anything that crosses the 1 MB ceiling mid-read.
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

    // V14.7.4: shared HTTP client (HTTP/2 + connection pool) for the
    // 12-way parallel downloader. Built once at engine startup; cloned
    // cheaply via Arc into every prewarm task.
    let http_client = match crate::downloader::build_shared_client() {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(?err, "failed to build shared HTTP client; downloads will fail");
            // Stub a minimal client so the engine can still start.
            Arc::new(reqwest::Client::new())
        }
    };
    let dispatch_http_client = http_client.clone();

    // V14.7.4: prewarm cancel flag. CancelPrewarm flips it; the
    // download_parallel inner loop polls it after every chunk.
    let prewarm_cancel: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dispatch_prewarm_cancel = prewarm_cancel.clone();

    let stdio_loop = tokio::spawn(async move {
        const MAX_FRAME_BYTES: usize = 1024 * 1024;
        let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
        loop {
            tokio::select! {
                biased;
                _ = dispatch_shutdown.notified() => {
                    tracing::info!("shutdown notified; stdio loop exiting");
                    break;
                }
                read = bounded_read_line(&mut reader, &mut buf, MAX_FRAME_BYTES) => {
                    match read {
                        // V14.9-Bug1: drop empty lines AND lines that are
                        // nothing but a BOM (`\u{FEFF}`) or other zero-info
                        // whitespace. .NET's StreamWriter for Process.
                        // StandardInput can push a UTF-8 BOM on first
                        // init, which otherwise lands here as a single
                        // codepoint and trips serde_json with
                        // "expected value at line 1 column 1".
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
                                &dispatch_scan_state,
                                &dispatch_deep_cancel,
                                &dispatch_http_client,
                                &dispatch_prewarm_cancel,
                                &text,
                            ).await;
                        }
                        Ok(BoundedRead::Oversized(seen)) => {
                            // SEC: rejected mid-read, never allocated past the cap.
                            tracing::warn!(bytes_seen = seen, "oversized ipc frame; rejecting");
                            dispatch_sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                                kind: "oversized_ipc_frame".into(),
                                message: format!("Command frame exceeded 1 MB cap (saw {} bytes before reject).", seen),
                                path: None,
                                model_kind: None,
                            })))).await;
                            // Drain to next newline so we resync with the next frame.
                            let _ = drain_to_newline(&mut reader).await;
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

async fn handle_line(
    sink: &Sink,
    shutdown: &Arc<Notify>,
    db: Option<&std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>>,
    scan_state: &Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>>,
    deep_analyze_cancel: &Arc<std::sync::atomic::AtomicBool>,
    http_client: &Arc<reqwest::Client>,
    prewarm_cancel: &Arc<std::sync::atomic::AtomicBool>,
    line: &str,
) {
    // V14.9-Bug1: strip a leading UTF-8 BOM defensively. The C# side
    // (EngineClient.cs ProcessStartInfo) was switched to BOM-less
    // UTF-8, but legacy installs or third-party wrappers may still
    // push `EF BB BF` on the first byte of stdin. Trim before the
    // deserializer sees it.
    let line = line.trim_start_matches('\u{FEFF}').trim_start();
    if line.is_empty() {
        return;
    }
    let cmd: IpcCommand = match serde_json::from_str(line) {
        Ok(c) => c,
        Err(err) => {
            // V14.9-Bug1: decode failures used to bubble up as a red
            // toast (`ipc_decode_failed` EngineError emitted to the
            // sink). That's diagnostic noise the user can't act on —
            // any stray byte on the pipe would alarm them. Log at
            // warn level so the engine.jsonl still records the event
            // for debugging, but DON'T surface it in the UI.
            tracing::warn!(%err, "ipc decode failed (silenced)");
            return;
        }
    };

    match cmd.payload {
        CommandPayload::RequestStatus(_) => {
            // Re-emit ready so the app can rebuild its EngineInfo snapshot.
            emit_ready(sink).await;
        }
        CommandPayload::VerifyCudaPack(_) => {
            handle_verify_cuda_pack(sink).await;
        }
        CommandPayload::Shutdown(_) => {
            tracing::info!("shutdown command received");
            shutdown.notify_waiters();
        }
        CommandPayload::PrewarmModel(payload) => {
            // V14.7.4: clear the cancel flag at the start of every NEW
            // prewarm call (an in-flight cancel from a prior call shouldn't
            // immediately abort this one). Downloads from different prewarm
            // calls run concurrently against the shared http_client pool.
            prewarm_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
            let sink = sink.clone();
            let model_kind = payload.model_kind.clone();
            let http_client = http_client.clone();
            let cancel = prewarm_cancel.clone();
            tokio::spawn(async move {
                handle_prewarm_model(sink, model_kind, http_client, cancel).await;
            });
        }
        CommandPayload::CancelPrewarm(_) => {
            // V14.7.4: previously parsed but silently dropped. Now actually
            // cancels the in-flight prewarm by flipping the AtomicBool.
            prewarm_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("CancelPrewarm received; in-flight downloads will abort");
        }
        CommandPayload::PlanRestructure(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "planRestructure").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                handle_plan_restructure(sink_c, db_c, payload).await;
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
                handle_apply_restructure(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::StartScan(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "startScan").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            let state_c = scan_state.clone();
            tokio::spawn(async move {
                handle_start_scan(sink_c, db_c, state_c, payload).await;
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
                handle_apply_tags(sink_c, db_c, payload).await;
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
                handle_rename_files(sink_c, db_c, payload).await;
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
                handle_trash_files(sink_c, db_c, payload).await;
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
                handle_merge_clusters(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::EmbedTextQuery(payload) => {
            let sink_c = sink.clone();
            tokio::spawn(async move {
                handle_embed_text_query(sink_c, payload).await;
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
                handle_rename_person(sink_c, db_c, payload).await;
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
                handle_mark_persons_as_unknown(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::FindMergeSuggestions(_) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "findMergeSuggestions").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                handle_find_merge_suggestions(sink_c, db_c).await;
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
                handle_embed_image_query(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::DeepAnalyzeFile(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "deepAnalyzeFile").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            let cancel = deep_analyze_cancel.clone();
            cancel.store(false, std::sync::atomic::Ordering::Relaxed);
            tokio::spawn(async move {
                handle_deep_analyze_file(sink_c, db_c, payload, cancel).await;
            });
        }
        CommandPayload::DeepAnalyzeFolder(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "deepAnalyzeFolder").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            let cancel = deep_analyze_cancel.clone();
            cancel.store(false, std::sync::atomic::Ordering::Relaxed);
            tokio::spawn(async move {
                handle_deep_analyze_folder(sink_c, db_c, payload, cancel).await;
            });
        }
        CommandPayload::DeepAnalyzeAll(payload) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "deepAnalyzeAll").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            let cancel = deep_analyze_cancel.clone();
            cancel.store(false, std::sync::atomic::Ordering::Relaxed);
            tokio::spawn(async move {
                handle_deep_analyze_all(sink_c, db_c, payload, cancel).await;
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
                handle_restore_from_trash(sink_c, db_c, payload).await;
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
                handle_revert_merge(sink_c, db_c, payload).await;
            });
        }
        CommandPayload::RunFaceClustering(_) => {
            let Some(db) = db else {
                emit_db_unavailable(sink, "runFaceClustering").await;
                return;
            };
            let sink_c = sink.clone();
            let db_c = db.clone();
            tokio::spawn(async move {
                handle_run_face_clustering(sink_c, db_c).await;
            });
        }
    }
}

/// Returns true iff `name` is exactly one Normal path component:
/// no slashes, no "..", no ".", no drive letter, no UNC, no leading/trailing
/// whitespace that the OS would silently strip. Used as the path-traversal
/// guard for `renameFiles`. Conservative — extra reject is safer than
/// extra allow when the destination is computed by joining to a directory.
fn is_safe_filename(name: &str) -> bool {
    use std::path::Component;
    if name.is_empty() || name.trim() != name {
        return false;
    }
    if name == "." || name == ".." {
        return false;
    }
    // SEC: trailing dot or space is a Windows quirk that resolves to a
    // different file than the literal name. Reject either side.
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    let p = std::path::Path::new(name);
    if p.is_absolute() {
        return false;
    }
    let mut comps = p.components();
    let first = match comps.next() {
        Some(c) => c,
        None => return false,
    };
    if comps.next().is_some() {
        return false; // multi-component path — definitely not a filename
    }
    if !matches!(first, Component::Normal(_)) {
        return false;
    }
    // SEC: reject Windows reserved names (CON, PRN, AUX, NUL, COM1..9,
    // LPT1..9), with or without an extension. MoveFileExW returns
    // cryptic errors and on some shells "rename to NUL" silently
    // discards the file.
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
            | "COM1" | "COM2" | "COM3" | "COM4" | "COM5"
            | "COM6" | "COM7" | "COM8" | "COM9"
            | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5"
            | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    )
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

/// Walk the `files` table for the picked library root, classify each
/// file via `pipeline::restructure::classify`, emit a `restructurePlan`
/// event with the proposed moves + per-category counts. The app's
/// Restructure tab consumes this to render the Sankey + tree-diff.
async fn handle_plan_restructure(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::PlanRestructurePayload,
) {
    use crate::ipc::{
        RestructureCategoryCount, RestructureMove as IpcMove, RestructurePlan,
    };
    use crate::pipeline::discovery::FileKind;
    use crate::pipeline::restructure::{classify, FileForClassify};
    use std::path::PathBuf;

    // Snapshot the table on a blocking thread so we don't park the
    // tokio runtime on a long SQLite scan.
    let library_root = payload.library_root.clone();
    let files: Vec<FileForClassify> = match tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<FileForClassify>> {
        let conn = db.lock();
        let mut stmt = conn.prepare(
            "SELECT id, path_text, kind, modified_at FROM files WHERE failed = 0",
        )?;
        let rows = stmt.query_map([], |row| {
            let kind_str: String = row.get(2)?;
            let kind = match kind_str.as_str() {
                "image" => FileKind::Image,
                "video" => FileKind::Video,
                "pdf" => FileKind::Pdf,
                "doc" => FileKind::Doc,
                "audio" => FileKind::Audio,
                _ => FileKind::Other,
            };
            let modified: Option<f64> = row.get(3)?;
            Ok(FileForClassify {
                file_id: row.get(0)?,
                source: PathBuf::from(row.get::<_, String>(1)?),
                kind,
                modified_unix: modified.unwrap_or(0.0),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => {
            tracing::warn!(?err, "planRestructure query failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "plan_restructure_db".into(),
                message: format!("Couldn't read files table: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
            return;
        }
        Err(err) => {
            tracing::warn!(?err, "planRestructure spawn_blocking failed");
            return;
        }
    };

    let library_root_path = std::path::Path::new(&library_root);
    let proposed = classify(&files, library_root_path);
    let category_summary = crate::pipeline::restructure::category_counts(&proposed);

    // V14.7.2: engine-authoritative folder classification.
    let folder_class = crate::pipeline::restructure::classify_folders(&proposed);
    let mut anchor = 0u32;
    let mut mixed = 0u32;
    let mut junk = 0u32;
    // V14.9 A7: index classification by source folder so we can stamp
    // per-move tiers without re-classifying.
    let mut tier_by_folder: std::collections::HashMap<PathBuf, &'static str> =
        std::collections::HashMap::with_capacity(folder_class.len());
    for f in &folder_class {
        let tier_label = match f.classification {
            crate::pipeline::restructure::FolderClassification::Anchor => { anchor += 1; "Anchor" }
            crate::pipeline::restructure::FolderClassification::Mixed  => { mixed  += 1; "Mixed"  }
            crate::pipeline::restructure::FolderClassification::Junk   => { junk   += 1; "Junk"   }
        };
        tier_by_folder.insert(f.source_folder.clone(), tier_label);
    }

    let plan = RestructurePlan {
        library_root,
        moves: proposed
            .into_iter()
            .map(|m| {
                let tier = m.source
                    .parent()
                    .and_then(|p| tier_by_folder.get(p))
                    .map(|s| (*s).to_string());
                IpcMove {
                    file_id: m.file_id,
                    source: m.source.to_string_lossy().to_string(),
                    destination: m.destination.to_string_lossy().to_string(),
                    category: m.category,
                    tier,
                }
            })
            .collect(),
        category_counts: category_summary
            .into_iter()
            .map(|c| RestructureCategoryCount { category: c.category, count: c.count })
            .collect(),
        folder_classifications: Some(crate::ipc::FolderClassificationCounts {
            anchor_folders: anchor,
            mixed_folders: mixed,
            junk_folders: junk,
        }),
    };

    sink.send(IpcEvent::now(EventPayload::RestructurePlan(Wrap::new(plan))))
        .await;
}

/// Apply a previously-planned set of moves on disk + update DB rows.
/// Path-traversal safe (every destination must canonicalize to inside
/// the library root); supports symlink mode for non-destructive preview.
async fn handle_apply_restructure(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::ApplyRestructurePayload,
) {
    use crate::pipeline::restructure_apply::RestructureApply;
    use std::path::PathBuf;

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<ipc::RestructureApplyResult> {
        let apply = RestructureApply::new(
            db,
            PathBuf::from(payload.library_root),
            payload.use_symlinks,
        );
        apply.apply(&payload.moves)
    })
    .await;

    match result {
        Ok(Ok(r)) => {
            sink.send(IpcEvent::now(EventPayload::RestructureApplyResult(Wrap::new(r))))
                .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "applyRestructure failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "apply_restructure".into(),
                message: format!("Apply failed: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "applyRestructure spawn_blocking failed");
        }
    }
}

/// V14.7.5 helper: RAII guard that removes a model_kind from the
/// in-flight set when the prewarm function returns / unwinds.
struct ReleaseOnDrop {
    kind: String,
    set: &'static parking_lot::Mutex<std::collections::HashSet<String>>,
}
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.set.lock().remove(&self.kind);
    }
}

/// Download every file in the requested model bundle, emit progress
/// events as bytes flow, drop a `.fileid-installed` sentinel when every
/// file lands successfully. The app's ModelInstallerService polls for
/// the sentinel to flip its per-model status to `Installed`.
async fn handle_prewarm_model(
    sink: Sink,
    model_kind: String,
    http_client: Arc<reqwest::Client>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) {
    use crate::downloader::{download_parallel, DownloadRequest};
    use crate::ipc::ModelDownloadProgress;
    use models::registry::LookupResult;

    // F1 (V14.8.3): emit an immediate "Queued" progress event so the welcome
    // sheet row flips out of the captionless-spinner state the moment the
    // engine sees the prewarm command. Without this, the row spinner shows
    // for ~1-3s while the registry lookup + HTTP handshake run before the
    // first real progress event arrives — and users read "spinning with no
    // text" as "stuck waiting for other downloads."
    sink.send(IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
        ModelDownloadProgress {
            model_kind: model_kind.clone(),
            fraction: 0.0,
            message: "Queued — starting download…".to_string(),
            bytes_done: None,
            total_bytes: None,
        },
    )))).await;

    tracing::info!(model_kind = %model_kind, "[PREWARM] entered handler");

    // V14.7.5: per-model-kind in-flight dedupe. Without this, repeated
    // clicks of "Install all" race on the .part file: first call's
    // rename(.part -> final) runs while a second call still streams
    // bytes into the same .part, then the second call's rename trips
    // "os error 2: file not found." Fix: a global Mutex<HashSet<String>>
    // — if model_kind is already mid-download, send a friendly progress
    // event saying "still downloading" and bail.
    static IN_FLIGHT: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let in_flight = IN_FLIGHT.get_or_init(|| parking_lot::Mutex::new(std::collections::HashSet::new()));
    // Check + insert under a short-lived guard. parking_lot's MutexGuard
    // is !Send, so we MUST drop it before the first .await below.
    let already_in_flight = {
        let mut guard = in_flight.lock();
        if guard.contains(&model_kind) {
            true
        } else {
            guard.insert(model_kind.clone());
            false
        }
    };
    if already_in_flight {
        tracing::info!(model = %model_kind, "prewarm already in flight; ignoring duplicate request");
        sink.send(IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
            ModelDownloadProgress {
                model_kind: model_kind.clone(),
                fraction: 0.0,
                message: "Already downloading...".to_string(),
                bytes_done: None,
                total_bytes: None,
            },
        )))).await;
        tracing::info!(model_kind = %model_kind, outcome = "duplicate_in_flight", "[PREWARM] exiting");
        return;
    }
    // RAII guard removes us from the set on every exit path.
    let _release = ReleaseOnDrop {
        kind: model_kind.clone(),
        set: in_flight,
    };

    let model = match models::registry::lookup_full(&model_kind) {
        LookupResult::Found(m) => m,
        LookupResult::NotYetAvailable { display_name, message } => {
            // Deliberate "not wired yet" — the app's Welcome sheet routes
            // CLIP + VLM here for now. Show the user a friendly note via
            // the same ModelDownloadProgress channel the row binds to,
            // not an error popup. Fraction stays at 0 so the row sticks
            // at NotInstalled state.
            sink.send(IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
                ModelDownloadProgress {
                    model_kind: model_kind.clone(),
                    fraction: 0.0,
                    message: format!("{display_name}: {message}"),
                    bytes_done: None,
                    total_bytes: None,
                },
            ))))
            .await;
            tracing::info!(model_kind = %model_kind, outcome = "not_yet_available", "[PREWARM] exiting");
            return;
        }
        LookupResult::Unknown => {
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "unknown_model".into(),
                message: format!(
                    "Model '{model_kind}' is not registered. \
                     Add it to engine/src/models/registry.rs."
                ),
                path: None,
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            tracing::warn!(model_kind = %model_kind, outcome = "unknown_model", "[PREWARM] exiting");
            return;
        }
    };

    tracing::info!(model = %model.id, files = model.files.len(), "starting prewarm");

    // Already-installed short-circuit.
    if let Some(sentinel) = models::registry::sentinel_path(&model) {
        if sentinel.exists() {
            sink.send(IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
                ModelDownloadProgress {
                    model_kind: model_kind.clone(),
                    fraction: 1.0,
                    message: format!("{} already installed", model.display_name),
                    bytes_done: None,
                    total_bytes: None,
                },
            ))))
            .await;
            tracing::info!(model_kind = %model_kind, outcome = "already_installed", "[PREWARM] exiting");
            return;
        }
    }

    let total_bytes_estimate: u64 = model.files.iter().map(|f| f.approx_bytes).sum();
    let total_estimate = total_bytes_estimate.max(1);
    let file_count = model.files.len();

    // Per-file bytes_done counters. All file progress callbacks update
    // their own slot so the aggregate fraction stays correct when
    // multiple files run concurrently. Without this MobileCLIP-S2 (4
    // files) advanced 4× slower than ArcFace/Qwen (1 file each).
    let per_file_done: std::sync::Arc<Vec<std::sync::atomic::AtomicU64>> =
        std::sync::Arc::new(
            (0..file_count)
                .map(|_| std::sync::atomic::AtomicU64::new(0))
                .collect(),
        );

    // Partition into regular vs zip files. Zip files run sequentially
    // because the post-download extract step mutates DLL search paths
    // and is order-sensitive (Performance Packs).
    let mut regular_indices: Vec<usize> = Vec::with_capacity(file_count);
    let mut zip_indices: Vec<usize> = Vec::with_capacity(file_count);
    for (idx, file) in model.files.iter().enumerate() {
        if file.dest.extension().and_then(|s| s.to_str()) == Some("zip") {
            zip_indices.push(idx);
        } else {
            regular_indices.push(idx);
        }
    }

    // Builds the per-file progress callback. All callbacks share the
    // `per_file_done` slot vec; each one writes its own slot and reads
    // the aggregate sum on every emit.
    let make_progress_cb = |idx: usize, file: &models::registry::ModelFile, label: String| {
        let model_kind_local = model_kind.clone();
        let display_name = model.display_name.to_string();
        let sink_for_progress = sink.clone();
        let per_file_done = per_file_done.clone();
        let file_bytes_mb = file.approx_bytes / (1024 * 1024);
        let file_label_for_msg = label;
        move |p: crate::downloader::DownloadProgress| {
            per_file_done[idx].store(p.bytes_done, std::sync::atomic::Ordering::Relaxed);
            let cur_total: u64 = per_file_done
                .iter()
                .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                .sum();
            let fraction = (cur_total as f64) / (total_estimate as f64);
            let msg = if file_count == 1 {
                format!("Downloading {display_name}…")
            } else {
                format!(
                    "Downloading {display_name} — {file_label_for_msg} ({of} of {total}, ~{mb} MB)",
                    of = idx + 1,
                    total = file_count,
                    mb = file_bytes_mb,
                )
            };
            let event = IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
                ModelDownloadProgress {
                    model_kind: model_kind_local.clone(),
                    fraction: fraction.min(0.999),
                    message: msg,
                    bytes_done: Some(cur_total),
                    total_bytes: Some(total_estimate),
                },
            )));
            let s = sink_for_progress.clone();
            tokio::spawn(async move { s.send(event).await; });
        }
    };

    // Run regular (non-zip) files in parallel. The downloader's global
    // HTTP semaphore caps total concurrent requests so this doesn't
    // worsen cross-model rate-limit pressure.
    let mut regular_handles: Vec<tokio::task::JoinHandle<Result<(), (String, std::path::PathBuf, anyhow::Error)>>> =
        Vec::with_capacity(regular_indices.len());
    for idx in regular_indices.iter().copied() {
        let file = model.files[idx].clone();
        let label = file
            .dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let file_bytes_mb = file.approx_bytes / (1024 * 1024);
        tracing::info!(
            model_kind = %model_kind,
            file_idx = idx,
            file_total = file_count,
            file = %label,
            file_mb = file_bytes_mb,
            "[PREWARM] starting file (parallel)"
        );
        let http_client = http_client.clone();
        let cancel = cancel.clone();
        let progress_cb = make_progress_cb(idx, &file, label.clone());
        let req = DownloadRequest {
            url: file.url.clone(),
            destination: file.dest.clone(),
            expected_sha256: file.sha256.clone(),
        };
        let label_for_err = label.clone();
        let dest_for_err = file.dest.clone();
        regular_handles.push(tokio::spawn(async move {
            download_parallel(http_client, req, cancel, progress_cb)
                .await
                .map_err(|e| (label_for_err, dest_for_err, e))
        }));
    }

    let mut first_err: Option<(String, std::path::PathBuf, anyhow::Error)> = None;
    for h in regular_handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(triple)) => {
                if first_err.is_none() {
                    first_err = Some(triple);
                }
            }
            Err(join_err) => {
                if first_err.is_none() {
                    first_err = Some((
                        "(task)".into(),
                        std::path::PathBuf::new(),
                        anyhow::anyhow!(join_err),
                    ));
                }
            }
        }
    }
    if let Some((label, dest, err)) = first_err {
        tracing::warn!(?err, file = %label, "model download failed");
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "model_download_failed".into(),
            message: format!(
                "Couldn't download {label}: {err}\n\n\
                 Large model downloads can take several minutes — \
                 check your connection and click Retry. Downloads \
                 resume from where they stopped, so no progress is lost."
            ),
            path: Some(dest.display().to_string()),
            model_kind: Some(model_kind.clone()),
        }))))
        .await;
        tracing::warn!(model_kind = %model_kind, outcome = "download_failed", file = %label, "[PREWARM] exiting");
        return;
    }
    // Lock in the final byte counts for regular files so zip progress
    // computes against the correct baseline.
    for idx in regular_indices.iter().copied() {
        per_file_done[idx].store(
            model.files[idx].approx_bytes,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    // Now run zip files sequentially, with post-download extract.
    for idx in zip_indices.iter().copied() {
        let file = &model.files[idx];
        let label = file
            .dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let file_bytes_mb = file.approx_bytes / (1024 * 1024);
        tracing::info!(
            model_kind = %model_kind,
            file_idx = idx,
            file_total = file_count,
            file = %label,
            file_mb = file_bytes_mb,
            "[PREWARM] starting file (zip, sequential)"
        );
        let progress_cb = make_progress_cb(idx, file, label.clone());
        let req = DownloadRequest {
            url: file.url.clone(),
            destination: file.dest.clone(),
            expected_sha256: file.sha256.clone(),
        };
        if let Err(err) = download_parallel(http_client.clone(), req, cancel.clone(), progress_cb).await {
            tracing::warn!(?err, file = %label, "model download failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "model_download_failed".into(),
                message: format!(
                    "Couldn't download {label}: {err}\n\n\
                     Large model downloads can take several minutes — \
                     check your connection and click Retry. Downloads \
                     resume from where they stopped, so no progress is lost."
                ),
                path: Some(file.dest.display().to_string()),
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            tracing::warn!(model_kind = %model_kind, outcome = "download_failed", file = %label, "[PREWARM] exiting");
            return;
        }
        let dest = file.dest.clone();
        let extract = tokio::task::spawn_blocking(move || extract_zip_into_parent(&dest)).await;
        if let Err(err) = extract.unwrap_or(Err(anyhow::anyhow!("zip extract panicked"))) {
            tracing::warn!(?err, "zip extract failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "zip_extract_failed".into(),
                message: format!("Couldn't extract {label}: {err}"),
                path: Some(file.dest.display().to_string()),
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            tracing::warn!(model_kind = %model_kind, outcome = "zip_extract_failed", file = %label, "[PREWARM] exiting");
            return;
        }
        let _ = std::fs::remove_file(&file.dest);
        if let Some(parent) = file.dest.parent() {
            platform::register_dll_dirs_under(parent);
        }
        per_file_done[idx].store(
            file.approx_bytes,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    // Atomic write so a kill mid-write can never leave a half-written
    // sentinel that subsequent runs treat as "installed."
    if let Some(sentinel) = models::registry::sentinel_path(&model) {
        if let Some(parent) = sentinel.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                tracing::error!(?err, dir = %parent.display(), "could not create .sentinels dir");
                sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                    kind: "sentinel_dir_create_failed".into(),
                    message: format!(
                        "Couldn't create the sentinel directory at {}. Model {} is downloaded but not registered as installed; try again.",
                        parent.display(),
                        model.display_name
                    ),
                    path: Some(parent.display().to_string()),
                    model_kind: Some(model_kind.clone()),
                }))))
                .await;
                return;
            }
        }
        let tmp = sentinel.with_extension("installed.tmp");
        if let Err(err) = tokio::fs::write(&tmp, model.id.as_bytes()).await {
            tracing::error!(?err, tmp = %tmp.display(), "sentinel tmp write failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "sentinel_write_failed".into(),
                message: format!("Couldn't write the install marker for {}: {err}", model.display_name),
                path: Some(tmp.display().to_string()),
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            return;
        }
        if let Err(err) = tokio::fs::rename(&tmp, &sentinel).await {
            tracing::error!(?err, from = %tmp.display(), to = %sentinel.display(), "sentinel rename failed");
            let _ = tokio::fs::remove_file(&tmp).await;
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "sentinel_rename_failed".into(),
                message: format!(
                    "Couldn't finalize the install marker for {}: {err}",
                    model.display_name
                ),
                path: Some(sentinel.display().to_string()),
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            return;
        }
    }

    sink.send(IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
        ModelDownloadProgress {
            model_kind: model_kind.clone(),
            fraction: 1.0,
            message: format!("{} installed", model.display_name),
            bytes_done: Some(total_bytes_estimate),
            total_bytes: Some(total_bytes_estimate),
        },
    ))))
    .await;
    tracing::info!(model_kind = %model_kind, outcome = "installed", "[PREWARM] exiting");
}

/// Bulk-apply tags to a set of files. Updates DB `tags` table + writes
/// the sidecar JSON so Explorer + future scans see the same set.
async fn handle_apply_tags(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::ApplyTagsPayload,
) {
    use crate::ipc::{BulkActionItem, BulkActionResult, TagMode};

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        for fid in &payload.file_ids {
            let path: Result<String, _> = tx.query_row(
                "SELECT path_text FROM files WHERE id = ?1",
                rusqlite::params![fid],
                |r| r.get::<_, String>(0),
            );
            let path = match path {
                Ok(p) => p,
                Err(err) => {
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(*fid),
                        ok: false,
                        message: Some(format!("not found: {err}")),
                    });
                    continue;
                }
            };
            if matches!(payload.mode, TagMode::Replace) {
                let _ = tx.execute(
                    "DELETE FROM tags WHERE file_id = ?1 AND source = 'user'",
                    rusqlite::params![fid],
                );
            }
            let mut row_ok = true;
            for tag in &payload.tags {
                let trimmed = tag.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Err(err) = tx.execute(
                    "INSERT OR REPLACE INTO tags (file_id, tag, source, score) VALUES (?1, ?2, 'user', NULL)",
                    rusqlite::params![fid, trimmed],
                ) {
                    failed += 1;
                    row_ok = false;
                    messages.push(BulkActionItem {
                        file_id: Some(*fid),
                        ok: false,
                        message: Some(format!("tag insert failed: {err}")),
                    });
                    break;
                }
            }
            if row_ok {
                // Read all current user tags to write the sidecar.
                let mut stmt = tx
                    .prepare_cached("SELECT tag FROM tags WHERE file_id = ?1 AND source = 'user' ORDER BY tag")?;
                let rows = stmt.query_map(rusqlite::params![fid], |r| r.get::<_, String>(0))?;
                let tags: Vec<String> = rows.filter_map(|r| r.ok()).collect();
                if let Err(err) = crate::shell::tags::write_tags(std::path::Path::new(&path), &tags) {
                    tracing::warn!(?err, %path, "sidecar tag write failed");
                }
                succeeded += 1;
                messages.push(BulkActionItem { file_id: Some(*fid), ok: true, message: None });
            }
        }
        tx.commit()?;
        Ok(BulkActionResult {
            action: "applyTags".into(),
            succeeded,
            failed,
            messages,
        })
    })
    .await;

    emit_bulk_result(&sink, "applyTags", result).await;
}

/// Bulk-rename a set of files (filename only, same directory). Each move
/// is `MoveFileExW` semantics via std::fs::rename + DB row update.
async fn handle_rename_files(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RenameFilesPayload,
) {
    use crate::ipc::{BulkActionItem, BulkActionResult};

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        for entry in &payload.renames {
            // Reject anything that isn't a single Normal path component.
            // Catches /, \, "..", ".", absolute paths (drive letters), and
            // any encoding tricks the OS might still resolve as parent-up.
            if !is_safe_filename(&entry.new_name) {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(entry.file_id),
                    ok: false,
                    message: Some("new name must be a single filename (no slashes, no '..', no '.', no drive)".into()),
                });
                continue;
            }
            let path: Result<String, _> = tx.query_row(
                "SELECT path_text FROM files WHERE id = ?1",
                rusqlite::params![entry.file_id],
                |r| r.get::<_, String>(0),
            );
            let path = match path {
                Ok(p) => std::path::PathBuf::from(p),
                Err(err) => {
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(entry.file_id),
                        ok: false,
                        message: Some(format!("not found: {err}")),
                    });
                    continue;
                }
            };
            let dir = match path.parent() {
                Some(d) => d.to_path_buf(),
                None => {
                    failed += 1;
                    messages.push(BulkActionItem {
                        file_id: Some(entry.file_id),
                        ok: false,
                        message: Some("source has no parent".into()),
                    });
                    continue;
                }
            };
            let dest = dir.join(&entry.new_name);
            // Refuse to clobber existing files. Bulk rename is an explicit
            // user action; surprising overwrites = data loss.
            if dest.exists() {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(entry.file_id),
                    ok: false,
                    message: Some(format!("destination exists: {}", dest.display())),
                });
                continue;
            }
            if let Err(err) = std::fs::rename(&path, &dest) {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(entry.file_id),
                    ok: false,
                    message: Some(format!("rename failed: {err}")),
                });
                continue;
            }
            let dest_text = dest.to_string_lossy().to_string();
            let _ = tx.execute(
                "UPDATE files SET path_text = ?1 WHERE id = ?2",
                rusqlite::params![dest_text, entry.file_id],
            );
            succeeded += 1;
            messages.push(BulkActionItem {
                file_id: Some(entry.file_id),
                ok: true,
                message: Some(dest_text),
            });
        }
        tx.commit()?;
        Ok(BulkActionResult {
            action: "renameFiles".into(),
            succeeded,
            failed,
            messages,
        })
    })
    .await;

    emit_bulk_result(&sink, "renameFiles", result).await;
}

/// Trash a set of files. Looks up paths from the DB, hands a Vec<PathBuf>
/// to shell::trash::trash, removes the rows on success.
async fn handle_trash_files(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::TrashFilesPayload,
) {
    use crate::ipc::{BulkActionItem, BulkActionResult};

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();
        let mut path_for_id: Vec<(i64, std::path::PathBuf)> = Vec::with_capacity(payload.file_ids.len());

        {
            let conn = db.lock();
            for fid in &payload.file_ids {
                if let Ok(p) = conn.query_row(
                    "SELECT path_text FROM files WHERE id = ?1",
                    rusqlite::params![fid],
                    |r| r.get::<_, String>(0),
                ) {
                    path_for_id.push((*fid, std::path::PathBuf::from(p)));
                }
            }
        }

        let outcomes = crate::shell::trash::trash(&path_for_id.iter().map(|(_, p)| p.clone()).collect::<Vec<_>>());

        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let mut log_items: Vec<TrashLogItem> = Vec::new();
        for ((fid, path), trashed_ok) in path_for_id.iter().zip(outcomes.into_iter()) {
            if trashed_ok {
                let _ = tx.execute("DELETE FROM files WHERE id = ?1", rusqlite::params![fid]);
                succeeded += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*fid),
                    ok: true,
                    message: Some(path.to_string_lossy().to_string()),
                });
                log_items.push(TrashLogItem {
                    file_id: *fid,
                    original_path: path.to_string_lossy().to_string(),
                    recycle_bin_id: None,
                });
            } else {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*fid),
                    ok: false,
                    message: Some(format!("trash failed: {}", path.display())),
                });
            }
        }
        tx.commit()?;

        // Append a batch entry to trash_log.json for restoreFromTrash.
        let batch_id = uuid::Uuid::new_v4().to_string();
        if !log_items.is_empty() {
            let entry = TrashLogEntry {
                batch_id: batch_id.clone(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0),
                items: log_items,
            };
            if let Err(err) = append_trash_log(&entry) {
                tracing::warn!(?err, "trash_log append failed");
            }
        }

        // Tag the BulkActionResult.action with the batch id so the app
        // can store it on the UndoStack entry without an extra IPC.
        Ok(BulkActionResult {
            action: format!("trashFiles:{}", batch_id),
            succeeded,
            failed,
            messages,
        })
    })
    .await;

    emit_bulk_result(&sink, "trashFiles", result).await;
}

/// Merge two person clusters: every face_print with person_id = source
/// is reassigned to destination, then the source person row is deleted.
async fn handle_merge_clusters(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::MergeClustersPayload,
) {
    use crate::ipc::{BulkActionItem, BulkActionResult};

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let moved = tx.execute(
            "UPDATE face_prints SET person_id = ?1 WHERE person_id = ?2",
            rusqlite::params![payload.destination_person_id, payload.source_person_id],
        )? as u32;
        let _ = tx.execute(
            "DELETE FROM persons WHERE id = ?1",
            rusqlite::params![payload.source_person_id],
        );
        // Recompute file_count for destination.
        let _ = tx.execute(
            "UPDATE persons SET file_count = (SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id = ?1) WHERE id = ?1",
            rusqlite::params![payload.destination_person_id],
        );
        tx.commit()?;
        Ok(BulkActionResult {
            action: "mergeClusters".into(),
            succeeded: 1,
            failed: 0,
            messages: vec![BulkActionItem {
                file_id: None,
                ok: true,
                message: Some(format!(
                    "moved {moved} face prints from #{src} into #{dst}",
                    src = payload.source_person_id,
                    dst = payload.destination_person_id,
                )),
            }],
        })
    })
    .await;

    emit_bulk_result(&sink, "mergeClusters", result).await;
}

async fn emit_bulk_result(
    sink: &Sink,
    action: &str,
    result: Result<anyhow::Result<ipc::BulkActionResult>, tokio::task::JoinError>,
) {
    use crate::ipc::{BulkActionItem, BulkActionResult};
    match result {
        Ok(Ok(r)) => {
            sink.send(IpcEvent::now(EventPayload::BulkActionResult(Wrap::new(r))))
                .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, action, "bulk action failed");
            sink.send(IpcEvent::now(EventPayload::BulkActionResult(Wrap::new(
                BulkActionResult {
                    action: action.into(),
                    succeeded: 0,
                    failed: 0,
                    messages: vec![BulkActionItem {
                        file_id: None,
                        ok: false,
                        message: Some(format!("{err}")),
                    }],
                },
            ))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, action, "bulk action spawn_blocking failed");
        }
    }
}

/// Save the structured-name fields (title/first/middle/last/suffix) for a
/// person cluster through the engine's single-writer connection.
async fn handle_rename_person(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RenamePersonPayload,
) {
    use crate::ipc::{BulkActionItem, BulkActionResult};

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let title  = payload.title.as_deref().filter(|s| !s.trim().is_empty());
        let first  = payload.first_name.as_deref().filter(|s| !s.trim().is_empty());
        let middle = payload.middle_name.as_deref().filter(|s| !s.trim().is_empty());
        let last   = payload.last_name.as_deref().filter(|s| !s.trim().is_empty());
        let suffix = payload.suffix.as_deref().filter(|s| !s.trim().is_empty());
        let display = match (first, last) {
            (Some(f), Some(l)) => Some(format!("{f} {l}")),
            (Some(f), None) => Some(f.to_string()),
            (None, Some(l)) => Some(l.to_string()),
            _ => None,
        };
        tx.execute(
            "UPDATE persons SET title=?1, first_name=?2, middle_name=?3, last_name=?4, suffix=?5, name=COALESCE(?6, name) WHERE id=?7",
            rusqlite::params![title, first, middle, last, suffix, display, payload.person_id],
        )?;
        tx.commit()?;
        Ok(BulkActionResult {
            action: "renamePerson".into(),
            succeeded: 1,
            failed: 0,
            messages: vec![BulkActionItem {
                file_id: Some(payload.person_id),
                ok: true,
                message: display,
            }],
        })
    })
    .await;

    emit_bulk_result(&sink, "renamePerson", result).await;
}

/// FEAT-CRIT-1: bulk "Mark as unknown" for multi-select people view.
/// Sets persons.is_unknown = 1 for every id in the payload + clears the
/// display name (so a previously-named cluster becomes anonymous when
/// the user reverses an assignment).
async fn handle_mark_persons_as_unknown(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::MarkPersonsAsUnknownPayload,
) {
    use crate::ipc::{BulkActionItem, BulkActionResult};

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();
        for id in &payload.person_ids {
            match tx.execute(
                "UPDATE persons SET is_unknown = 1, name = NULL, first_name = NULL, last_name = NULL WHERE id = ?1",
                rusqlite::params![id],
            ) {
                Ok(_) => {
                    succeeded += 1;
                    messages.push(BulkActionItem { file_id: Some(*id), ok: true, message: None });
                }
                Err(e) => {
                    failed += 1;
                    messages.push(BulkActionItem { file_id: Some(*id), ok: false, message: Some(e.to_string()) });
                }
            }
        }
        tx.commit()?;
        Ok(BulkActionResult { action: "markPersonsAsUnknown".into(), succeeded, failed, messages })
    })
    .await;

    emit_bulk_result(&sink, "markPersonsAsUnknown", result).await;
}

/// Find merge-candidate cluster pairs by ArcFace cosine similarity in the
/// uncertain band (COS_LOW..COS_HIGH from face_clustering). Pairs already
/// confirmed-different in face_verifications are filtered out so the
/// suggested-merges sheet doesn't keep re-prompting.
async fn handle_find_merge_suggestions(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
) {
    use crate::ipc::{MergeSuggestion, MergeSuggestions};
    use crate::pipeline::face_clustering::{COS_HIGH, COS_LOW};

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<MergeSuggestions> {
        let conn = db.lock();
        // For each person, take the highest-quality face print as the
        // anchor embedding. Compare anchor cosines pairwise; emit pairs
        // in the uncertain band that haven't been marked different.
        let mut stmt = conn.prepare(
            "SELECT p.id, p.representative_face_id, COUNT(fp.id),
                    (SELECT fp2.arcface_embedding FROM face_prints fp2
                     WHERE fp2.person_id = p.id AND fp2.arcface_embedding IS NOT NULL
                     ORDER BY COALESCE(fp2.face_quality, 0) DESC LIMIT 1) AS anchor_blob,
                    (SELECT fp3.id FROM face_prints fp3
                     WHERE fp3.person_id = p.id AND fp3.arcface_embedding IS NOT NULL
                     ORDER BY COALESCE(fp3.face_quality, 0) DESC LIMIT 1) AS anchor_id
             FROM persons p JOIN face_prints fp ON fp.person_id = p.id
             GROUP BY p.id"
        )?;
        let rows: Vec<(i64, i64, i64, Vec<u8>)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(4).unwrap_or(0),
                    r.get::<_, i64>(2)?,
                    r.get::<_, Vec<u8>>(3).unwrap_or_default(),
                ))
            })?
            .filter_map(|r| r.ok())
            .filter(|(_, _, _, blob)| !blob.is_empty() && blob.len() % 4 == 0)
            .collect();

        let decode = |blob: &[u8]| -> Vec<f32> {
            blob.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        };
        let cos = |a: &[f32], b: &[f32]| -> f32 {
            let mut acc = 0.0;
            for i in 0..a.len().min(b.len()) {
                acc += a[i] * b[i];
            }
            acc
        };

        // Pre-load the "verified-different" pair set so we don't suggest them.
        let mut verified_different: std::collections::HashSet<(i64, i64)> =
            std::collections::HashSet::new();
        if let Ok(mut vstmt) = conn.prepare(
            "SELECT person_a, person_b FROM face_verifications WHERE same_person = 0",
        ) {
            let rs = vstmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
                .ok();
            if let Some(rs) = rs {
                for r in rs.flatten() {
                    let (a, b) = if r.0 < r.1 { (r.0, r.1) } else { (r.1, r.0) };
                    verified_different.insert((a, b));
                }
            }
        }

        let embeddings: Vec<(i64, i64, i64, Vec<f32>)> = rows
            .into_iter()
            .map(|(pid, anchor_id, count, blob)| (pid, anchor_id, count, decode(&blob)))
            .collect();

        let mut pairs: Vec<MergeSuggestion> = Vec::new();
        for i in 0..embeddings.len() {
            for j in (i + 1)..embeddings.len() {
                let (pa, anchor_a, count_a, ref ea) = embeddings[i];
                let (pb, anchor_b, count_b, ref eb) = embeddings[j];
                let key = if pa < pb { (pa, pb) } else { (pb, pa) };
                if verified_different.contains(&key) {
                    continue;
                }
                let s = cos(ea, eb);
                if s >= COS_LOW && s < COS_HIGH {
                    pairs.push(MergeSuggestion {
                        source_person_id: pa,
                        destination_person_id: pb,
                        similarity: s,
                        source_anchor_face_id: anchor_a,
                        destination_anchor_face_id: anchor_b,
                        source_member_count: count_a,
                        destination_member_count: count_b,
                    });
                }
            }
        }
        pairs.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
        if pairs.len() > 50 { pairs.truncate(50); }

        Ok(MergeSuggestions { pairs })
    })
    .await;

    match result {
        Ok(Ok(s)) => {
            sink.send(IpcEvent::now(EventPayload::MergeSuggestions(Wrap::new(s))))
                .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "find_merge_suggestions failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "find_merge_suggestions_failed".into(),
                message: format!("Find merge suggestions failed: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "find_merge_suggestions spawn failed");
        }
    }
}

// ─── Sidecar undo logs ──────────────────────────────────────────────
//
// trash_log.json / merge_log.json are append-only NDJSON files capped at
// the last 1024 entries. Lets the app's UndoStack stay process-local
// across restarts AND lets restoreFromTrash know which paths to bring
// back from the Recycle Bin without bloating the SQLite schema.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TrashLogEntry {
    batch_id: String,
    timestamp: f64,
    items: Vec<TrashLogItem>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TrashLogItem {
    file_id: i64,
    original_path: String,
    /// Hint set by IFileOperation if available (.GetName on the IShellItem
    /// after delete) — the Recycle Bin renames each item to a $R*.* form.
    /// Often empty; restore by path is the canonical fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recycle_bin_id: Option<String>,
}

fn append_trash_log(entry: &TrashLogEntry) -> anyhow::Result<()> {
    use std::io::Write;
    let path = paths::trash_log_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string(entry)?;
    // V14.7.2: HMAC-sign each entry so a local attacker who appends
    // a forged entry can't get it accepted by restoreFromTrash. The
    // entry format is `{json}\t{hex_hmac}` — the existing single-line
    // JSON parser is updated to split on \t and verify before parse.
    let mac = hmac_sha256_hex(&log_hmac_key()?, json.as_bytes());
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{json}\t{mac}")?;
    // Force flush so a crash immediately after delete-to-trash doesn't
    // lose the log entry (which would orphan the Recycle Bin items).
    file.sync_all()?;
    Ok(())
}

fn read_trash_log_batch(batch_id: &str) -> anyhow::Result<Option<TrashLogEntry>> {
    let path = paths::trash_log_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let key = log_hmac_key()?;
    let raw = std::fs::read_to_string(&path)?;
    for line in raw.lines() {
        if line.trim().is_empty() { continue; }
        // V14.7.2: split json + HMAC. Pre-V14.7.2 entries (no tab) are
        // accepted in read-only mode for backward compat; new writes
        // always carry a HMAC. After 14 days of run-time the legacy-
        // entries path gets rotated out organically.
        let (payload, mac_hex) = match line.find('\t') {
            Some(i) => (&line[..i], Some(&line[i + 1..])),
            None    => (line, None),
        };
        if let Some(expected) = mac_hex {
            let actual = hmac_sha256_hex(&key, payload.as_bytes());
            if !constant_time_eq_str(&actual, expected) {
                tracing::warn!("trash_log entry HMAC mismatch -- rejecting forged entry");
                continue;
            }
        }
        if let Ok(entry) = serde_json::from_str::<TrashLogEntry>(payload) {
            if entry.batch_id == batch_id {
                return Ok(Some(entry));
            }
        }
    }
    Ok(None)
}

/// V14.7.2: HMAC-SHA256 hand-rolled atop the existing `sha2` dependency.
/// 30 lines beats adding the `hmac` crate for one call site.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    const BLOCK_SIZE: usize = 64;
    let mut k = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let h = Sha256::digest(key);
        k[..32].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let out = outer.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&out);
    bytes
}

fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    hex::encode(hmac_sha256(key, msg))
}

/// Constant-time string comparison (avoids timing-side-channel HMAC
/// validation). Caller supplies hex strings of equal length; mismatched
/// lengths short-circuit fail.
fn constant_time_eq_str(a: &str, b: &str) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Lazily-loaded 32-byte HMAC key for the trash/merge sidecar logs.
/// Persisted at `%LOCALAPPDATA%\FileID\log-hmac.key`. NTFS ACLs on
/// `%LOCALAPPDATA%` already restrict to the user; that's enough for
/// our threat model (defense against another local app's tampering).
fn log_hmac_key() -> anyhow::Result<Vec<u8>> {
    static KEY: parking_lot::Mutex<Option<Vec<u8>>> = parking_lot::Mutex::new(None);
    let mut guard = KEY.lock();
    if let Some(k) = guard.as_ref() {
        return Ok(k.clone());
    }
    let root = paths::root()?;
    std::fs::create_dir_all(&root).ok();
    let path = root.join("log-hmac.key");
    let bytes = if path.exists() {
        std::fs::read(&path)?
    } else {
        // Generate via getrandom() through `uuid` (already a dep).
        // Two UUIDs = 32 bytes of OS-CSPRNG entropy.
        let mut k = Vec::with_capacity(32);
        k.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        k.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        std::fs::write(&path, &k)?;
        k
    };
    *guard = Some(bytes.clone());
    Ok(bytes)
}

/// SEC-7: best-effort canonicalize for a path that may not exist (the
/// file is in the Recycle Bin). Returns the closest existing ancestor's
/// canonical path joined with the missing tail. Same shape as
/// `canonicalize_safely` in restructure_apply but lives here to avoid
/// a cross-module dependency.
fn canonicalize_path_for_containment(p: &std::path::Path) -> std::path::PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    let mut cur = p.to_path_buf();
    let mut tail = std::path::PathBuf::new();
    while !cur.exists() {
        if let Some(name) = cur.file_name() {
            tail = if tail.as_os_str().is_empty() {
                std::path::PathBuf::from(name)
            } else {
                std::path::Path::new(name).join(tail)
            };
        }
        if !cur.pop() { break; }
    }
    let mut canonical = std::fs::canonicalize(&cur).unwrap_or(cur);
    canonical.push(tail);
    canonical
}

/// Bounded line read from stdin. Reads byte-by-byte (well, in 8-KB
/// chunks via the BufReader) and bails the moment the in-progress line
/// would exceed `max_bytes`. Returns one of:
/// - `Line(text)`           — a complete line under the cap
/// - `Oversized(seen)`      — refused after `seen` bytes; caller drains
/// - `Eof`                  — clean stdin close
enum BoundedRead {
    Line(String),
    Oversized(usize),
    Eof,
}

async fn bounded_read_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<BoundedRead> {
    buf.clear();
    let mut byte = [0u8; 1];
    loop {
        // Read one byte at a time. Slow in theory; the BufReader fills its
        // internal 8-KB buffer in larger reads, so this only crosses the
        // syscall boundary every ~8 KB. The tradeoff: we get to enforce
        // the cap on every byte without first allocating a giant Vec.
        match reader.read_exact(&mut byte).await {
            Ok(_) => {
                if byte[0] == b'\n' {
                    if buf.last() == Some(&b'\r') { buf.pop(); }
                    let text = String::from_utf8(std::mem::take(buf))
                        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                    return Ok(BoundedRead::Line(text));
                }
                if buf.len() >= max_bytes {
                    return Ok(BoundedRead::Oversized(buf.len()));
                }
                buf.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                if buf.is_empty() {
                    return Ok(BoundedRead::Eof);
                }
                // Trailing partial line at EOF — treat as a complete frame.
                let text = String::from_utf8(std::mem::take(buf))
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                return Ok(BoundedRead::Line(text));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Drain bytes from `reader` until the next newline (used to resync the
/// IPC framing after rejecting an oversized frame). Best-effort; swallows
/// errors and returns on any failure or EOF.
async fn drain_to_newline<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) {
    let mut byte = [0u8; 1];
    while reader.read_exact(&mut byte).await.is_ok() {
        if byte[0] == b'\n' { return; }
    }
}

/// Extract every entry of `zip_path` into its parent directory. Used by
/// the prewarm flow for .zip downloads (llama.cpp runtime, Performance
/// Packs). Files in nested folders inside the zip land under the same
/// nested folders next to the zip.
///
/// Hardened against:
/// - **Zip slip** (entries with absolute / `..` paths). `enclosed_name()`
///   blocks `..`; we ALSO canonicalize-and-`starts_with`-check the
///   destination against the parent to catch any junction/symlink
///   traversal at the FS layer.
/// - **Zip bombs** — caps total uncompressed bytes at 2 GiB and entry
///   count at 10,000.
/// - **Symlink/special entries** — skipped (we only write regular files
///   and create directories).
fn extract_zip_into_parent(zip_path: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context;
    const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB cumulative
    const MAX_ENTRIES: usize = 10_000;

    let parent = zip_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("zip has no parent dir"))?;
    // Canonicalize the parent once for the post-write containment check.
    let parent_canon = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());

    let file = std::fs::File::open(zip_path).context("opening zip")?;
    let mut archive = zip::ZipArchive::new(file).context("reading zip directory")?;

    if archive.len() > MAX_ENTRIES {
        anyhow::bail!("zip rejected: {} entries (cap {})", archive.len(), MAX_ENTRIES);
    }

    let mut total_bytes: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("zip entry")?;
        // SEC: enclosed_name blocks `..` and absolute paths in the entry name.
        let name = entry.enclosed_name().ok_or_else(|| {
            anyhow::anyhow!("zip contains an entry with an unsafe name")
        })?;
        let dest = parent.join(&name);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest).ok();
            continue;
        }
        // Skip symlink / special entries. We use the high bits of the
        // unix_mode field; only regular files (S_IFREG = 0o100000) pass.
        if let Some(mode) = entry.unix_mode() {
            const S_IFMT: u32 = 0o170000;
            const S_IFREG: u32 = 0o100000;
            if (mode & S_IFMT) != S_IFREG {
                continue;
            }
        }
        // Cumulative-size cap (zip-bomb defense).
        let entry_size = entry.size();
        if entry_size > MAX_BYTES || total_bytes.saturating_add(entry_size) > MAX_BYTES {
            anyhow::bail!("zip rejected: cumulative size exceeds {} bytes", MAX_BYTES);
        }
        total_bytes = total_bytes.saturating_add(entry_size);

        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).ok();
        }
        let mut out = std::fs::File::create(&dest)
            .with_context(|| format!("creating {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("writing {}", dest.display()))?;

        // SEC: post-write containment check. If a junction/symlink along
        // the path led elsewhere, reject by deleting + bailing.
        if let Ok(real) = std::fs::canonicalize(&dest) {
            if !real.starts_with(&parent_canon) {
                let _ = std::fs::remove_file(&dest);
                anyhow::bail!("zip entry escaped extraction root: {}", dest.display());
            }
        }
    }
    Ok(())
}

// ─── Undo + recent scans handlers ───────────────────────────────────

async fn handle_restore_from_trash(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RestoreFromTrashPayload,
) {
    use crate::ipc::{BulkActionItem, BulkActionResult};

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let entry = read_trash_log_batch(&payload.batch_id)?
            .ok_or_else(|| anyhow::anyhow!("trash log batch {} not found", payload.batch_id))?;

        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut messages = Vec::new();

        // The Recycle Bin restore via IFileOperation::Recycle reverse is
        // non-trivial — IShellFolder enumeration of the bin + matching
        // pidl by display path. For the V14.6 cut we shell out to
        // PowerShell which has a direct cmdlet (`Restore-RecycleBin -DriveLetter
        // C:`). When restoration succeeds, the file lands at its original
        // path; we re-INSERT a stripped-down DB row so the Library tab
        // shows it again.
        let conn = db.lock();

        // SEC-7: collect every authorized scan root from scan_sessions
        // and require each restore destination to be a descendant. This
        // defends against trash_log forgery — a local attacker who
        // appends a hostile entry like
        //   {"original_path":"C:\\Windows\\System32\\foo.exe", ...}
        // would otherwise be able to write into System32 via our
        // PowerShell shell-out. With containment, restore destinations
        // are restricted to user-blessed library directories.
        let allowed_roots: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT root_path FROM scan_sessions WHERE root_path IS NOT NULL"
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let allowed_canonical: Vec<std::path::PathBuf> = allowed_roots.iter()
            .filter_map(|r| std::fs::canonicalize(r).ok())
            .collect();

        let tx = conn.unchecked_transaction()?;
        for item in &entry.items {
            // Path-containment check before we touch PowerShell.
            let path_obj = std::path::Path::new(&item.original_path);
            // Use canonicalize_safely-style logic — the file doesn't
            // exist (it's in the trash), so canonicalize the closest
            // existing ancestor and append the tail.
            let candidate = canonicalize_path_for_containment(path_obj);
            let allowed = allowed_canonical.iter().any(|root| candidate.starts_with(root));
            if !allowed {
                tracing::warn!(
                    path = %platform::redact_path_for_log(&item.original_path),
                    "SEC-7: refusing restore — path is outside every authorized library root"
                );
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(item.file_id),
                    ok: false,
                    message: Some(format!(
                        "Refused: {} is not inside any authorized library root.",
                        item.original_path
                    )),
                });
                continue;
            }
            let restored = restore_one_from_recycle_bin(&item.original_path).is_ok();
            if restored {
                // Re-insert a row. The next scan will fill in the rest of
                // the metadata; for now we just need the path → id mapping.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let path_obj = std::path::Path::new(&item.original_path);
                let extension = path_obj
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let kind = crate::pipeline::discovery::FileKind::from_extension(&extension);
                let _ = tx.execute(
                    "INSERT OR IGNORE INTO files \
                     (path_text, path_hash, size_bytes, scanned_at, kind, extension, \
                      has_faces, has_text, failed) \
                     VALUES (?1, ?2, 0, ?3, ?4, ?5, 0, 0, 0)",
                    rusqlite::params![
                        item.original_path,
                        stable_path_hash(&item.original_path),
                        now,
                        kind.as_str(),
                        extension,
                    ],
                );
                succeeded += 1;
                messages.push(BulkActionItem {
                    file_id: Some(item.file_id),
                    ok: true,
                    message: Some(item.original_path.clone()),
                });
            } else {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(item.file_id),
                    ok: false,
                    message: Some(format!(
                        "could not restore from Recycle Bin: {}",
                        item.original_path
                    )),
                });
            }
        }
        tx.commit()?;
        Ok(BulkActionResult {
            action: "restoreFromTrash".into(),
            succeeded,
            failed,
            messages,
        })
    })
    .await;

    emit_bulk_result(&sink, "restoreFromTrash", result).await;
}

fn stable_path_hash(path: &str) -> i64 {
    // Case-insensitive on Windows (NTFS) so re-scans after a path-case
    // change don't create duplicate rows. See dbwriter.rs's identical
    // function for the full rationale.
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let normalized = path.to_ascii_lowercase();
    normalized.hash(&mut h);
    h.finish() as i64
}

#[cfg(windows)]
fn restore_one_from_recycle_bin(original_path: &str) -> anyhow::Result<()> {
    // PowerShell walks the Recycle Bin, finds an item whose
    // `OriginalLocation` + `Name` matches, and invokes its Verb
    // "Undelete" (Restore). Path data flows through environment
    // variables (FILEID_RB_PARENT / FILEID_RB_NAME) instead of being
    // interpolated into the script — eliminates every escape concern
    // (single-quote doubling, backslashes, Unicode, RTL marks, etc).
    let parent = std::path::Path::new(original_path)
        .parent()
        .ok_or_else(|| anyhow::anyhow!("bad path"))?
        .to_string_lossy()
        .to_string();
    let name = std::path::Path::new(original_path)
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("bad path"))?
        .to_string_lossy()
        .to_string();
    // Script reads from $env: vars — no string interpolation of paths.
    let script = "\
$shell = New-Object -ComObject Shell.Application; \
$bin = $shell.NameSpace(0x0a); \
$wantParent = $env:FILEID_RB_PARENT; \
$wantName = $env:FILEID_RB_NAME; \
foreach ($i in $bin.Items()) { \
    $loc = $i.ExtendedProperty('System.Recycle.DeletedFrom'); \
    $nm = $i.Name; \
    if ($loc -eq $wantParent -and $nm -eq $wantName) { \
        $i.InvokeVerb('Undelete'); break; \
    } \
}";
    // SEC: pin -ExecutionPolicy Bypass so the script runs even when
    // group policy locks the user-default policy to AllSigned/Restricted.
    // The script is internal (not user-supplied), and arguments cross
    // via env vars so there's no string-interpolation surface.
    let status = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy", "Bypass",
            "-Command", script,
        ])
        .env("FILEID_RB_PARENT", &parent)
        .env("FILEID_RB_NAME", &name)
        .status()?;
    if !status.success() {
        anyhow::bail!("powershell restore exit {:?}", status.code());
    }
    if !std::path::Path::new(original_path).exists() {
        anyhow::bail!("restore reported success but file is still missing");
    }
    Ok(())
}

#[cfg(not(windows))]
fn restore_one_from_recycle_bin(_original_path: &str) -> anyhow::Result<()> {
    anyhow::bail!("Recycle Bin restore not supported on this platform")
}

async fn handle_revert_merge(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::RevertMergePayload,
) {
    use crate::ipc::{BulkActionItem, BulkActionResult};

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<BulkActionResult> {
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        // Re-create the source person row + reassign the listed faces.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        // Use the original id if it's still free; else let SQLite pick a new one.
        tx.execute(
            "INSERT OR IGNORE INTO persons (id, file_count, created_at) VALUES (?1, 0, ?2)",
            rusqlite::params![payload.source_person_id, now],
        )?;
        let new_pid: i64 = tx.query_row(
            "SELECT id FROM persons WHERE id = ?1",
            rusqlite::params![payload.source_person_id],
            |r| r.get(0),
        )?;
        let mut update = tx.prepare("UPDATE face_prints SET person_id = ?1 WHERE id = ?2")?;
        let mut moved = 0u32;
        for fid in &payload.face_ids_to_revert {
            update.execute(rusqlite::params![new_pid, fid])?;
            moved += 1;
        }
        drop(update);
        // Recompute file_count for both clusters.
        let _ = tx.execute(
            "UPDATE persons SET file_count = (SELECT COUNT(DISTINCT file_id) \
             FROM face_prints WHERE person_id = ?1) WHERE id IN (?1, ?2)",
            rusqlite::params![new_pid, payload.destination_person_id],
        );
        tx.commit()?;
        Ok(BulkActionResult {
            action: "revertMerge".into(),
            succeeded: 1,
            failed: 0,
            messages: vec![BulkActionItem {
                file_id: None,
                ok: true,
                message: Some(format!("Restored {moved} face print(s) to person #{new_pid}")),
            }],
        })
    })
    .await;

    emit_bulk_result(&sink, "revertMerge", result).await;
}

// ─── Deep Analyze handlers ──────────────────────────────────────────

async fn handle_deep_analyze_file(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeFilePayload,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use crate::ipc::{DeepAnalyzeComplete, DeepAnalyzeFileDone, DeepAnalyzeProgress, DeepAnalyzeStarting, DeepAnalyzeStartingPhase};
    use crate::pipeline::deep_analyze::{analyze_file, AnalyzeMode};

    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeStarting(Wrap::new(
        DeepAnalyzeStarting {
            model_kind: payload.model_kind.clone(),
            phase: DeepAnalyzeStartingPhase::LoadingModel,
            message: format!("Captioning file #{}…", payload.file_id),
        },
    ))))
    .await;

    let runner = match crate::models::vlm::VlmRunner::find() {
        Ok(r) => r,
        Err(err) => {
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "llama_cpp_missing".into(),
                message: format!("{err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
            return;
        }
    };

    let sink_c = sink.clone();
    let model_kind = payload.model_kind.clone();
    let model_kind_for_progress = model_kind.clone();
    let file_id = payload.file_id;
    let started_at = std::time::Instant::now();
    // V14.9-I: accumulate per-token text so the UI can render the live
    // caption stream word-by-word. Throttle wire emission to 4 Hz so a
    // 50-tok/sec VLM doesn't flood the sink.
    let caption_buf = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
    let last_emit = std::sync::Arc::new(parking_lot::Mutex::new(std::time::Instant::now() - std::time::Duration::from_millis(500)));
    let caption_buf_cb = caption_buf.clone();
    let last_emit_cb = last_emit.clone();
    let outcome = analyze_file(
        db,
        &runner,
        file_id,
        &model_kind,
        AnalyzeMode::Both,
        cancel.clone(),
        move |chunk| {
            // Intentional try_send + drop-on-overflow. Per-token streaming
            // can fire 50+/sec and the original tokio::spawn(async {
            // send.await }) pattern would pile up unbounded tasks if
            // the sink filled. Drops are fine — UI gets the next chunk
            // a few ms later.
            //
            // V14.9-K2: trim chunk + single-space-join. Each chunk from
            // llama-mtmd-cli is one stdout line that may carry trailing
            // whitespace (alignment padding) or no whitespace at all.
            // append_caption_chunk normalizes both into single-space
            // separators so the UI never renders double-spaces or
            // glued-together words.
            append_caption_chunk(&caption_buf_cb, chunk);
            let now = std::time::Instant::now();
            let should_emit = {
                let mut last = last_emit_cb.lock();
                if now.duration_since(*last) >= std::time::Duration::from_millis(250) {
                    *last = now;
                    true
                } else {
                    false
                }
            };
            if !should_emit { return; }
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
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "deep_analyze_failed".into(),
                message: format!("{err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
    }
}

async fn handle_deep_analyze_folder(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeFolderPayload,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let prefix = format!("{}%", payload.path_prefix);
    let ids = match collect_file_ids(&db, "WHERE path_text LIKE ?1 AND kind IN ('image','video')", &[&prefix]) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "deep_analyze_folder query");
            return;
        }
    };
    run_deep_analyze_batch(sink, db, &payload.model_kind, ids, cancel, true).await;
}

async fn handle_deep_analyze_all(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeAllPayload,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let ids = match collect_file_ids(&db, "WHERE kind IN ('image','video')", &[]) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "deep_analyze_all query");
            return;
        }
    };
    run_deep_analyze_batch(sink, db, &payload.model_kind, ids, cancel, payload.skip_existing).await;
}

fn collect_file_ids(
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    where_clause: &str,
    params: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Vec<i64>> {
    let conn = db.lock();
    let sql = format!("SELECT id FROM files {} ORDER BY id", where_clause);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| r.get::<_, i64>(0))?;
    rows.collect()
}

async fn run_deep_analyze_batch(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    model_kind: &str,
    file_ids: Vec<i64>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    skip_existing: bool,
) {
    use crate::ipc::{DeepAnalyzeComplete, DeepAnalyzeFileDone, DeepAnalyzeProgress, DeepAnalyzeStarting, DeepAnalyzeStartingPhase};
    use crate::pipeline::deep_analyze::{analyze_file, AnalyzeMode};

    let runner = match crate::models::vlm::VlmRunner::find() {
        Ok(r) => r,
        Err(err) => {
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "llama_cpp_missing".into(),
                message: format!("{err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
            return;
        }
    };

    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeStarting(Wrap::new(
        DeepAnalyzeStarting {
            model_kind: model_kind.to_string(),
            phase: DeepAnalyzeStartingPhase::LoadingModel,
            message: format!("Analyzing {} file(s)…", file_ids.len()),
        },
    ))))
    .await;

    let total = file_ids.len() as u64;
    let mut processed = 0u64;
    let mut failed = 0u64;
    let started_at = std::time::Instant::now();

    for (idx, file_id) in file_ids.iter().copied().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
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

        if skip_existing {
            let already = {
                let conn = db.lock();
                conn.query_row(
                    "SELECT vlm_description FROM files WHERE id = ?1",
                    rusqlite::params![file_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .unwrap_or(None)
                .is_some()
            };
            if already { continue; }
        }

        // Resolve a display path for the progress event.
        let current_path: Option<String> = {
            let conn = db.lock();
            conn.query_row(
                "SELECT path_text FROM files WHERE id = ?1",
                rusqlite::params![file_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
        };
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(
            DeepAnalyzeProgress {
                processed: idx as u64,
                total,
                eta_seconds: None,
                current_path,
                model_kind: model_kind.to_string(),
                current_caption: None, // pre-inference progress; caption not started.
            },
        ))))
        .await;

        let sink_c = sink.clone();
        let model_kind_c = model_kind.to_string();
        // V14.9-I: per-file caption accumulator + 4 Hz throttle.
        let caption_buf = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
        let last_emit = std::sync::Arc::new(parking_lot::Mutex::new(std::time::Instant::now() - std::time::Duration::from_millis(500)));
        let caption_buf_cb = caption_buf.clone();
        let last_emit_cb = last_emit.clone();
        let outcome = analyze_file(
            db.clone(),
            &runner,
            file_id,
            model_kind,
            AnalyzeMode::Both,
            cancel.clone(),
            move |chunk| {
                // Intentional try_send + drop-on-overflow (see the
                // single-file analyze callback above for rationale).
                // V14.9-K2: append_caption_chunk normalizes spacing.
                append_caption_chunk(&caption_buf_cb, chunk);
                let now = std::time::Instant::now();
                let should_emit = {
                    let mut last = last_emit_cb.lock();
                    if now.duration_since(*last) >= std::time::Duration::from_millis(250) {
                        *last = now;
                        true
                    } else {
                        false
                    }
                };
                if !should_emit { return; }
                let snapshot = caption_buf_cb.lock().clone();
                let kind = model_kind_c.clone();
                let _ = sink_c.try_send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(
                    DeepAnalyzeProgress {
                        processed: idx as u64,
                        total,
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
                failed += 1;
                tracing::warn!(?err, file_id, "deep analyze file failed");
            }
        }
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

/// Pull the stored CLIP image embedding for a file_id from
/// `clip_embeddings` and emit it as a `clipTextEmbedding` event so the
/// app's existing CLIP-search consumer can use it as a similarity seed.
async fn handle_embed_image_query(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::EmbedImageQueryPayload,
) {
    use crate::ipc::ClipTextEmbedding;
    let query_id = payload.query_id.clone();

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<f32>>> {
        let conn = db.lock();
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT embedding FROM clip_embeddings WHERE file_id = ?1",
                rusqlite::params![payload.file_id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .ok();
        Ok(blob.and_then(|b| {
            if b.is_empty() || b.len() % 4 != 0 {
                return None;
            }
            Some(
                b.chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect(),
            )
        }))
    })
    .await;

    match result {
        Ok(Ok(Some(embedding))) => {
            sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
                ClipTextEmbedding {
                    query_id,
                    query: format!("file:{}", payload.file_id),
                    embedding,
                },
            ))))
            .await;
        }
        Ok(Ok(None)) => {
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "embedding_missing".into(),
                message: "This file doesn't have a CLIP embedding yet. Re-scan with CLIP installed.".into(),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "embed_image_query failed");
        }
        Err(err) => {
            tracing::warn!(?err, "embed_image_query spawn failed");
        }
    }
}

/// Re-cluster every face in the DB. Loads all face_prints with an
/// arcface_embedding, runs the connected-components algorithm in
/// `face_clustering`, persists per-face person_id assignments, emits a
/// `faceClusteringComplete` event the People tab refreshes from.
async fn handle_run_face_clustering(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
) {
    use crate::ipc::FaceClusteringResult;
    use crate::pipeline::face_clustering::{cluster, FaceRow};
    use std::time::Instant;

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<FaceClusteringResult> {
        let started = Instant::now();
        let conn = db.lock();

        // Load every face that has an ArcFace embedding.
        let mut faces: Vec<FaceRow> = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id, file_id, arcface_embedding, COALESCE(face_quality, 0.0) \
                 FROM face_prints \
                 WHERE arcface_embedding IS NOT NULL AND COALESCE(excluded, 0) = 0",
            )?;
            let rows = stmt.query_map([], |r| {
                let id: i64 = r.get(0)?;
                let file_id: i64 = r.get(1)?;
                let blob: Vec<u8> = r.get(2)?;
                let quality: f64 = r.get(3)?;
                Ok((id, file_id, blob, quality))
            })?;
            for row in rows {
                let (id, file_id, blob, quality) = row?;
                if blob.len() % 4 != 0 || blob.is_empty() {
                    continue;
                }
                let mut embedding = Vec::with_capacity(blob.len() / 4);
                for chunk in blob.chunks_exact(4) {
                    embedding.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
                faces.push(FaceRow {
                    face_id: id,
                    file_id,
                    embedding,
                    quality: quality as f32,
                });
            }
        }

        let face_count = faces.len() as u64;
        let (assignments, anchors) = cluster(&faces);

        let tx = conn.unchecked_transaction()?;
        // Persist clusters: clear existing person_id assignments + persons,
        // re-create one persons row per anchor, point face_prints at it.
        tx.execute("UPDATE face_prints SET person_id = NULL", [])?;
        tx.execute("DELETE FROM persons", [])?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        // Map cluster_id (1-based) → DB person row id.
        let mut cid_to_person: std::collections::HashMap<i32, i64> = std::collections::HashMap::new();
        for anchor in &anchors {
            tx.execute(
                "INSERT INTO persons (name, representative_face_id, file_count, created_at) \
                 VALUES (NULL, ?1, ?2, ?3)",
                rusqlite::params![anchor.anchor_face_id, anchor.member_count as i64, now],
            )?;
            let person_id = tx.last_insert_rowid();
            cid_to_person.insert(anchor.cluster_id, person_id);
        }

        let mut update = tx.prepare("UPDATE face_prints SET person_id = ?1 WHERE id = ?2")?;
        for a in &assignments {
            if let Some(&pid) = cid_to_person.get(&a.cluster_id) {
                update.execute(rusqlite::params![pid, a.face_id])?;
            }
        }
        drop(update);
        tx.commit()?;

        Ok(FaceClusteringResult {
            person_count: anchors.len() as u32,
            face_count,
            unmatched_faces: 0,
            duration_seconds: started.elapsed().as_secs_f64(),
        })
    })
    .await;

    match result {
        Ok(Ok(r)) => {
            sink.send(IpcEvent::now(EventPayload::FaceClusteringComplete(Wrap::new(r))))
                .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "face clustering failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "face_clustering_failed".into(),
                message: format!("Face clustering failed: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "face clustering spawn failed");
        }
    }
}

async fn handle_embed_text_query(sink: Sink, payload: ipc::EmbedTextQueryPayload) {
    use crate::ipc::ClipTextEmbedding;
    let query = payload.query.clone();
    let query_id = payload.query_id.clone();

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<f32>> {
        // Tokenizer + model live in a process-static slot so back-to-back
        // queries reuse them (avoids the 100-300 ms ORT session create on
        // every keystroke).
        use std::sync::OnceLock;
        static TEXT_MODEL: OnceLock<parking_lot::Mutex<Option<crate::models::clip_text::ClipText>>> = OnceLock::new();
        let cell = TEXT_MODEL.get_or_init(|| parking_lot::Mutex::new(None));
        let mut guard = cell.lock();
        if guard.is_none() {
            let weights = crate::models::clip_text::default_weights_path()?;
            let dir = weights.parent().ok_or_else(|| anyhow::anyhow!("text weights have no parent dir"))?;
            let vocab_path = dir.join("vocab.json");
            let merges_path = dir.join("merges.txt");
            let vocab = std::fs::read_to_string(&vocab_path)
                .map_err(|e| anyhow::anyhow!("vocab.json missing at {}: {}", vocab_path.display(), e))?;
            let merges = std::fs::read_to_string(&merges_path)
                .map_err(|e| anyhow::anyhow!("merges.txt missing at {}: {}", merges_path.display(), e))?;
            let tokenizer = crate::models::ClipTokenizer::new(&vocab, &merges)?;
            let model = crate::models::clip_text::ClipText::load(weights, tokenizer)?;
            *guard = Some(model);
        }
        let model = guard.as_mut().expect("just set");
        model.embed(&payload.query)
    })
    .await;

    match result {
        Ok(Ok(embedding)) => {
            sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
                ClipTextEmbedding {
                    query_id,
                    query,
                    embedding,
                },
            ))))
            .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "CLIP text embed failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "clip_text_embed_failed".into(),
                message: format!("CLIP text embed failed: {err}. Install CLIP via Welcome / Settings."),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "CLIP embed spawn failed");
        }
    }
}

/// Drive an end-to-end scan. Loads ML weights, registers the coordinator
/// in the shared state slot, runs the pipeline, clears the slot when done.
async fn handle_start_scan(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    scan_state: Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>>,
    payload: ipc::StartScanPayload,
) {
    use crate::pipeline::tagging::ModelStack;
    use crate::scan_session::ScanSession;
    use ipc::ScanPhase;
    use std::path::PathBuf;

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

    // Pre-flight before ModelStack::load_default. Without this, a user
    // who clicked Scan before completing Welcome would wedge ORT for the
    // full timeout window with no actionable feedback.
    let missing_models: Vec<&str> = ["mobileclip_s2", "arcface", "clip_text"]
        .iter()
        .filter_map(|kind| {
            let model = match models::registry::lookup_full(kind) {
                models::registry::LookupResult::Found(m) => m,
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

    // V14.8.4 Bug 5: emit Discovering immediately so the UI flips out of
    // IdlePanel within microseconds, regardless of how long ModelStack
    // takes to load. Without this the user sees "nothing happens" for
    // 100ms–30s and assumes scanning is unimplemented. Use .await (not
    // try_send) so the event can't be silently dropped under sink load.
    sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
        ScanPhase::Discovering,
    ))))
    .await;

    // V14.9-N2.1: baseline Progress so the sidebar stats flip from "—"
    // to "0" immediately. Previously, if no file ever reached the
    // DBWriter (empty folder, all-filtered, or a stall anywhere
    // downstream), LastProgress stayed null and every stat row in the
    // sidebar rendered "—" forever — looked identical to "scan did
    // nothing". This baseline event guarantees the user always sees
    // a confirmation that the scan started.
    let session_id_baseline = uuid::Uuid::new_v4().to_string();
    sink.send(IpcEvent::now(EventPayload::Progress(Wrap::new(
        ipc::ScanProgress {
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
        },
    ))))
    .await;

    let coord = coordinator::ScanCoordinator::new();
    *scan_state.lock() = Some(coord.clone());

    // Load ML model weights once per session. Heavy enough to belong on a
    // blocking thread (ORT session create can take 100-500ms per model).
    // 8s timeout: ORT session-create for a healthy local file completes in
    // well under a second; budget 8s for cold-cache cases. A wedged
    // commit_from_file (corrupt .onnx) was previously hanging the entire
    // scan for 30s — that read to users as "Start scan did nothing".
    let models = match tokio::time::timeout(
        std::time::Duration::from_secs(8),
        tokio::task::spawn_blocking(ModelStack::load_default),
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
            tracing::error!("model stack load timed out after 8s");
            sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
                ScanPhase::Failed,
            ))))
            .await;
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "model_load_timeout".into(),
                message:
                    "Loading inference models took longer than 8 seconds — \
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
        if models.mobileclip.is_none() {
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
    let session = ScanSession::new(coord, db, worker_count, sink.clone(), models);
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

/// V14.9-G: build a fresh `HardwareInfo` snapshot by re-running the
/// detection probe. Shared by `emit_ready` (engine startup) and the
/// `verifyCudaPack` handler (Settings → Performance "Verify install"
/// button) so both surfaces see the same authoritative shape.
/// V14.9-K2: append a per-token caption chunk from `llama-mtmd-cli` into
/// the shared accumulator with normalized single-space separators.
/// `llama-mtmd-cli` emits one stdout line per `on_token` call; lines may
/// carry leading/trailing whitespace (alignment padding) or none at all.
/// Trimming each chunk and joining with exactly one space produces clean
/// English-prose output regardless of the model's actual whitespace habit.
fn append_caption_chunk(buf: &std::sync::Arc<parking_lot::Mutex<String>>, chunk: &str) {
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

fn build_hardware_info() -> ipc::HardwareInfo {
    use ipc::HardwareInfo;
    use models::runtime::{ExecutionProvider, GpuVendor, RuntimeProbe};

    let probe = RuntimeProbe::detect();
    let vendor_str = match probe.vendor {
        GpuVendor::Nvidia    => "nvidia",
        GpuVendor::Amd       => "amd",
        GpuVendor::Intel     => "intel",
        GpuVendor::Qualcomm  => "qualcomm",
        GpuVendor::Other(_)  => "other",
        GpuVendor::None      => "none",
    };

    // Recommendation copy. Only suggest packs that would actually help.
    let recommendation = match (probe.vendor, probe.provider, probe.cuda_pack_present, probe.openvino_pack_present, probe.qnn_pack_present) {
        (GpuVendor::Nvidia, ExecutionProvider::DirectMl, false, _, _) =>
            "NVIDIA detected. Install the CUDA Pack in Settings → Performance for ~30% faster ML inference.".to_string(),
        (GpuVendor::Intel, ExecutionProvider::DirectMl, _, false, _) =>
            "Intel iGPU/Arc detected. Install the OpenVINO Pack in Settings → Performance for vendor-tuned inference.".to_string(),
        (GpuVendor::Qualcomm, ExecutionProvider::DirectMl, _, _, false) =>
            "Snapdragon NPU detected. Install the QNN Pack in Settings → Performance to use the Hexagon NPU.".to_string(),
        (GpuVendor::None, _, _, _, _) =>
            "No GPU detected. Falling back to CPU inference.".to_string(),
        _ => String::new(),
    };

    HardwareInfo {
        gpu_vendor: vendor_str.into(),
        adapter_name: probe.adapter_name.clone(),
        execution_provider: probe.provider.as_str().into(),
        physical_cpu_cores: num_cpus::get_physical().max(1) as u32,
        cuda_pack_present: probe.cuda_pack_present,
        openvino_pack_present: probe.openvino_pack_present,
        qnn_pack_present: probe.qnn_pack_present,
        recommendation,
    }
}

async fn emit_ready(sink: &Sink) {
    let hardware = build_hardware_info();
    let info = EngineInfo {
        version: ENGINE_VERSION.into(),
        pid: std::process::id() as i32,
        worker_cap: platform::default_worker_cap(),
        physical_memory_gb: platform::physical_memory_gb(),
        hardware: Some(hardware),
    };
    sink.send(IpcEvent::now(EventPayload::Ready(Wrap::new(info))))
        .await;
}

/// V14.9-G: handle `verifyCudaPack`. Re-runs the CUDA + cuDNN probe
/// and emits a `HardwareReprobed` event with the fresh `HardwareInfo`
/// plus a `diagnostics` string when the pack is absent. Lets the
/// Settings → Performance card flip to ✓ the moment the user installs
/// cuDNN, without an engine restart.
async fn handle_verify_cuda_pack(sink: &Sink) {
    let hardware = build_hardware_info();
    let diagnostics = models::runtime::probe_cuda_pack().diagnostics;
    tracing::info!(
        cuda_pack_present = hardware.cuda_pack_present,
        execution_provider = %hardware.execution_provider,
        "[VERIFY] hardware reprobed"
    );
    sink.send(IpcEvent::now(EventPayload::HardwareReprobed(Wrap::new(
        ipc::HardwareReprobed { hardware, diagnostics },
    ))))
    .await;
}

fn init_tracing() -> Result<()> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let logs_dir = paths::logs_dir().context("resolving logs dir")?;
    std::fs::create_dir_all(&logs_dir).context("creating logs dir")?;

    // Rolling daily JSON log. Local-only, no network sink. PII-redaction
    // happens at call sites, not here.
    let file_appender = tracing_appender::rolling::daily(&logs_dir, "engine.jsonl");
    let (file_writer, _file_guard) = tracing_appender::non_blocking(file_appender);
    // _file_guard must outlive the program; we leak it on purpose so the
    // appender flushes on every event. Engine lifetime is short enough that
    // this is fine; main returns once and process exits.
    Box::leak(Box::new(_file_guard));

    let file_layer = fmt::layer()
        .json()
        .with_writer(file_writer)
        .with_target(true)
        .with_current_span(false);

    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(true);

    let env_filter = EnvFilter::try_from_env("FILEID_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .with(stderr_layer)
        .init();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{append_caption_chunk, is_safe_filename};
    use std::sync::Arc;
    use parking_lot::Mutex;

    fn run_caption_chunks(chunks: &[&str]) -> String {
        let buf = Arc::new(Mutex::new(String::new()));
        for c in chunks {
            append_caption_chunk(&buf, c);
        }
        let result = buf.lock().clone();
        result
    }

    #[test]
    fn caption_chunks_join_with_single_space() {
        // Word-per-chunk: standard prose.
        let out = run_caption_chunks(&["A", "dog", "sits", "on", "a", "couch"]);
        assert_eq!(out, "A dog sits on a couch");
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

    #[test]
    fn safe_filenames_accepted() {
        assert!(is_safe_filename("photo.jpg"));
        assert!(is_safe_filename("My Vacation Photo (2024).heic"));
        assert!(is_safe_filename("a"));
    }

    #[test]
    fn traversal_rejected() {
        assert!(!is_safe_filename(".."));
        assert!(!is_safe_filename("."));
        assert!(!is_safe_filename("../etc/passwd"));
        assert!(!is_safe_filename("..\\windows\\system32"));
        assert!(!is_safe_filename("a/b"));
        assert!(!is_safe_filename("a\\b"));
        assert!(!is_safe_filename("/abs"));
        assert!(!is_safe_filename("\\abs"));
        assert!(!is_safe_filename("C:\\evil.exe"));
        assert!(!is_safe_filename("\\\\unc\\share\\evil.exe"));
        assert!(!is_safe_filename(""));
        assert!(!is_safe_filename("  "));
        assert!(!is_safe_filename(" leading-space.jpg"));
        assert!(!is_safe_filename("trailing-space.jpg "));
    }
}
