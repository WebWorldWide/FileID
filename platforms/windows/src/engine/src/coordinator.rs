// ScanCoordinator — pause/resume/cancel state for the scan pipeline.
//
// Discovery → bounded channel → N tagging workers → bounded channel →
// DBWriter. Each stage checks the coordinator's AtomicBool sync mirrors
// on hot paths so cancellation lands within milliseconds, no actor-hop
// tax per file.
//
// `request_pause` / `request_resume` / `request_cancel` are idempotent and
// safe from any thread; the workers poll the flags between batches.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::Notify;

#[derive(Default)]
struct GpuFailureLatch {
    dead: AtomicBool,
    changed: Notify,
}

impl GpuFailureLatch {
    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }

    fn mark_dead(&self) -> bool {
        let first = !self.dead.swap(true, Ordering::AcqRel);
        self.changed.notify_waiters();
        first
    }

    async fn wait_dead(&self) {
        loop {
            // Arm the Notified future (register as a waiter) BEFORE re-checking
            // the flag. `Notify::notify_waiters()` only wakes ALREADY-registered
            // waiters and stores no permit, so an unenabled future misses a
            // notify that lands between construction and the first poll — a lost
            // wakeup that would block wait_dead forever while the GPU is already
            // gone. enable() closes that window: a notify after enable() but
            // before await is captured and the await returns immediately.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_dead() {
                return;
            }
            notified.await;
        }
    }
}

fn process_gpu_failure_latch() -> Arc<GpuFailureLatch> {
    static LATCH: OnceLock<Arc<GpuFailureLatch>> = OnceLock::new();
    LATCH
        .get_or_init(|| Arc::new(GpuFailureLatch::default()))
        .clone()
}

pub(crate) fn process_gpu_device_removed() -> bool {
    process_gpu_failure_latch().is_dead()
}

pub(crate) fn latch_process_gpu_device_removed() -> bool {
    process_gpu_failure_latch().mark_dead()
}

pub(crate) async fn wait_for_process_gpu_device_removed() {
    process_gpu_failure_latch().wait_dead().await;
}

pub(crate) const GPU_DEVICE_REMOVED_MESSAGE: &str = "The active GPU device was removed or reset. FileID stopped GPU work to keep the system responsive. Restart the engine before scanning or running AI features again; if this repeats, switch to the CPU execution provider or reduce model concurrency.";

#[derive(Clone)]
pub struct ScanCoordinator {
    inner: Arc<Inner>,
}

struct Inner {
    paused: AtomicBool,
    cancelled: AtomicBool,
    gpu_failure_latch: Arc<GpuFailureLatch>,
    gpu_dead_observed: AtomicBool,
    /// Workers that hit the pause flag await on this notifier. Resume
    /// `notify_waiters()` wakes everyone at once.
    resume_notify: Notify,
}

impl ScanCoordinator {
    pub fn new() -> Self {
        Self::with_gpu_failure_latch(process_gpu_failure_latch())
    }

    fn with_gpu_failure_latch(gpu_failure_latch: Arc<GpuFailureLatch>) -> Self {
        Self {
            inner: Arc::new(Inner {
                paused: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                gpu_failure_latch,
                gpu_dead_observed: AtomicBool::new(false),
                resume_notify: Notify::new(),
            }),
        }
    }

    /// Returns true once any worker has marked the GPU as device-removed.
    /// Workers should treat this as a hard stop — no more session.run.
    pub fn is_gpu_dead(&self) -> bool {
        self.inner.gpu_failure_latch.is_dead()
    }

    /// Latch process-wide GPU failure and wake this scan's workers. Returns true
    /// once per coordinator so racing workers emit one diagnostic.
    pub fn mark_gpu_dead(&self) -> bool {
        self.inner.gpu_failure_latch.mark_dead();
        let first_for_scan = !self.inner.gpu_dead_observed.swap(true, Ordering::AcqRel);
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.resume_notify.notify_waiters();
        first_for_scan
    }

    /// Cheap, can be polled inside hot loops on the worker thread.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Relaxed) || self.is_gpu_dead()
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
        self.inner.resume_notify.notify_waiters();
    }

    pub async fn wait_cancelled(&self) {
        loop {
            let notified = self.inner.resume_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// Workers call this between batches; if paused, awaits resume. Returns
    /// `Err(())` if cancelled — the caller drops out of its loop.
    pub async fn check(&self) -> Result<(), ()> {
        if self.is_cancelled() {
            return Err(());
        }
        let resumed = self.inner.resume_notify.notified();
        let gpu_failed = self.inner.gpu_failure_latch.changed.notified();
        tokio::pin!(resumed);
        tokio::pin!(gpu_failed);
        resumed.as_mut().enable();
        gpu_failed.as_mut().enable();
        while self.is_paused() {
            if self.is_cancelled() {
                return Err(());
            }
            tokio::select! {
                _ = resumed.as_mut() => {
                    resumed.set(self.inner.resume_notify.notified());
                    resumed.as_mut().enable();
                }
                _ = gpu_failed.as_mut() => return Err(()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_failure_is_shared_across_scan_coordinators() {
        let latch = Arc::new(GpuFailureLatch::default());
        let first = ScanCoordinator::with_gpu_failure_latch(latch.clone());
        let next = ScanCoordinator::with_gpu_failure_latch(latch);

        assert!(!first.is_gpu_dead());
        assert!(!next.is_gpu_dead());
        assert!(first.mark_gpu_dead());
        assert!(first.is_cancelled());
        assert!(next.is_gpu_dead());
        assert!(next.is_cancelled());
    }

    #[test]
    fn gpu_failure_diagnostic_is_once_per_scan() {
        let coordinator =
            ScanCoordinator::with_gpu_failure_latch(Arc::new(GpuFailureLatch::default()));

        assert!(coordinator.mark_gpu_dead());
        assert!(!coordinator.mark_gpu_dead());
    }

    #[tokio::test]
    async fn cancellation_wakes_receivers_waiting_for_work() {
        let coordinator =
            ScanCoordinator::with_gpu_failure_latch(Arc::new(GpuFailureLatch::default()));
        let waiting = tokio::spawn({
            let coordinator = coordinator.clone();
            async move { coordinator.wait_cancelled().await }
        });
        tokio::task::yield_now().await;

        coordinator.request_cancel();

        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("cancel waiter must wake")
            .expect("cancel waiter must not panic");
    }

    #[tokio::test]
    async fn shared_gpu_failure_wakes_a_paused_scan() {
        let latch = Arc::new(GpuFailureLatch::default());
        let paused = ScanCoordinator::with_gpu_failure_latch(latch.clone());
        let reporter = ScanCoordinator::with_gpu_failure_latch(latch);
        paused.request_pause();
        let waiting = tokio::spawn({
            let paused = paused.clone();
            async move { paused.check().await }
        });
        tokio::task::yield_now().await;

        reporter.mark_gpu_dead();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("paused worker must wake")
            .expect("check task must not panic");
        assert!(outcome.is_err());
    }
}
