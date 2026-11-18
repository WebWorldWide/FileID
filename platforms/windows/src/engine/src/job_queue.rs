// Single-FIFO queue tracking the engine's background work (Scan, FaceCluster, DeepAnalyze); one job runs at a time.
// Each push/pop emits a `queueState` IPC event so the app's sidebar stays in sync. Runners push before they start and
// pop when done (main.rs dispatcher); the queue itself runs nothing.

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

    pub fn on_change<F>(&self, listener: F)
    where
        F: Fn(QueueState) + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock();
        inner.on_change.push(Box::new(listener));
    }

    pub fn clear_listeners(&self) {
        self.inner.lock().on_change.clear();
    }

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

    pub fn has_active_besides(&self, category: JobCategory) -> bool {
        let inner = self.inner.lock();
        inner.running.iter().chain(inner.pending.iter()).any(|job| job.category != category)
    }

    /// RAII guards call this on Drop so a panicking task can't wedge the sidebar queue. Cross-category jobs can
    /// genuinely overlap on this engine; the single-running model is a sidebar approximation matching macOS's
    /// serialized JobQueue and always converges as jobs finish.
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

    fn build_state(inner: &Inner) -> QueueState {
        let total_eta_seconds = {
            let mut total = 0.0;
            let mut any = false;
            if let Some(r) = &inner.running
                && let Some(e) = r.eta_seconds {
                    total += e;
                    any = true;
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
