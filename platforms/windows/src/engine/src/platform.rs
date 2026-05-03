//! Windows platform helpers — parent-PID watchdog, worker count heuristic,
//! physical memory probe, sleep guard.
//!
//! The non-windows builds (Linux Phase 5) get stub implementations so the
//! engine compiles on every host while the Windows-specific surfaces stay
//! gated behind `#[cfg(windows)]`.

use std::sync::Arc;
use tokio::sync::Notify;

/// Number of tagging workers to spin up by default. Mirrors the macOS
/// heuristic (14 on M1 Pro = ~physical cores * 1.7).
pub fn default_worker_cap() -> u32 {
    let physical = num_cpus::get_physical().max(1);
    let cap = (physical as f64 * 1.7).round() as u32;
    cap.clamp(2, 32)
}

/// Physical memory in GiB.
pub fn physical_memory_gb() -> f64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let bytes = sys.total_memory(); // bytes since sysinfo 0.30
    (bytes as f64) / (1024.0 * 1024.0 * 1024.0)
}

/// Get the parent-process PID via OS-specific API. None if unknown.
#[cfg(windows)]
pub fn get_parent_pid() -> Option<u32> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let our_pid = std::process::id();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut entry).is_err() {
            return None;
        }
        loop {
            if entry.th32ProcessID == our_pid {
                return Some(entry.th32ParentProcessID);
            }
            if Process32NextW(snap, &mut entry).is_err() {
                return None;
            }
        }
    }
}

#[cfg(not(windows))]
pub fn get_parent_pid() -> Option<u32> {
    // POSIX: getppid() is always available.
    #[cfg(unix)]
    unsafe {
        Some(libc::getppid() as u32)
    }
    #[cfg(not(unix))]
    None
}

/// Watch the parent process. Set the shutdown notify when the parent goes
/// away (handle invalid / process ID no longer alive).
#[cfg(windows)]
pub async fn watch_parent(parent_pid: u32, shutdown: Arc<Notify>) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    let handle: HANDLE = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid) } {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            tracing::warn!("could not open parent handle (pid={parent_pid}); watchdog disabled");
            return;
        }
    };

    // HANDLE wraps *mut c_void which the windows crate doesn't mark Send.
    // Smuggle it across the closure boundary as a usize — integers are
    // always Send. SAFETY: a Win32 HANDLE is opaque; CloseHandle and
    // WaitForSingleObject can be called from any thread once the handle
    // exists. This task is the sole owner of the value across the move.
    let handle_addr = handle.0 as usize;

    // Spawn a blocking task that waits on the handle. WaitForSingleObject
    // returns WAIT_OBJECT_0 when the parent exits.
    let result = tokio::task::spawn_blocking(move || {
        let h = HANDLE(handle_addr as *mut core::ffi::c_void);
        let r = unsafe { WaitForSingleObject(h, u32::MAX) };
        unsafe { let _ = CloseHandle(h); }
        r
    })
    .await;

    if result.is_ok() {
        tracing::info!("parent process exited; signaling shutdown");
        shutdown.notify_waiters();
    } else {
        tracing::warn!("parent watchdog task aborted; falling back to stdin EOF detection");
    }
}

#[cfg(not(windows))]
pub async fn watch_parent(parent_pid: u32, shutdown: Arc<Notify>) {
    // POSIX fallback: poll getppid every 5 s; if it changes (i.e. our parent
    // was reaped and we got reparented to init) trigger shutdown.
    let _ = parent_pid;
    let _ = shutdown;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        // Stub: real implementation lands in Phase 5 alongside Linux work.
    }
}

// ─── Sleep guard ────────────────────────────────────────────────────────────
//
// SetThreadExecutionState equivalent of macOS IOPMAssertion. Set on scan
// start, cleared on scan end. The OS keeps the system awake but lets the
// display sleep — we pass ES_SYSTEM_REQUIRED but NOT ES_DISPLAY_REQUIRED.

/// RAII guard. While alive, prevents Windows from sleeping the system.
/// Drop = release the assertion. Multiple guards stack (each thread sets
/// independently); this is by Win32 design.
pub struct SleepGuard {
    #[cfg(windows)]
    set: bool,
}

impl SleepGuard {
    #[cfg(windows)]
    pub fn acquire() -> Self {
        use windows::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
        };
        let prev = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
        Self { set: prev.0 != 0 }
    }

    #[cfg(not(windows))]
    pub fn acquire() -> Self { Self {} }
}

#[cfg(windows)]
impl Drop for SleepGuard {
    fn drop(&mut self) {
        if self.set {
            use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};
            unsafe { SetThreadExecutionState(ES_CONTINUOUS); }
        }
    }
}

// ─── Process priority ───────────────────────────────────────────────────────
//
// During scans the engine bumps to ABOVE_NORMAL so the OS scheduler
// preempts background services (Defender, OneDrive sync, Windows
// Search) and our worker pool stays saturated. Restored to NORMAL on
// scan end so the user's foreground apps (Zoom, browser) don't stutter.
//
// We deliberately stay BELOW HIGH_PRIORITY_CLASS — that level is for
// real-time workloads and starves the rest of the system. ABOVE_NORMAL
// is enough to win against Defender + sync clients without being
// antisocial.

/// RAII guard. While alive, the engine process runs at ABOVE_NORMAL
/// priority. Drop = restore NORMAL.
pub struct PriorityBoost {
    #[cfg(windows)]
    boosted: bool,
}

impl PriorityBoost {
    #[cfg(windows)]
    pub fn acquire() -> Self {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS,
        };
        let ok = unsafe { SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS) };
        Self { boosted: ok.is_ok() }
    }

    #[cfg(not(windows))]
    pub fn acquire() -> Self { Self {} }
}

#[cfg(windows)]
impl Drop for PriorityBoost {
    fn drop(&mut self) {
        if self.boosted {
            use windows::Win32::System::Threading::{
                GetCurrentProcess, SetPriorityClass, NORMAL_PRIORITY_CLASS,
            };
            unsafe { let _ = SetPriorityClass(GetCurrentProcess(), NORMAL_PRIORITY_CLASS); }
        }
    }
}
