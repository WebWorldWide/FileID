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

    /// Sends every queue change to the app without ever dropping the last one: a full channel makes the forwarder wait, and
    /// bursts of changes collapse into the newest snapshot (each snapshot is the whole queue).
    pub fn forward_to(&self, sink: crate::ipc::sink::Sink) {
        let (latest, mut changes) = tokio::sync::watch::channel(None::<QueueState>);
        self.on_change(move |state| {
            latest.send_replace(Some(state));
        });
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let Some(state) = changes.borrow_and_update().clone() else { continue };
                sink.send(crate::ipc::IpcEvent::now(crate::ipc::EventPayload::QueueState(crate::ipc::Wrap::new(state)))).await;
            }
        });
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

    /// Renames the job and refreshes its remaining time, emitting `queueState`; nothing happens once it has finished.
    pub fn retitle(&self, id: &str, title: impl Into<String>, eta_seconds: Option<f64>) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let Some(job) = inner.running.iter_mut().chain(inner.pending.iter_mut()).find(|job| job.id == id) else { return };
        job.title = title.into();
        job.eta_seconds = eta_seconds;
        Self::emit_locked(inner);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retitle_changes_the_running_job_and_announces_it() {
        let queue = JobQueue::new();
        let seen = Arc::new(Mutex::new(Vec::<QueueState>::new()));
        let sink = seen.clone();
        queue.on_change(move |state| sink.lock().push(state));
        let id = queue.push(JobCategory::FaceCluster, "Group faces", None);
        queue.promote_next();
        queue.retitle(&id, "Finding faces… 5 of 10", Some(3.0));
        let last = seen.lock().last().cloned().unwrap();
        let running = last.running.unwrap();
        assert_eq!((running.title.as_str(), running.eta_seconds, last.total_eta_seconds), ("Finding faces… 5 of 10", Some(3.0), Some(3.0)));
        let announced = seen.lock().len();
        queue.finish(&id);
        let after_finish = seen.lock().len();
        queue.retitle(&id, "late", None);
        assert_eq!(seen.lock().len(), after_finish, "a finished job is not retitled or announced");
        assert!(after_finish > announced);
    }

    #[tokio::test]
    async fn the_final_queue_state_survives_a_full_event_channel() {
        use crate::ipc::{EventPayload, IpcEvent};
        let queue = JobQueue::new();
        let (sink, mut events) = crate::ipc::sink::Sink::channel_for_test(1);
        queue.forward_to(sink.clone());
        sink.try_send(IpcEvent::now(EventPayload::Log(crate::ipc::Wrap::new(crate::ipc::LogLine { level: crate::ipc::LogLevel::Info, message: "filler".into() })))).unwrap();
        let id = queue.push(JobCategory::Scan, "Scan", None);
        queue.promote_next();
        queue.finish(&id);
        let mut last = None;
        while let Ok(Some(event)) = tokio::time::timeout(std::time::Duration::from_millis(300), events.recv()).await {
            if let EventPayload::QueueState(state) = event.payload {
                last = Some(state.inner);
            }
        }
        let last = last.expect("a queue state arrived once the channel had room");
        assert!(last.running.is_none() && last.pending.is_empty());
    }
}
