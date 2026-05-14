// ScanCoordinator — pause/resume/cancel state for the scan pipeline.
//
// Mirror of macOS engine/Sources/FileIDEngine/ScanCoordinator.swift. The
// scan pipeline runs as Discovery → bounded channel → N tagging workers
// → bounded channel → DBWriter. Each stage checks the coordinator's
// AtomicBool sync mirrors on hot paths so cancellation lands within
// milliseconds, no actor-hop tax per file.
//
// `request_pause` / `request_resume` / `request_cancel` are idempotent and
// safe from any thread; the workers poll the flags between batches.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Clone)]
pub struct ScanCoordinator {
    inner: Arc<Inner>,
}

struct Inner {
    paused: AtomicBool,
    cancelled: AtomicBool,
    /// Workers that hit the pause flag await on this notifier. Resume
    /// `notify_waiters()` wakes everyone at once.
    resume_notify: Notify,
}

impl ScanCoordinator {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                paused: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                resume_notify: Notify::new(),
            }),
        }
    }

    /// Cheap, can be polled inside hot loops on the worker thread.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Relaxed)
    }

    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::Relaxed)
    }

    pub fn request_pause(&self) {
        self.inner.paused.store(true, Ordering::Relaxed);
    }

    pub fn request_resume(&self) {
        self.inner.paused.store(false, Ordering::Relaxed);
        self.inner.resume_notify.notify_waiters();
    }

    pub fn request_cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Relaxed);
        // Wake any pause-waiting worker so they observe the cancel and exit.
        self.inner.resume_notify.notify_waiters();
    }

    /// Reset for a new scan session. Must only be called when no tasks are
    /// observing the coordinator.
    #[allow(dead_code)]
    pub fn reset(&self) {
        self.inner.paused.store(false, Ordering::Relaxed);
        self.inner.cancelled.store(false, Ordering::Relaxed);
    }

    /// Workers call this between batches; if paused, awaits resume. Returns
    /// `Err(())` if cancelled — the caller drops out of its loop.
    pub async fn check(&self) -> Result<(), ()> {
        if self.is_cancelled() {
            return Err(());
        }
        while self.is_paused() {
            self.inner.resume_notify.notified().await;
            if self.is_cancelled() {
                return Err(());
            }
        }
        Ok(())
    }
}

impl Default for ScanCoordinator {
    fn default() -> Self {
        Self::new()
    }
}
