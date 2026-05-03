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
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;

use ipc::{
    sink::Sink, CommandPayload, EngineError, EngineInfo, EventPayload, IpcCommand,
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
    let dispatch_db = db_conn.clone();

    // Active scan coordinator. None when no scan is running; populated by
    // StartScan and consulted by PauseScan / ResumeScan / CancelScan.
    let scan_state: Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>> =
        Arc::new(parking_lot::Mutex::new(None));
    let dispatch_scan_state = scan_state.clone();

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
                            handle_line(
                                &dispatch_sink,
                                &dispatch_shutdown,
                                dispatch_db.as_ref(),
                                &dispatch_scan_state,
                                &text,
                            ).await;
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
    if let Some(conn_arc) = &db_conn {
        let guard = conn_arc.lock();
        if let Err(err) = db::checkpoint_truncate(&guard) {
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

async fn handle_line(
    sink: &Sink,
    shutdown: &Arc<Notify>,
    db: Option<&std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>>,
    scan_state: &Arc<parking_lot::Mutex<Option<coordinator::ScanCoordinator>>>,
    line: &str,
) {
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
        CommandPayload::PrewarmModel(payload) => {
            // Spawn so the IPC loop keeps reading other commands while
            // the (potentially slow) download runs. Each download is its
            // own task; downloads from different prewarm calls run in
            // parallel.
            let sink = sink.clone();
            let model_kind = payload.model_kind.clone();
            tokio::spawn(async move {
                handle_prewarm_model(sink, model_kind).await;
            });
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
        CommandPayload::AutoPilot(_) => {
            // AutoPilot orchestrates scan → cluster → caption → plan in
            // sequence. Each phase needs real ML (Phase 2.6) for the
            // chain to actually produce output. Surface a friendly
            // status event today; Phase 2.6 wires the orchestrator
            // body that drives the four IPC commands.
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "autopilot_pending".into(),
                message: "AutoPilot chains scan → face clustering → captions → restructure plan. \
                          Currently waiting on Phase 2.6 (real ML inference) for the captioning + \
                          face-clustering phases to produce real output. The IPC plumbing is here; \
                          wire when ML lands."
                    .into(),
                path: None,
            }))))
            .await;
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

async fn emit_db_unavailable(sink: &Sink, command: &str) {
    sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
        kind: "db_unavailable".into(),
        message: format!(
            "Cannot run '{command}': the engine couldn't open the SQLite database \
             at startup. Check %LOCALAPPDATA%\\FileID\\logs\\ for the open error."
        ),
        path: None,
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

    let plan = RestructurePlan {
        library_root,
        moves: proposed
            .into_iter()
            .map(|m| IpcMove {
                file_id: m.file_id,
                source: m.source.to_string_lossy().to_string(),
                destination: m.destination.to_string_lossy().to_string(),
                category: m.category,
            })
            .collect(),
        category_counts: category_summary
            .into_iter()
            .map(|c| RestructureCategoryCount { category: c.category, count: c.count })
            .collect(),
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
            }))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "applyRestructure spawn_blocking failed");
        }
    }
}

/// Download every file in the requested model bundle, emit progress
/// events as bytes flow, drop a `.fileid-installed` sentinel when every
/// file lands successfully. The app's ModelInstallerService polls for
/// the sentinel to flip its per-model status to `Installed`.
async fn handle_prewarm_model(sink: Sink, model_kind: String) {
    use crate::downloader::{download_simple, DownloadRequest};
    use crate::ipc::ModelDownloadProgress;
    use models::registry::LookupResult;

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
            }))))
            .await;
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
            return;
        }
    }

    let total_bytes_estimate: u64 = model.files.iter().map(|f| f.approx_bytes).sum();
    let mut bytes_done_aggregate: u64 = 0;
    let file_count = model.files.len();

    for (idx, file) in model.files.iter().enumerate() {
        let label = file
            .dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let model_kind_local = model_kind.clone();
        let display_name = model.display_name.to_string();
        let sink_for_progress = sink.clone();
        let bytes_so_far = bytes_done_aggregate;
        let total_estimate = total_bytes_estimate.max(1);

        // The closure runs synchronously inside `download_simple` whenever
        // a chunk lands. We re-emit it as an IPC event from a spawned
        // task so we don't block the download stream.
        let progress_cb = move |p: crate::downloader::DownloadProgress| {
            let cur_total = bytes_so_far + p.bytes_done;
            let fraction = (cur_total as f64) / (total_estimate as f64);
            let msg = if file_count == 1 {
                format!("Downloading {display_name}…")
            } else {
                format!(
                    "Downloading {display_name} ({of} of {total})…",
                    of = idx + 1,
                    total = file_count
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
        };

        let req = DownloadRequest {
            url: file.url.clone(),
            destination: file.dest.clone(),
            expected_sha256: file.sha256.clone(),
        };

        if let Err(err) = download_simple(req, progress_cb).await {
            tracing::warn!(?err, file = %label, "model download failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "model_download_failed".into(),
                message: format!(
                    "Couldn't download {label}: {err}\n\n\
                     Check your internet connection and try again."
                ),
                path: Some(file.dest.display().to_string()),
            }))))
            .await;
            return;
        }
        bytes_done_aggregate += file.approx_bytes;
    }

    // All files landed — drop the sentinel so the app sees Installed.
    if let Some(sentinel) = models::registry::sentinel_path(&model) {
        if let Err(err) = tokio::fs::write(&sentinel, model.id.as_bytes()).await {
            tracing::warn!(?err, sentinel = %sentinel.display(), "sentinel write failed");
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
            // Reject path components in new_name to prevent traversal.
            if entry.new_name.contains('/') || entry.new_name.contains('\\') || entry.new_name.is_empty() {
                failed += 1;
                messages.push(BulkActionItem {
                    file_id: Some(entry.file_id),
                    ok: false,
                    message: Some("new name must be filename only".into()),
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
        for ((fid, path), trashed_ok) in path_for_id.iter().zip(outcomes.into_iter()) {
            if trashed_ok {
                let _ = tx.execute("DELETE FROM files WHERE id = ?1", rusqlite::params![fid]);
                succeeded += 1;
                messages.push(BulkActionItem {
                    file_id: Some(*fid),
                    ok: true,
                    message: Some(path.to_string_lossy().to_string()),
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
        Ok(BulkActionResult {
            action: "trashFiles".into(),
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
    use std::path::PathBuf;

    let already_running = scan_state.lock().is_some();
    if already_running {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "scan_already_running".into(),
            message: "A scan is already running. Cancel it before starting a new one.".into(),
            path: None,
        }))))
        .await;
        return;
    }

    let coord = coordinator::ScanCoordinator::new();
    *scan_state.lock() = Some(coord.clone());

    // Load ML model weights once per session. Heavy enough to belong on a
    // blocking thread (ORT session create can take 100-500ms per model).
    let models = match tokio::task::spawn_blocking(ModelStack::load_default).await {
        Ok(m) => Arc::new(m),
        Err(err) => {
            tracing::error!(?err, "model stack load panicked");
            *scan_state.lock() = None;
            return;
        }
    };

    let worker_count = platform::default_worker_cap() as usize;
    let session = ScanSession::new(coord, db, worker_count, sink.clone(), models);
    let root = PathBuf::from(payload.root_path.clone());

    let scan_state_release = scan_state.clone();
    let outcome = session.run(&root, |_| {}).await;
    *scan_state_release.lock() = None;

    if let Err(err) = outcome {
        tracing::warn!(?err, root = %root.display(), "scan failed");
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "scan_failed".into(),
            message: format!("Scan failed: {err}"),
            path: Some(payload.root_path),
        }))))
        .await;
    }
}

async fn emit_ready(sink: &Sink) {
    use ipc::HardwareInfo;
    use models::runtime::{ExecutionProvider, GpuVendor, RuntimeProbe};

    // Probe once at ready. Cheap (a single DXGI walk + a few file-exists
    // checks). Persist would happen here in Phase 5; for now we re-probe
    // every spawn so device-driver changes get picked up.
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

    let hardware = HardwareInfo {
        gpu_vendor: vendor_str.into(),
        adapter_name: probe.adapter_name.clone(),
        execution_provider: probe.provider.as_str().into(),
        physical_cpu_cores: num_cpus::get_physical().max(1) as u32,
        cuda_pack_present: probe.cuda_pack_present,
        openvino_pack_present: probe.openvino_pack_present,
        qnn_pack_present: probe.qnn_pack_present,
        recommendation,
    };

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
        CommandPayload::PlanRestructure(_)   => "planRestructure",
        CommandPayload::ApplyRestructure(_)  => "applyRestructure",
        CommandPayload::AutoPilot(_)         => "autoPilot",
        CommandPayload::ApplyTags(_)         => "applyTags",
        CommandPayload::RenameFiles(_)       => "renameFiles",
        CommandPayload::TrashFiles(_)        => "trashFiles",
        CommandPayload::MergeClusters(_)     => "mergeClusters",
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
