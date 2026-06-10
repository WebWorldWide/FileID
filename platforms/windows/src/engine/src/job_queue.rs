#![allow(dead_code)]
// JobQueue — single-FIFO queue for background work the engine runs.
//
// Categories: Scan, FaceCluster, DeepAnalyze. Only one job runs at a time;
// new requests append to the pending list. Each push/pop emits a
// `queueState` IPC event so the app's sidebar queue list stays in sync.
//
// The queue itself doesn't run jobs — it tracks them. Job runners push
// before they start and pop when done; the dispatcher in main.rs wires
// command → push → run → pop.

use parking_lot::Mutex;
use std::sync::Arc;
use uuid::Uuid;

use crate::ipc::{JobCategory, QueueState, QueuedJob};

pub type JobId = String;

#[derive(Clone)]
pub struct JobQueue {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    running: Option<QueuedJob>,
    pending: Vec<QueuedJob>,
    on_change: Vec<Box<dyn Fn(QueueState) + Send + Sync>>,
}

impl JobQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                running: None,
                pending: Vec::new(),
                on_change: Vec::new(),
            })),
        }
    }

    /// Subscribe to queue-state changes. Each call to push/pop/promote
    /// fires every registered listener with the latest snapshot. The
    /// engine's IPC sink subscribes here.
    pub fn on_change<F>(&self, listener: F)
    where
        F: Fn(QueueState) + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock();
        inner.on_change.push(Box::new(listener));
    }

    /// Append a job to the pending list. Returns the job's ID.
    pub fn push(&self, category: JobCategory, title: impl Into<String>, eta_seconds: Option<f64>) -> JobId {
        let job = QueuedJob {
            id: Uuid::new_v4().to_string(),
            category,
            title: title.into(),
            eta_seconds,
        };
        let id = job.id.clone();
        let mut inner = self.inner.lock();
        inner.pending.push(job);
        Self::emit_locked(&inner);
        id
    }

    /// Move the next pending job to running. Returns it, or None if the
    /// pending list was empty.
    pub fn promote_next(&self) -> Option<QueuedJob> {
        let mut inner = self.inner.lock();
        if inner.running.is_some() {
            return None;
        }
        if inner.pending.is_empty() {
            return None;
        }
        let next = inner.pending.remove(0);
        inner.running = Some(next.clone());
        Self::emit_locked(&inner);
        Some(next)
    }

    /// Mark the running job complete (no replacement). Caller invokes
    /// `promote_next` afterwards to start the next pending job.
    pub fn complete_running(&self) {
        let mut inner = self.inner.lock();
        inner.running = None;
        Self::emit_locked(&inner);
    }

    /// Terminal bookkeeping for a job wherever it sits: a running job
    /// is completed and the next pending one promoted; a still-pending
    /// job is dropped. RAII guards call this on Drop so a panicking
    /// task can't wedge the sidebar queue. (Cross-category jobs can
    /// genuinely overlap on this engine; the single-running model is a
    /// sidebar approximation matching macOS's serialized JobQueue, and
    /// it always converges as jobs finish.)
    pub fn finish(&self, id: &str) {
        let mut inner = self.inner.lock();
        if inner.running.as_ref().is_some_and(|r| r.id == id) {
            inner.running = None;
            if !inner.pending.is_empty() {
                let next = inner.pending.remove(0);
                inner.running = Some(next);
            }
            Self::emit_locked(&inner);
            return;
        }
        let before = inner.pending.len();
        inner.pending.retain(|j| j.id != id);
        if inner.pending.len() != before {
            Self::emit_locked(&inner);
        }
    }

    /// Cancel a specific pending job. Returns true if it was found.
    pub fn cancel_pending(&self, id: &str) -> bool {
        let mut inner = self.inner.lock();
        let len_before = inner.pending.len();
        inner.pending.retain(|j| j.id != id);
        let changed = inner.pending.len() != len_before;
        if changed {
            Self::emit_locked(&inner);
        }
        changed
    }

    /// Snapshot for tests + the IPC `requestStatus` reply path.
    pub fn snapshot(&self) -> QueueState {
        Self::build_state(&self.inner.lock())
    }

    fn build_state(inner: &Inner) -> QueueState {
        let total_eta_seconds = {
            let mut total = 0.0;
            let mut any = false;
            if let Some(r) = &inner.running {
                if let Some(e) = r.eta_seconds {
                    total += e;
                    any = true;
                }
            }
            for p in &inner.pending {
                if let Some(e) = p.eta_seconds {
                    total += e;
                    any = true;
                }
            }
            if any { Some(total) } else { None }
        };
        QueueState {
            running: inner.running.clone(),
            pending: inner.pending.clone(),
            total_eta_seconds,
        }
    }

    fn emit_locked(inner: &Inner) {
        let state = Self::build_state(inner);
        for listener in &inner.on_change {
            listener(state.clone());
        }
    }
}

impl Default for JobQueue {
    fn default() -> Self {
        Self::new()
    }
}
