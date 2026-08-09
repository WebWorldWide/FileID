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
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Clone)]
pub struct ScanCoordinator {
    inner: Arc<Inner>,
}

struct Inner {
    paused: AtomicBool,
    cancelled: AtomicBool,
    /// Sticky flag set when any worker detects `DXGI_ERROR_DEVICE_REMOVED`
    /// from ORT/DirectML. Distinct from `cancelled` because the cause is
    /// fatal (GPU is gone for the rest of the process) and the IPC layer
    /// emits a different error kind. Workers MUST stop submitting GPU work
    /// — retrying spams the dead driver and prevents TDR recovery, which
    /// on a typical machine wedges the desktop until a hard reboot.
    gpu_dead: AtomicBool,
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
                gpu_dead: AtomicBool::new(false),
                resume_notify: Notify::new(),
            }),
        }
    }

    /// Returns true once any worker has marked the GPU as device-removed.
    /// Workers should treat this as a hard stop — no more session.run.
    pub fn is_gpu_dead(&self) -> bool {
        self.inner.gpu_dead.load(Ordering::Relaxed)
    }

    /// Latch the GPU-dead flag and flip `cancelled` so every worker
    /// checkpoint exits at the next poll. Returns true on the FIRST caller
    /// so the IPC event fires exactly once across N racing workers.
    pub fn mark_gpu_dead(&self) -> bool {
        let was = self.inner.gpu_dead.swap(true, Ordering::AcqRel);
        if !was {
            self.inner.cancelled.store(true, Ordering::Relaxed);
            self.inner.resume_notify.notify_waiters();
        }
        !was
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
    /// NOTE: `gpu_dead` is intentionally NOT reset. Once the GPU device
    /// has been removed in this process, every subsequent ORT session is
    /// invalid — a full engine restart is required to recover. Resetting
    /// would lure us back into the cascade-spam failure mode.
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
        // Register the waiter BEFORE reading `paused`. tokio's
        // notify_waiters() (used by request_resume/cancel/gpu_dead) wakes only
        // already-registered waiters and stores NO permit, so a resume that
        // fired between an is_paused()==true read and a plain notified().await
        // was lost — the worker parked forever, the Tagging→DBWriter channel
        // never closed, and the whole scan wedged. enable() inserts this future
        // into the waiter list up front, so any notify in the race window marks
        // it notified and the await returns immediately. Mirrors main.rs.
        let notified = self.inner.resume_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        while self.is_paused() {
            notified.as_mut().await;
            // Re-arm for the next loop iteration.
            notified.set(self.inner.resume_notify.notified());
            notified.as_mut().enable();
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
