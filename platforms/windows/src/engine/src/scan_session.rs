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
// V14.x AutoPilot follow-on (face clustering → deep analyze → restructure
// plan, in that order, on the same session ID) lives separately so
// individual phases stay testable in isolation.

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
use crate::pipeline::tagging::{ModelStack, Tagger, TaggedFile, TAGGING_CHANNEL_CAP};
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
            let p = phase.as_ipc_phase();
            let s = sink.clone();
            tokio::spawn(async move {
                s.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(p))))
                    .await;
            });
        };

        on_phase(SessionPhase::Discovering);
        emit_phase(SessionPhase::Discovering);

        // Wire the three pipeline stages with bounded mpsc channels.
        let discovery = Discovery::new(root, self.coordinator.clone());
        let discovered_rx = discovery.spawn();

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

        let elapsed = started.elapsed().as_secs_f64();

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
    let s = sink.clone();
    tokio::spawn(async move {
        s.send(IpcEvent::now(EventPayload::BatchSummary(Wrap::new(summary))))
            .await;
    });
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
    let s = sink.clone();
    tokio::spawn(async move {
        s.send(IpcEvent::now(EventPayload::Progress(Wrap::new(progress))))
            .await;
    });
}

/// Lift the bounded-channel cap (Tagger → DBWriter) out of the
/// `Tagger` impl detail so AutoPilot can use the same value for its own
/// inter-phase channels.
pub fn tagging_channel_cap() -> usize {
    TAGGING_CHANNEL_CAP
}
