// ScanSession — top-level scan orchestrator. Drives Discovery → Tagging
// → DBWriter end-to-end with phase + progress + per-batch summary
// emission. Acquires a process-priority boost and a sleep-prevention
// guard for the duration of the scan.
//
// Mirror of macOS engine/Sources/FileIDEngine/ScanSession.swift. One
// session = one "scan a folder" run. Holds the coordinator + child
// pipeline tasks, calls into the IPC sink for `phaseChanged`,
// `progress`, `batchSummary`, `scanComplete` events.
//
// Follow-on phases (face clustering → deep analyze → restructure plan)
// each live in their own handler so individual phases stay testable in
// isolation; ScanSession is intentionally just one "scan a folder" run.

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
}

impl ScanSession {
    pub fn new(
        coordinator: ScanCoordinator,
        db_conn: Arc<Mutex<Connection>>,
        worker_count: usize,
        sink: Sink,
        models: Arc<ModelStack>,
    ) -> Self {
        Self {
            session_id: Uuid::new_v4().to_string(),
            coordinator,
            db_conn,
            worker_count: worker_count.max(1),
            sink,
            models,
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
            let p = phase.as_ipc_phase();
            let _ = sink.try_send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(p))));
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

        // Wire the three pipeline stages with bounded mpsc channels.
        let discovery = Discovery::new(root, self.coordinator.clone());
        let handle = discovery.spawn();
        let discovered_count = handle.count.clone();
        let discovered_done = handle.done.clone();
        let discovered_errors = handle.error_count.clone();
        let discovered_rx = handle.rx;

        // V14.9-N2.2: emit a live Progress event every 250 ms while
        // discovery is walking the tree. Without this, the user saw the
        // sidebar stuck on "Discovering…" with every stat row reading
        // "—" until the first DBWriter batch flushed (often >1 s on
        // cold cache; never, if every file was filtered out). The tick
        // task also detects "discovery finished with 0 files" and
        // surfaces a clean empty-folder event so the UI exits Discovering
        // instead of hanging forever.
        let sink_for_tick = sink.clone();
        let session_id_for_tick = session_id.clone();
        let coord_for_tick = self.coordinator.clone();
        let root_for_tick = root.to_string_lossy().into_owned();
        let errors_for_tick = discovered_errors.clone();
        let tick = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if coord_for_tick.is_cancelled() {
                    return;
                }
                let count = discovered_count.load(std::sync::atomic::Ordering::Relaxed);
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
                        resident_mb: 0,
                        available_mb: 0,
                    },
                ))));
                if finished {
                    // V14.9-N2.3: graceful empty-folder path. Tagging will
                    // wait on its input channel forever if discovery
                    // produced zero files — close out the scan cleanly
                    // with a "no supported files found" error so the
                    // user knows what happened.
                    let errs = errors_for_tick.load(std::sync::atomic::Ordering::Relaxed);
                    if count == 0 {
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
                    } else if errs > 0 {
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

        let (total, failed) = writer
            .run(tagged_rx, move |stats: BatchStats| {
                emit_batch_summary(&sink_for_batch, &stats);
                maybe_emit_progress(
                    &sink_for_batch,
                    &progress_state_for_batch,
                    &session_id_for_batch,
                    &stats,
                );
            })
            .await?;

        // V14.9-N2: tick task exits on its own once discovery's `done`
        // flips true, so this abort is belt-and-suspenders for the rare
        // case where DBWriter returns before tick observes the done flag
        // (e.g. cancellation mid-walk).
        tick.abort();

        let elapsed = started.elapsed().as_secs_f64();

        // Stamp the scan_sessions row with completed_at + status.
        {
            let conn = self.db_conn.lock();
            let final_status = if self.coordinator.is_cancelled() { "cancelled" } else { "completed" };
            let completed_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let _ = conn.execute(
                "UPDATE scan_sessions SET completed_at = ?1, total_files = ?2, status = ?3 WHERE id = ?4",
                rusqlite::params![completed_unix, total as i64, final_status, session_id],
            );
        }

        if self.coordinator.is_cancelled() {
            on_phase(SessionPhase::Cancelled);
            emit_phase(SessionPhase::Cancelled);
        } else {
            on_phase(SessionPhase::PostScan);
            emit_phase(SessionPhase::PostScan);
            on_phase(SessionPhase::Completed);
            emit_phase(SessionPhase::Completed);
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
}

impl ProgressState {
    fn new() -> Self {
        Self {
            last_emit: Instant::now() - Duration::from_secs(60),
            last_total: 0,
        }
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
        resident_mb: 0,
        available_mb: 0,
    };
    // Intentional try_send. Per-batch BatchSummary events (one per ~100
    // files) are best-effort — dropping during a sink-full burst is
    // preferable to spawning an unbounded tail of tasks awaiting capacity.
    let _ = sink.try_send(IpcEvent::now(EventPayload::BatchSummary(Wrap::new(summary))));
}

const PROGRESS_THROTTLE_MS: u128 = 100;
const PROGRESS_THROTTLE_FILES: u64 = 1000;

fn maybe_emit_progress(
    sink: &Sink,
    state: &Arc<Mutex<ProgressState>>,
    session_id: &str,
    stats: &BatchStats,
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
    {
        let mut st = state.lock();
        st.last_emit = now;
        st.last_total = stats.processed_total;
    }
    let progress = ScanProgress {
        session_id: session_id.into(),
        phase: ScanPhase::Tagging,
        total: stats.processed_total,
        discovered: stats.processed_total,
        processed: stats.processed_total,
        failed: 0,
        files_per_second: stats.files_per_second,
        eta_seconds: None,
        resident_mb: 0,
        available_mb: 0,
    };
    // Intentional try_send. Progress events are throttled to 10 Hz / 1k
    // files, so dropping is rare in practice — and the next emit (≤100ms
    // later) brings the UI back in sync.
    let _ = sink.try_send(IpcEvent::now(EventPayload::Progress(Wrap::new(progress))));
}

