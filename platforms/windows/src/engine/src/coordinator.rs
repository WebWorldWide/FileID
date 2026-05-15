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
    /// V14.9-Y: sticky flag set the moment any worker detects a
    /// `DXGI_ERROR_DEVICE_REMOVED` from ORT/DirectML. Distinct from
    /// `cancelled` because the cause is fatal (the GPU is gone for the
    /// rest of the process) and the IPC layer emits a different error
    /// kind. Workers MUST observe this and stop submitting GPU work
    /// immediately — retrying spams the dead driver and prevents
    /// Windows TDR from recovering the device, which on a typical
    /// machine wedges the entire desktop until a hard reboot.
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

    /// V14.9-Y: returns true once any worker has marked the GPU as
    /// device-removed. Workers should treat this as a hard stop —
    /// don't submit any more session.run calls.
    pub fn is_gpu_dead(&self) -> bool {
        self.inner.gpu_dead.load(Ordering::Relaxed)
    }

    /// V14.9-Y: latch the GPU-dead flag and also flip `cancelled` so
    /// every existing worker checkpoint exits at the next poll. Returns
    /// true on the FIRST caller (lets the caller emit the IPC event
    /// exactly once across N workers racing on the same event).
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
