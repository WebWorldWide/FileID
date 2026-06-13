// ScanSession — top-level scan orchestrator. Drives Discovery → Tagging
// → DBWriter end-to-end with phase + progress + per-batch summary
// emission. Acquires a process-priority boost and a sleep-prevention
// guard for the duration of the scan.
//
// One session = one "scan a folder" run. Holds the coordinator + child
// pipeline tasks, calls into the IPC sink for `phaseChanged`, `progress`,
// `batchSummary`, `scanComplete` events. Follow-on phases (face clustering,
// deep analyze, restructure plan) each live in their own handler.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::Connection;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::coordinator::ScanCoordinator;
use crate::ipc::{
    sink::Sink, BatchSummary, EventPayload, IpcEvent, ScanComplete, ScanPhase, ScanProgress, Wrap,
};
use crate::pipeline::dbwriter::{BatchStats, DbWriter};
use crate::pipeline::discovery::Discovery;
use crate::pipeline::tagging::{ModelStack, Tagger, TaggedFile};
use crate::platform::{PriorityBoost, SleepGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SessionPhase {
    Idle,
    Discovering,
    Tagging,
    PostScan,
    Completed,
    Cancelled,
    Failed,
}

impl SessionPhase {
    fn as_ipc_phase(self) -> ScanPhase {
        match self {
            SessionPhase::Idle => ScanPhase::Idle,
            SessionPhase::Discovering => ScanPhase::Discovering,
            SessionPhase::Tagging => ScanPhase::Tagging,
            SessionPhase::PostScan => ScanPhase::PostScan,
            SessionPhase::Completed => ScanPhase::Completed,
            SessionPhase::Cancelled => ScanPhase::Cancelled,
            SessionPhase::Failed => ScanPhase::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct SessionResult {
    pub processed: u64,
    pub failed: u64,
    pub elapsed_seconds: f64,
}

pub struct ScanSession {
    pub session_id: String,
    coordinator: ScanCoordinator,
    db_conn: Arc<Mutex<Connection>>,
    worker_count: usize,
    sink: Sink,
    models: Arc<ModelStack>,
    /// When true, skip the incremental-rescan skip set and reprocess
    /// every file. Plumbed from `StartScanPayload::rescan`.
    rescan: bool,
}

impl ScanSession {
    #[allow(dead_code)]
    pub fn new(
        coordinator: ScanCoordinator,
        db_conn: Arc<Mutex<Connection>>,
        worker_count: usize,
        sink: Sink,
        models: Arc<ModelStack>,
    ) -> Self {
        Self::new_with_options(coordinator, db_conn, worker_count, sink, models, false)
    }

    pub fn new_with_options(
        coordinator: ScanCoordinator,
        db_conn: Arc<Mutex<Connection>>,
        worker_count: usize,
        sink: Sink,
        models: Arc<ModelStack>,
        rescan: bool,
    ) -> Self {
        Self {
            session_id: Uuid::new_v4().to_string(),
            coordinator,
            db_conn,
            worker_count: worker_count.max(1),
            sink,
            models,
            rescan,
        }
    }

    /// Run an end-to-end scan against `root`. Returns when DBWriter has
    /// drained the final batch, every worker has exited, and the
    /// `scanComplete` IPC event has been emitted.
    pub async fn run<F>(self, root: &Path, mut on_phase: F) -> Result<SessionResult>
    where
        F: FnMut(SessionPhase),
    {
        // Boost process priority + prevent sleep for the lifetime of the
        // scan. Both are RAII so a panic / cancel still releases them.
        let _priority = PriorityBoost::acquire();
        let _sleep = SleepGuard::acquire();

        let started = Instant::now();
        let session_id = self.session_id.clone();
        let sink = self.sink.clone();

        let emit_phase = |phase: SessionPhase| {
            // Intentional try_send + drop-on-overflow. Phase changes are
            // low-frequency (a few per scan) so the drop is unlikely
            // in practice, but a spawn(async { send.await }) pattern
            // could pile up unbounded tasks waiting on a full sink.
            // try_send bounds the engine's worst-case memory.
            //
            // C1-002: this droppable path is for NON-terminal phases only.
            // Terminal phases (Cancelled / Failed / Completed) MUST go through
            // `emit_phase_guaranteed` below — a dropped terminal PhaseChanged
            // makes the app render a cancelled/failed scan as Completed and
            // auto-fire face clustering. The engine's "terminal events must
            // not drop" rule.
            let p = phase.as_ipc_phase();
            let _ = sink.try_send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(p))));
        };

        // C1-002: guaranteed (blocking/backpressuring) terminal-phase emit.
        // Awaits channel capacity rather than dropping, so a cancelled/failed
        // terminal state can never be silently lost under sink backpressure.
        let emit_phase_guaranteed = |sink: &Sink, phase: SessionPhase| {
            let p = phase.as_ipc_phase();
            let sink = sink.clone();
            async move {
                sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(p))))
                    .await;
            }
        };

        on_phase(SessionPhase::Discovering);
        emit_phase(SessionPhase::Discovering);

        // Persist a scan_sessions row so Settings → Recent scans can list
        // past scans + offer Re-scan. Uses the engine's single-writer
        // connection — same one DBWriter holds.
        let started_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        {
            let conn = self.db_conn.lock();
            let _ = conn.execute(
                "INSERT OR REPLACE INTO scan_sessions \
                 (id, root_path, started_at, completed_at, last_file_index, total_files, status) \
                 VALUES (?1, ?2, ?3, NULL, NULL, NULL, 'running')",
                rusqlite::params![
                    session_id,
                    root.to_string_lossy(),
                    started_unix,
                ],
            );
        }

        // Pre-load the "already current" set from DB so Discovery can skip
        // files whose `scanned_at >= modified_at`. For a 1M-file repeat
        // scan this turns an 8-hour redo into ~1-second startup + ~0
        // discovery walk. The rescan flag forces a full rescan.
        let skip_paths = if self.rescan {
            tracing::info!("[SCAN] rescan=true; processing every file regardless of prior scan state");
            std::sync::Arc::new(std::collections::HashSet::new())
        } else {
            let conn = self.db_conn.lock();
            let mut set = std::collections::HashSet::<std::path::PathBuf>::new();
            let root_prefix = root.to_string_lossy().to_string();
            // Diagnostic counts so we can tell at a glance whether (a) the
            // DB is empty, (b) the LIKE prefix is wrong, or (c) the
            // scanned_at >= modified_at filter excludes everything.
            let total_files: i64 = conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                .unwrap_or(0);
            let lo = root_prefix.trim_end_matches(['\\', '/']).to_string();
            // P16: a BINARY range seek (`>= lo AND < hi`) is sargable on the
            // implicit UNIQUE index on path_text, unlike `LIKE 'lo%'` (which is
            // non-sargable because LIKE defaults to case-insensitive and forces
            // a full table scan). Stored paths derive from THIS root so they
            // share its exact casing; the skip set is an optimization that fails
            // safe (a miss just re-scans the file).
            let hi = prefix_upper_bound(&lo);
            let prefix_match: i64 = {
                match &hi {
                    Some(hi) => conn.query_row(
                        "SELECT COUNT(*) FROM files WHERE path_text >= ?1 AND path_text < ?2",
                        rusqlite::params![lo, hi],
                        |r| r.get(0),
                    ),
                    None => conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)),
                }
                .unwrap_or(0)
            };
            // failed=0 excludes prior failures (they must retry every scan).
            // modified_at IS NULL rows fall out automatically: NULL comparisons
            // are NULL → treated as false in WHERE, so the row is omitted from
            // the skip set and gets reprocessed.
            //
            // C1-013: a content-bearing file scanned as an online-only
            // (dehydrated OneDrive Files-On-Demand) placeholder records
            // content_hash=NULL — the tagger skips the content read so it
            // never triggers a network hydration. Hydration does NOT bump
            // modified_at, so the bare `scanned_at >= modified_at` predicate
            // would strand the now-local file in the skip set forever. Exclude
            // any content-bearing kind that lacks a content_hash so a
            // dehydrated→hydrated file is reprocessed; a transient hash failure
            // re-scans too, which is the skip set's documented fail-safe.
            let content_hash_gate = SKIP_SET_CONTENT_HASH_GATE;
            let select_sql = if hi.is_some() {
                format!(
                    "SELECT path_text FROM files \
                     WHERE path_text >= ?1 AND path_text < ?2 \
                     AND failed = 0 \
                     AND scanned_at >= modified_at \
                     {content_hash_gate}"
                )
            } else {
                format!(
                    "SELECT path_text FROM files \
                     WHERE failed = 0 \
                     AND scanned_at >= modified_at \
                     {content_hash_gate}"
                )
            };
            if let Ok(mut stmt) = conn.prepare(&select_sql) {
                // One closure literal shared across both arms — two separate
                // closures are distinct types and `match` can't unify them.
                let row_to_string = |r: &rusqlite::Row| r.get::<_, String>(0);
                let rows = match &hi {
                    Some(hi) => stmt.query_map(rusqlite::params![lo, hi], row_to_string),
                    None => stmt.query_map([], row_to_string),
                };
                if let Ok(rows) = rows {
                    for p in rows.flatten() {
                        set.insert(std::path::PathBuf::from(p));
                    }
                }
            }
            tracing::info!(
                already_current = set.len(),
                files_total = total_files,
                files_under_root = prefix_match,
                root = %crate::platform::redact_path_for_log(&root_prefix),
                "[SCAN] preloaded skip set for incremental rescan"
            );
            std::sync::Arc::new(set)
        };

        // Wire the three pipeline stages with bounded mpsc channels.
        // Capture the skip-set size before it moves into Discovery: a count==0
        // result with a non-empty skip set means "incremental rescan, all files
        // already current" — NOT an empty/unsupported folder (#21).
        let skip_count = skip_paths.len();
        let discovery = Discovery::new_with_skip(root, self.coordinator.clone(), skip_paths);
        let handle = discovery.spawn();
        let discovered_count = handle.count.clone();
        let discovered_done = handle.done.clone();
        let discovered_errors = handle.error_count.clone();
        let discovered_rx = handle.rx;

        // Emit a live Progress event every 250 ms while discovery walks
        // the tree. Without this, the sidebar stays on "Discovering…" with
        // every stat row reading "—" until the first DBWriter batch flushes
        // (often >1 s on cold cache; never if every file was filtered out).
        // Also surfaces an empty-folder event so the UI doesn't hang.
        let sink_for_tick = sink.clone();
        let session_id_for_tick = session_id.clone();
        let coord_for_tick = self.coordinator.clone();
        let root_for_tick = root.to_string_lossy().into_owned();
        let errors_for_tick = discovered_errors.clone();
        // Clone the discovery counter for the ticker; the original stays
        // in scope so the tagging callback can also read it for Progress
        // event totals.
        let discovered_count_for_tick = discovered_count.clone();
        // Single-shot guard shared with the post-drain fallback below: the tick
        // sleeps 250 ms before its first wake-up, so a folder that discovers and
        // drains faster than that (empty folder / fully-current rescan) would be
        // aborted before the tick emits its notice. Whichever path wins the swap
        // emits; the other skips. (audit E3)
        let notice_emitted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notice_emitted_for_tick = notice_emitted.clone();
        // discoveryComplete must fire EXACTLY ONCE on every scan terminal path
        // (normal, empty, sub-250 ms drain, cancel-during-discovery) — the IPC
        // parity invariant the app's progress bar depends on for its file total.
        // The tick emits it on the normal path; the post-drain backstop below
        // covers the paths where the tick is aborted mid-sleep or returns early
        // on cancel. This shared single-shot flag keeps the two from double-
        // emitting. (audit F-C2-006)
        let discovery_complete_emitted =
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let discovery_complete_emitted_for_tick = discovery_complete_emitted.clone();
        // The tick takes `discovered_done` by move (async move); keep a clone for
        // the post-drain fallback's done-check below. (audit E3)
        let discovered_done_post = discovered_done.clone();
        let tick = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if coord_for_tick.is_cancelled() {
                    return;
                }
                let count = discovered_count_for_tick.load(std::sync::atomic::Ordering::Relaxed);
                let finished = discovered_done.load(std::sync::atomic::Ordering::Acquire);
                let _ = sink_for_tick.try_send(IpcEvent::now(EventPayload::Progress(Wrap::new(
                    ScanProgress {
                        session_id: session_id_for_tick.clone(),
                        phase: ScanPhase::Discovering,
                        total: 0,
                        discovered: count,
                        processed: 0,
                        failed: 0,
                        files_per_second: 0.0,
                        eta_seconds: None,
                        resident_mb: crate::platform::process_memory_mb(),
                        available_mb: 0,
                    },
                ))));
                if finished {
                    // IPC parity with macOS (FileIDEngineMain.swift:486):
                    // announce the discovery total once the walk ends —
                    // macOS emits it even for a cancel-truncated walk. Swap the
                    // shared flag first so the post-drain backstop never re-emits
                    // what the tick already sent. (audit F-C2-006)
                    if !discovery_complete_emitted_for_tick
                        .swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        let _ = sink_for_tick.try_send(IpcEvent::now(
                            EventPayload::DiscoveryComplete(
                                crate::ipc::DiscoveryCompletePayload { total_files: count },
                            ),
                        ));
                    }
                    // Empty-folder path: tagging would wait forever on its
                    // input channel if discovery produced zero files.
                    // Close out cleanly with a "no supported files found"
                    // error so the user knows what happened.
                    let errs = errors_for_tick.load(std::sync::atomic::Ordering::Relaxed);
                    let already =
                        notice_emitted_for_tick.swap(true, std::sync::atomic::Ordering::SeqCst);
                    if !already && count == 0 && skip_count > 0 {
                        // Incremental rescan where every file was already
                        // current — not an error. Non-fatal "already up to
                        // date" notice (#21).
                        let _ = sink_for_tick.try_send(IpcEvent::now(EventPayload::Error(
                            Wrap::new(crate::ipc::EngineError {
                                kind: "rescan_no_changes".into(),
                                message: "Library is already up to date — no new or changed files to scan."
                                    .into(),
                                path: Some(root_for_tick.clone()),
                                model_kind: None,
                            }),
                        )));
                    } else if !already && count == 0 {
                        // Genuinely empty / unsupported folder.
                        let _ = sink_for_tick.try_send(IpcEvent::now(EventPayload::Error(
                            Wrap::new(crate::ipc::EngineError {
                                kind: "empty_folder".into(),
                                message: format!(
                                    "No supported files found in {}.\n\
                                     Pick a folder with images, videos, PDFs, or documents.",
                                    root_for_tick
                                ),
                                path: Some(root_for_tick.clone()),
                                model_kind: None,
                            }),
                        )));
                    } else if !already && errs > 0 {
                        // Non-fatal: walk recovered after the failures.
                        let _ = sink_for_tick.try_send(IpcEvent::now(EventPayload::Error(
                            Wrap::new(crate::ipc::EngineError {
                                kind: "discovery_partial".into(),
                                message: format!(
                                    "Scanned {} file(s); {} path(s) couldn't be read \
                                     (permission denied or removed mid-scan). Scan continues.",
                                    count, errs
                                ),
                                path: Some(root_for_tick.clone()),
                                model_kind: None,
                            }),
                        )));
                    }
                    return;
                }
            }
        });

        on_phase(SessionPhase::Tagging);
        emit_phase(SessionPhase::Tagging);
        let tagger = Tagger::new(self.coordinator.clone(), self.worker_count, self.models.clone());
        let tagged_rx: mpsc::Receiver<TaggedFile> = tagger.spawn(discovered_rx);

        // Throttle progress emission: at most one event per 100 ms OR
        // every 1000 files (whichever first). Without throttling, a fast
        // scan floods the IPC channel and the app dispatcher.
        let progress_state = Arc::new(Mutex::new(ProgressState::new()));
        let writer = DbWriter::new(self.db_conn.clone(), self.coordinator.clone());

        let sink_for_batch = sink.clone();
        let session_id_for_batch = session_id.clone();
        let progress_state_for_batch = progress_state.clone();
        // Clone the discovery counter for the tagging callback so Progress
        // events during tagging report the real discovered total — else
        // the sidebar progress bar pegs at 100 % during tagging.
        let discovered_count_for_batch = discovered_count.clone();

        let writer_outcome = writer
            .run(tagged_rx, move |stats: BatchStats| {
                emit_batch_summary(&sink_for_batch, &stats);
                let discovered = discovered_count_for_batch.load(std::sync::atomic::Ordering::Relaxed);
                maybe_emit_progress(
                    &sink_for_batch,
                    &progress_state_for_batch,
                    &session_id_for_batch,
                    &stats,
                    discovered,
                );
            })
            .await;

        // Tick exits on its own once discovery's `done` flips true; this
        // abort is belt-and-suspenders for the rare case where DBWriter
        // returns before tick observes the done flag (cancel mid-walk).
        // Must run BEFORE the error propagation below, or a failed writer
        // leaves the tick emitting Discovering progress after the caller's
        // PhaseChanged(Failed).
        tick.abort();

        let (total, failed) = match writer_outcome {
            Ok(t) => t,
            Err(err) => {
                // Stamp the row 'failed' so Settings → Recent scans doesn't
                // show it as 'running' forever. Best-effort: the writer
                // failure may be the DB itself (SQLITE_FULL / unreachable).
                let conn = self.db_conn.lock();
                let completed_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let _ = conn.execute(
                    "UPDATE scan_sessions SET completed_at = ?1, status = 'failed' WHERE id = ?2",
                    rusqlite::params![completed_unix, session_id],
                );
                return Err(err);
            }
        };

        // discoveryComplete backstop (audit F-C2-006): the 250 ms tick emits this
        // on a normal scan, but a sub-250 ms drain (empty folder / fully-current
        // rescan) aborts the tick mid-first-sleep, and a cancel-during-discovery
        // returns the tick early — both BEFORE it emits. The walk has ended by
        // here (writer.run drained tagged_rx), so `discovered_count` is final.
        // Emit unconditionally w.r.t. cancel to match the macOS reference, which
        // announces the total even on a cancel-truncated walk; the shared single-
        // shot flag the tick also swaps guarantees exactly-once across both paths.
        if !discovery_complete_emitted.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let count = discovered_count.load(std::sync::atomic::Ordering::Relaxed);
            sink.send(IpcEvent::now(EventPayload::DiscoveryComplete(
                crate::ipc::DiscoveryCompletePayload { total_files: count },
            )))
            .await;
        }

        // Post-drain fallback for the empty/rescan/partial notice: if discovery
        // finished but the tick was aborted before it could emit (drained inside
        // its 250 ms first sleep — empty folder or fully-current rescan), emit it
        // here so the UI never silently hangs on "Discovering…". Single-shot via
        // the shared flag so we never double-emit what the tick already sent. (audit E3)
        if !self.coordinator.is_cancelled()
            && discovered_done_post.load(std::sync::atomic::Ordering::Acquire)
            && !notice_emitted.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let count = discovered_count.load(std::sync::atomic::Ordering::Relaxed);
            let errs = discovered_errors.load(std::sync::atomic::Ordering::Relaxed);
            let root_display = root.to_string_lossy().into_owned();
            let notice = if count == 0 && skip_count > 0 {
                Some((
                    "rescan_no_changes".to_string(),
                    "Library is already up to date — no new or changed files to scan.".to_string(),
                ))
            } else if count == 0 {
                Some((
                    "empty_folder".to_string(),
                    format!(
                        "No supported files found in {}.\n\
                         Pick a folder with images, videos, PDFs, or documents.",
                        root_display
                    ),
                ))
            } else if errs > 0 {
                Some((
                    "discovery_partial".to_string(),
                    format!(
                        "Scanned {} file(s); {} path(s) couldn't be read \
                         (permission denied or removed mid-scan). Scan continues.",
                        count, errs
                    ),
                ))
            } else {
                None
            };
            if let Some((kind, message)) = notice {
                sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(
                    crate::ipc::EngineError {
                        kind,
                        message,
                        path: Some(root_display),
                        model_kind: None,
                    },
                ))))
                .await;
            }
        }

        let elapsed = started.elapsed().as_secs_f64();

        // Stamp the scan_sessions row with completed_at + status. gpu-dead
        // wins over cancelled: a TDR-aborted scan emits PhaseChanged(Failed)
        // below and must not be recorded as completed/cancelled.
        {
            let conn = self.db_conn.lock();
            let final_status = if self.coordinator.is_gpu_dead() {
                "failed"
            } else if self.coordinator.is_cancelled() {
                "cancelled"
            } else {
                "completed"
            };
            let completed_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let _ = conn.execute(
                "UPDATE scan_sessions SET completed_at = ?1, total_files = ?2, status = ?3 WHERE id = ?4",
                rusqlite::params![completed_unix, total as i64, final_status, session_id],
            );
        }

        if self.coordinator.is_gpu_dead() {
            // Distinct error kind so the C# app can show a
            // "GPU device suspended — restart the app to recover" banner
            // instead of the generic cancelled state. Emit BEFORE
            // PhaseChanged(Failed) so IPC consumers see the cause first.
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(
                crate::ipc::EngineError {
                    kind: "gpu_device_removed".into(),
                    message: "Windows reported GPU device removed (TDR). The scan has been aborted to keep the system responsive. Restart the engine to recover; if this repeats, consider lowering FILEID_MODEL_POOL_SIZE or switching to CPU EP via gpuExecutionProviderOverride.".into(),
                    path: None,
                    model_kind: None,
                },
            ))))
            .await;
            on_phase(SessionPhase::Failed);
            // C1-002: terminal Failed phase via the guaranteed path — a drop
            // here would render the TDR-aborted scan as Completed and auto-fire
            // face clustering on a half-scanned library.
            emit_phase_guaranteed(&sink, SessionPhase::Failed).await;
            // C1-002 backstop: Failed had no terminal ScanComplete. Without it,
            // a consumer that missed the (now-guaranteed) PhaseChanged still has
            // no terminal frame to settle the scan. Emit one carrying the final
            // counts; the preceding phaseChanged(failed) is the authoritative
            // STATE — consumers must not infer "completed" from this event.
            sink.send(IpcEvent::now(EventPayload::ScanComplete(Wrap::new(
                ScanComplete {
                    session_id: session_id.clone(),
                    total_files: discovered_count.load(std::sync::atomic::Ordering::Relaxed),
                    processed_files: total,
                    failed_files: failed,
                    total_seconds: elapsed,
                },
            ))))
            .await;
        } else if self.coordinator.is_cancelled() {
            on_phase(SessionPhase::Cancelled);
            // C1-002: terminal Cancelled phase via the guaranteed path — a drop
            // makes the app treat a cancelled scan as Completed.
            emit_phase_guaranteed(&sink, SessionPhase::Cancelled).await;
            // IPC parity with macOS (markSessionFinal emits scanComplete
            // unconditionally, pinned by ScanCancellationTests): cancelled
            // scans still get the terminal scanComplete carrying the final
            // counts. Terminal STATE stays the preceding
            // phaseChanged(cancelled) — consumers must not infer
            // "completed" from this event.
            // totalFiles = discovered (can exceed processed when the
            // cancel landed mid-tagging) — macOS emits the same split.
            sink.send(IpcEvent::now(EventPayload::ScanComplete(Wrap::new(
                ScanComplete {
                    session_id: session_id.clone(),
                    total_files: discovered_count.load(std::sync::atomic::Ordering::Relaxed),
                    processed_files: total,
                    failed_files: failed,
                    total_seconds: elapsed,
                },
            ))))
            .await;
        } else {
            on_phase(SessionPhase::PostScan);
            emit_phase(SessionPhase::PostScan);
            on_phase(SessionPhase::Completed);
            // C1-002: terminal Completed phase via the guaranteed path so the
            // app's terminal state can never be lost under backpressure.
            emit_phase_guaranteed(&sink, SessionPhase::Completed).await;
            sink.send(IpcEvent::now(EventPayload::ScanComplete(Wrap::new(
                ScanComplete {
                    session_id: session_id.clone(),
                    total_files: total,
                    processed_files: total,
                    failed_files: failed,
                    total_seconds: elapsed,
                },
            ))))
            .await;
        }

        Ok(SessionResult {
            processed: total,
            failed,
            elapsed_seconds: elapsed,
        })
    }
}

/// Throttle state for progress emission. The DBWriter callback fires per
/// batch (every 100 files or 200 ms), so we further throttle into the
/// IPC sink to at most ~10 Hz.
struct ProgressState {
    last_emit: Instant,
    last_total: u64,
    // Rolling throughput for the ETA, measured from the processed-count delta
    // over REAL wall-clock time between emits. This replaces the per-batch
    // `files_in_batch / flush_wall` rate, which measured only the DB-INSERT
    // speed (hundreds–thousands of files/s) and produced absurd ETAs — e.g.
    // "13s" for an hour of actual work. EMA-smoothed, mirroring the macOS
    // ScanCoordinator (0.7 old / 0.3 new).
    rate_anchor: Instant,
    rate_anchor_total: u64,
    rolling_fps: f64,
}

impl ProgressState {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            // -60 s forces the first batch callback past the throttle so the
            // sidebar fills immediately; the rate anchor below uses the real
            // `now` so the first measured interval is honest.
            last_emit: now - Duration::from_secs(60),
            last_total: 0,
            rate_anchor: now,
            rate_anchor_total: 0,
            rolling_fps: 0.0,
        }
    }

    /// Fold a new processed-count sample into the rolling files/sec and return
    /// it. Re-samples only after `MIN_RATE_DT` so a burst of file-triggered
    /// emits a few ms apart can't divide by a near-zero interval; between
    /// re-samples it returns the last rolling value unchanged.
    fn observe_rate(&mut self, processed_total: u64, now: Instant) -> f64 {
        const MIN_RATE_DT: f64 = 0.5;
        let dt = now.duration_since(self.rate_anchor).as_secs_f64();
        if dt >= MIN_RATE_DT {
            let delta = processed_total.saturating_sub(self.rate_anchor_total) as f64;
            let instant = delta / dt;
            self.rolling_fps = if self.rolling_fps <= 0.0 {
                instant
            } else {
                0.7 * self.rolling_fps + 0.3 * instant
            };
            self.rate_anchor = now;
            self.rate_anchor_total = processed_total;
        }
        self.rolling_fps
    }
}

fn emit_batch_summary(sink: &Sink, stats: &BatchStats) {
    let summary = BatchSummary {
        batch_index: stats.batch_index,
        files_in_batch: stats.files_in_batch,
        processed_total: stats.processed_total,
        wall_seconds: stats.wall_seconds,
        files_per_second: stats.files_per_second,
        utilization: stats.utilization,
        vision_p50_ms: stats.vision_p50_ms,
        vision_p95_ms: stats.vision_p95_ms,
        clip_p50_ms: stats.clip_p50_ms,
        clip_p95_ms: stats.clip_p95_ms,
        store_insert_p50_ms: stats.store_insert_p50_ms,
        store_insert_p95_ms: stats.store_insert_p95_ms,
        resident_mb: crate::platform::process_memory_mb(),
        available_mb: 0,
    };
    // Intentional try_send. Per-batch BatchSummary events (one per ~100
    // files) are best-effort — dropping during a sink-full burst is
    // preferable to spawning an unbounded tail of tasks awaiting capacity.
    let _ = sink.try_send(IpcEvent::now(EventPayload::BatchSummary(Wrap::new(summary))));
}

/// C1-013 skip-set guard: drop any content-bearing row that was scanned
/// without a content_hash (the signature of an online-only OneDrive
/// placeholder, whose content read is skipped to avoid a network hydration).
/// Because hydration doesn't bump `modified_at`, this is the only durable
/// signal that a now-local file still needs ML processing.
pub(crate) const SKIP_SET_CONTENT_HASH_GATE: &str =
    "AND NOT (content_hash IS NULL \
      AND kind IN ('image', 'video', 'pdf', 'doc', 'audio'))";

/// Exclusive upper bound for a sargable prefix range: `prefix` with its last
/// Unicode scalar incremented, so `path_text >= prefix AND path_text < upper`
/// selects exactly the rows `LIKE 'prefix%'` would (BINARY collation). Returns
/// None when no finite bound exists (empty prefix, or an all-`char::MAX` tail)
/// — callers then match the whole table.
pub(crate) fn prefix_upper_bound(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(last) = chars.pop() {
        let mut cp = last as u32 + 1;
        // Jump the UTF-16 surrogate gap — those code points aren't scalars.
        if (0xD800..=0xDFFF).contains(&cp) {
            cp = 0xE000;
        }
        if let Some(next) = char::from_u32(cp) {
            chars.push(next);
            return Some(chars.iter().collect());
        }
        // cp > 0x10FFFF (last was char::MAX) — carry into the previous char.
    }
    None
}

const PROGRESS_THROTTLE_MS: u128 = 100;
const PROGRESS_THROTTLE_FILES: u64 = 1000;

fn maybe_emit_progress(
    sink: &Sink,
    state: &Arc<Mutex<ProgressState>>,
    session_id: &str,
    stats: &BatchStats,
    discovered_total: u64,
) {
    let now = Instant::now();
    let should_emit = {
        let st = state.lock();
        let elapsed_ms = now.duration_since(st.last_emit).as_millis();
        let files_since = stats.processed_total.saturating_sub(st.last_total);
        elapsed_ms >= PROGRESS_THROTTLE_MS || files_since >= PROGRESS_THROTTLE_FILES
    };
    if !should_emit {
        return;
    }
    // Fold the new sample into the rolling wall-clock throughput under one lock.
    let fps = {
        let mut st = state.lock();
        st.last_emit = now;
        st.last_total = stats.processed_total;
        st.observe_rate(stats.processed_total, now)
    };
    // Progress payload fields:
    //   total            = discovered file count (from Discovery's atomic
    //                      counter; persisted through tagging so the
    //                      progress bar shows correct fill, not 100%).
    //   files_per_second = the ROLLING wall-clock rate (real end-to-end
    //                      throughput), NOT the per-batch DB-flush rate.
    //   eta_seconds      = (total - processed) / rolling_fps when known;
    //                      None during ramp-up (first ~0.5 s, or total==0).
    //   failed           = cumulative failed-file count from DBWriter.
    //   resident_mb      = process RSS via Win32 GetProcessMemoryInfo.
    let total = discovered_total.max(stats.processed_total);
    let remaining = total.saturating_sub(stats.processed_total);
    let eta_seconds = if fps > 0.01 && remaining > 0 && total > 0 {
        Some(remaining as f64 / fps)
    } else {
        None
    };
    let progress = ScanProgress {
        session_id: session_id.into(),
        phase: ScanPhase::Tagging,
        total,
        discovered: discovered_total,
        processed: stats.processed_total,
        failed: stats.failed_total,
        files_per_second: fps,
        eta_seconds,
        resident_mb: crate::platform::process_memory_mb(),
        available_mb: 0,
    };
    // Intentional try_send. Progress events are throttled to 10 Hz / 1k
    // files, so dropping is rare in practice — and the next emit (≤100ms
    // later) brings the UI back in sync.
    let _ = sink.try_send(IpcEvent::now(EventPayload::Progress(Wrap::new(progress))));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rolling fps must measure real wall-clock throughput, not the
    /// per-batch DB-flush rate — the regression that produced "13s" ETAs for
    /// an hour of work.
    #[test]
    fn rolling_fps_measures_wallclock_throughput() {
        let mut st = ProgressState::new();
        let t0 = Instant::now();
        st.rate_anchor = t0;
        st.rate_anchor_total = 0;
        st.rolling_fps = 0.0;

        // 5 files in the first second → ~5 files/s (the real pipeline rate),
        // NOT the thousands/s a DB-flush measurement reports.
        let fps1 = st.observe_rate(5, t0 + Duration::from_secs(1));
        assert!((fps1 - 5.0).abs() < 0.01, "first-interval rate should be 5/s, got {fps1}");

        // Sustained 5/s → EMA stays ~5.
        let fps2 = st.observe_rate(10, t0 + Duration::from_secs(2));
        assert!((fps2 - 5.0).abs() < 0.01, "sustained rate ~5/s, got {fps2}");

        // An ETA built on this rate is hours for tens of thousands remaining,
        // not 13 seconds — proving the bug is gone.
        let eta = 18_000.0_f64 / fps2;
        assert!(eta > 3000.0, "eta should be ~hours ({eta}s)");
    }

    /// P16: the prefix upper bound must select exactly the rows `LIKE
    /// 'prefix%'` would under BINARY collation — i.e. lo <= x < hi iff x starts
    /// with prefix.
    #[test]
    fn prefix_upper_bound_brackets_the_prefix() {
        let lo = r"C:\Users\a\Photos";
        let hi = prefix_upper_bound(lo).unwrap();
        assert!(lo < hi.as_str(), "lo must sort before hi");
        // A child path is inside [lo, hi).
        let child = r"C:\Users\a\Photos\2024\img.jpg";
        assert!(lo <= child && child < hi.as_str());
        // A sibling that merely shares a shorter prefix is outside.
        let sibling = r"C:\Users\a\Pictures\x.jpg";
        assert!(!(lo <= sibling && sibling < hi.as_str()));
        // Empty prefix → no finite bound.
        assert_eq!(prefix_upper_bound(""), None);
        // Simple increment.
        assert_eq!(prefix_upper_bound("abc").as_deref(), Some("abd"));
    }

    /// Two emits closer than MIN_RATE_DT must not re-divide and spike the rate.
    #[test]
    fn rolling_fps_holds_between_rapid_samples() {
        let mut st = ProgressState::new();
        let t0 = Instant::now();
        st.rate_anchor = t0;
        st.rate_anchor_total = 0;
        st.rolling_fps = 0.0;

        let _ = st.observe_rate(10, t0 + Duration::from_secs(1)); // seeds ~10/s
        let held = st.observe_rate(1000, t0 + Duration::from_millis(1100)); // only +0.1 s
        assert!(held < 50.0, "a rapid sample (<0.5s) must not spike the rate, got {held}");
    }

    /// C1-013: the incremental skip-set must NOT skip a file whose
    /// reparse/placeholder state changed (dehydrated→hydrated). A OneDrive
    /// online-only placeholder is recorded with `content_hash = NULL` and
    /// `scanned_at >= modified_at`; after the user hydrates it, the modified
    /// time is unchanged, so the bare `scanned_at >= modified_at` predicate
    /// would strand it forever. The content_hash gate must drop it from the
    /// skip set while keeping fully-scanned files in it.
    #[test]
    fn skip_set_excludes_hydrated_placeholder_keeps_scanned_file() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (
                 path_text    TEXT NOT NULL UNIQUE,
                 kind         TEXT NOT NULL,
                 modified_at  DOUBLE,
                 scanned_at   DOUBLE NOT NULL,
                 failed       INTEGER NOT NULL DEFAULT 0,
                 content_hash BLOB
             );",
        )
        .unwrap();
        // A dehydrated image scanned as an online-only placeholder: no
        // content_hash, scanned_at >= modified_at, failed=0. After hydration
        // modified_at is unchanged — the row below is exactly that state.
        conn.execute(
            "INSERT INTO files (path_text, kind, modified_at, scanned_at, failed, content_hash) \
             VALUES ('C:\\OneDrive\\dehydrated.jpg', 'image', 100.0, 200.0, 0, NULL)",
            [],
        )
        .unwrap();
        // A fully-scanned local image: has a content_hash → belongs in the skip set.
        conn.execute(
            "INSERT INTO files (path_text, kind, modified_at, scanned_at, failed, content_hash) \
             VALUES ('C:\\Local\\photo.jpg', 'image', 100.0, 200.0, 0, X'00112233')",
            [],
        )
        .unwrap();
        // A non-content kind (e.g. 'other') legitimately lacks a content_hash
        // and must NOT be force-rescanned by the gate.
        conn.execute(
            "INSERT INTO files (path_text, kind, modified_at, scanned_at, failed, content_hash) \
             VALUES ('C:\\Local\\notes.bin', 'other', 100.0, 200.0, 0, NULL)",
            [],
        )
        .unwrap();

        // Run the EXACT production skip-set predicate (no path-prefix arm).
        let sql = format!(
            "SELECT path_text FROM files \
             WHERE failed = 0 \
             AND scanned_at >= modified_at \
             {SKIP_SET_CONTENT_HASH_GATE}"
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let skip: std::collections::HashSet<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();

        assert!(
            !skip.contains("C:\\OneDrive\\dehydrated.jpg"),
            "a dehydrated→hydrated placeholder (content_hash NULL) must be reprocessed, not skipped"
        );
        assert!(
            skip.contains("C:\\Local\\photo.jpg"),
            "a fully-scanned content-bearing file must stay in the skip set"
        );
        assert!(
            skip.contains("C:\\Local\\notes.bin"),
            "a non-content kind without a content_hash must not be force-rescanned"
        );
    }
}

