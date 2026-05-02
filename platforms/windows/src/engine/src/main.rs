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

mod db;
mod ipc;
mod paths;
mod platform;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;

use ipc::{
    sink::Sink, CommandPayload, Empty, EngineError, EngineInfo, EventPayload, IpcCommand,
    IpcEvent, Wrap,
};

const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    init_tracing()?;
    let _ = paths::ensure_state_dirs()?; // create %LOCALAPPDATA%/FileID/{logs,Models,...}

    tracing::info!(version = ENGINE_VERSION, "FileIDEngine starting");

    // Open the DB up front so migrations apply (and any failure surfaces
    // before we tell the app we're ready). Checkpoint + close on shutdown.
    let db_path = paths::db_path()?;
    let db_conn = match db::open_writer(&db_path) {
        Ok(c) => Some(c),
        Err(err) => {
            tracing::error!(?err, ?db_path, "failed to open database");
            None
        }
    };

    let (sink, sink_writer) = Sink::spawn();

    // Emit `ready` first thing so the app sidebar can transition out of
    // .starting. The handshake is one-way; the app doesn't ack.
    emit_ready(&sink).await;

    // Coordinated shutdown signal. set() once, awaited by the stdio loop +
    // the parent watchdog so they cooperate on exit.
    let shutdown = Arc::new(Notify::new());

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
    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    let dispatch_sink = sink.clone();
    let dispatch_shutdown = shutdown.clone();

    let stdio_loop = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = dispatch_shutdown.notified() => {
                    tracing::info!("shutdown notified; stdio loop exiting");
                    break;
                }
                line = lines.next_line() => {
                    match line {
                        Ok(Some(text)) if text.trim().is_empty() => continue,
                        Ok(Some(text)) => {
                            handle_line(&dispatch_sink, &dispatch_shutdown, &text).await;
                        }
                        Ok(None) => {
                            tracing::info!("stdin EOF; entering shutdown");
                            dispatch_shutdown.notify_waiters();
                            break;
                        }
                        Err(err) => {
                            tracing::error!(?err, "stdin read error");
                            dispatch_shutdown.notify_waiters();
                            break;
                        }
                    }
                }
            }
        }
    });

    // Wait for shutdown signal (from either source).
    shutdown.notified().await;

    // WAL checkpoint into the main file before exit so the on-disk state is
    // self-contained (no .wal/.shm sidecars needed to read the DB next time).
    if let Some(conn) = &db_conn {
        if let Err(err) = db::checkpoint_truncate(conn) {
            tracing::warn!(?err, "WAL checkpoint at shutdown failed; data is still safe in WAL");
        }
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

async fn handle_line(sink: &Sink, shutdown: &Arc<Notify>, line: &str) {
    let cmd: IpcCommand = match serde_json::from_str(line) {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(%err, "ipc decode failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "ipc_decode_failed".into(),
                message: format!("could not parse command frame: {err}"),
                path: None,
            }))))
            .await;
            return;
        }
    };

    match cmd.payload {
        CommandPayload::RequestStatus(_) => {
            // Re-emit ready so the app can rebuild its EngineInfo snapshot.
            emit_ready(sink).await;
        }
        CommandPayload::Shutdown(_) => {
            tracing::info!("shutdown command received");
            shutdown.notify_waiters();
        }
        // Phase 0 stub: every other variant gets a structured "not implemented"
        // error so the app surfaces it visibly during bring-up. Phase 1+ wires
        // each variant to its real handler.
        other => {
            let kind = command_kind(&other);
            tracing::info!(command = kind, "command received (phase 0 stub)");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "not_implemented".into(),
                message: format!("command '{kind}' not implemented in Phase 0 engine yet"),
                path: None,
            }))))
            .await;
        }
    }
}

async fn emit_ready(sink: &Sink) {
    let info = EngineInfo {
        version: ENGINE_VERSION.into(),
        pid: std::process::id() as i32,
        worker_cap: platform::default_worker_cap(),
        physical_memory_gb: platform::physical_memory_gb(),
    };
    sink.send(IpcEvent::now(EventPayload::Ready(Wrap::new(info))))
        .await;
}

fn command_kind(p: &CommandPayload) -> &'static str {
    match p {
        CommandPayload::StartScan(_)         => "startScan",
        CommandPayload::PauseScan(_)         => "pauseScan",
        CommandPayload::ResumeScan(_)        => "resumeScan",
        CommandPayload::CancelScan(_)        => "cancelScan",
        CommandPayload::RequestStatus(_)     => "requestStatus",
        CommandPayload::Shutdown(_)          => "shutdown",
        CommandPayload::RunFaceClustering(_) => "runFaceClustering",
        CommandPayload::DeepAnalyzeFile(_)   => "deepAnalyzeFile",
        CommandPayload::DeepAnalyzeFolder(_) => "deepAnalyzeFolder",
        CommandPayload::DeepAnalyzeAll(_)    => "deepAnalyzeAll",
        CommandPayload::DeepAnalyzeCancel(_) => "deepAnalyzeCancel",
        CommandPayload::PrewarmModel(_)      => "prewarmModel",
        CommandPayload::CancelPrewarm(_)     => "cancelPrewarm",
    }
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
