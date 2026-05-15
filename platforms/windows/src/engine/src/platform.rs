//! Windows platform helpers — parent-PID watchdog, worker count heuristic,
//! physical memory probe, sleep guard.
//!
//! The non-windows builds (Linux Phase 5) get stub implementations so the
//! engine compiles on every host while the Windows-specific surfaces stay
//! gated behind `#[cfg(windows)]`.

use std::path::Path;
use std::sync::Arc;
use tokio::sync::Notify;

/// Redact a user path for logs: keep last two components; pass
/// app-structural paths verbatim. Mirrors PathRedaction.swift.
pub fn redact_path_for_log(path: impl AsRef<Path>) -> String {
    let s = path.as_ref().to_string_lossy().to_string();
    let s_lower = s.to_lowercase();
    // App-structural paths: pass through.
    if s_lower.contains("\\fileid\\")
        || s_lower.contains("/fileid/")
        || s_lower.contains("appdata\\local\\fileid")
    {
        return s;
    }
    let parts: Vec<&str> = path
        .as_ref()
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    if parts.is_empty() {
        return "…".to_string();
    }
    let tail = parts
        .iter()
        .rev()
        .take(2)
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("/");
    if tail.is_empty() { "…".to_string() } else { format!("…/{tail}") }
}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn redacts_deep_user_path() {
        let r = redact_path_for_log(r"C:\Users\Adam\Pictures\Vacation\IMG.jpg");
        assert!(r.starts_with("…/"));
        assert!(r.ends_with("Vacation/IMG.jpg"));
        assert!(!r.contains("Adam"));
    }

    #[test]
    fn passes_through_app_structural_path() {
        let s = r"C:\Users\Adam\AppData\Local\FileID\Models\arcface\weights.onnx";
        assert_eq!(redact_path_for_log(s), s);
    }

    #[test]
    fn handles_empty_input() {
        assert_eq!(redact_path_for_log(""), "…");
    }
}

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
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let our_pid = std::process::id();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        // Wrap the snapshot walk in a closure so every exit (early return,
        // loop break) falls through to the CloseHandle below.
        let result = (|| -> Option<u32> {
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
        })();
        let _ = CloseHandle(snap);
        result
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

    let wait = tokio::task::spawn_blocking(move || {
        let h = HANDLE(handle_addr as *mut core::ffi::c_void);
        let r = unsafe { WaitForSingleObject(h, u32::MAX) };
        unsafe { let _ = CloseHandle(h); }
        r
    });

    // Race the blocking wait against the shared shutdown signal so EOF /
    // explicit Shutdown / etc. can unblock us when the parent stays alive
    // (e.g. test harnesses, debugger sessions). If we lose the race, the
    // blocking thread keeps its INFINITE wait until process exit; the OS
    // reaps it. shutdown_timeout(0) on the Runtime ensures the leaked
    // blocking task does NOT delay process exit.
    tokio::select! {
        _ = shutdown.notified() => {
            tracing::info!("watch_parent cancelled by shutdown signal");
        }
        result = wait => {
            if result.is_ok() {
                tracing::info!("parent process exited; signaling shutdown");
                shutdown.notify_waiters();
            } else {
                tracing::warn!("parent watchdog task aborted; falling back to stdin EOF detection");
            }
        }
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
// V14.9-Y: was ABOVE_NORMAL, now NORMAL. The previous bump fought DWM
// for CPU during scans, and when DirectML hit TDR (GPU device removed)
// our high-priority workers spammed retries against the dying driver
// faster than DWM could recover the GPU → full system hang. Staying at
// NORMAL lets the desktop compositor win under contention while still
// using all our worker threads.
//
// Override via FILEID_PROCESS_PRIORITY=above_normal if a user explicitly
// wants the engine to outrank background services (Defender, OneDrive).

/// RAII guard. While alive, the engine process priority is set per the
/// FILEID_PROCESS_PRIORITY env var (default NORMAL). Drop = restore NORMAL.
pub struct PriorityBoost {
    #[cfg(windows)]
    boosted: bool,
}

impl PriorityBoost {
    #[cfg(windows)]
    pub fn acquire() -> Self {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS,
            BELOW_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_CREATION_FLAGS,
        };
        let pri: PROCESS_CREATION_FLAGS = match std::env::var("FILEID_PROCESS_PRIORITY")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("above_normal") => ABOVE_NORMAL_PRIORITY_CLASS,
            Some("below_normal") => BELOW_NORMAL_PRIORITY_CLASS,
            _ => NORMAL_PRIORITY_CLASS,
        };
        let ok = unsafe { SetPriorityClass(GetCurrentProcess(), pri) };
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

// ─── Performance-Pack DLL search ────────────────────────────────────────────
//
// SEC-3 locked the DLL search path to System32 + the engine binary's
// directory only. Performance Packs extract into
// %LOCALAPPDATA%\FileID\Models\packs\<vendor>\ — outside both. Without
// teaching the loader about those dirs, an installed CUDA / OpenVINO / QNN
// pack stays invisible to ORT and the EP probe falls back to DirectML or
// CPU. AddDllDirectory adds a single trusted dir to the per-process search
// list; we walk one level deep so DLLs at the root OR in a flat `bin/`
// subdir of the pack are both reachable.

/// Register every directory under `root` (up to MAX_DEPTH levels deep)
/// that contains at least one .dll as an additional DLL search path.
/// Idempotent: called every time a pack is extracted; the loader dedupes
/// internally. Returns the list of directories that exist + have DLLs but
/// for which `AddDllDirectory` returned null — callers (notably main.rs's
/// CUDA toolkit hookup) surface these to the app as a
/// `cuda_dll_registration_failed` engine error so a missing CUDA EP
/// becomes diagnosable instead of silently falling back to DirectML.
///
/// V14.9-U: depth extended from 1 to MAX_DEPTH so the auto-fetched cuDNN
/// pack (which extracts to `<root>/<versioned-archive>/bin/*.dll`) gets
/// picked up. Existing single-level callers (llama.cpp, packs/<vendor>)
/// keep working — only dirs that actually contain DLLs are registered.
#[cfg(windows)]
pub fn register_dll_dirs_under(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    use windows::core::PCWSTR;
    use windows::Win32::System::LibraryLoader::AddDllDirectory;

    const MAX_DEPTH: usize = 4;

    fn dir_has_dll(p: &std::path::Path) -> bool {
        let Ok(rd) = std::fs::read_dir(p) else { return false; };
        for e in rd.flatten() {
            if e.path().extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("dll")).unwrap_or(false) {
                return true;
            }
        }
        false
    }

    fn add(dir: &std::path::Path) -> bool {
        let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
        wide.push(0);
        let cookie = unsafe { AddDllDirectory(PCWSTR(wide.as_ptr())) };
        if cookie.is_null() {
            tracing::warn!(dir = %dir.display(), "AddDllDirectory returned null");
            false
        } else {
            tracing::info!(dir = %dir.display(), "[EP] AddDllDirectory registered pack dir");
            true
        }
    }

    fn walk(dir: &std::path::Path, depth: usize, failed: &mut Vec<std::path::PathBuf>) {
        if !dir.is_dir() { return; }
        if dir_has_dll(dir) && !add(dir) {
            failed.push(dir.to_path_buf());
        }
        if depth == 0 { return; }
        let Ok(rd) = std::fs::read_dir(dir) else { return; };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, depth - 1, failed);
            }
        }
    }

    use std::os::windows::ffi::OsStrExt;

    let mut failed: Vec<std::path::PathBuf> = Vec::new();
    walk(root, MAX_DEPTH, &mut failed);
    failed
}

#[cfg(not(windows))]
pub fn register_dll_dirs_under(_root: &std::path::Path) -> Vec<std::path::PathBuf> {
    Vec::new()
}

/// Returns the primary (largest-VRAM) non-software adapter's
/// `DedicatedVideoMemory` in MB. None if DXGI enumeration fails or no
/// physical adapter is present. Used by V14.9-X to gate the ML Session
/// pool — without this, a V14.9-W-style multi-Session config can exhaust
/// a 6 GB card's VRAM and wedge the DirectML driver (full system hang).
///
/// Cheap to call (~1 ms); the DXGI factory + adapter enumeration is the
/// same primitive `models::runtime::probe_gpu_vendor` uses for vendor
/// detection. Safe to call from any thread.
#[cfg(windows)]
pub fn dedicated_vram_mb() -> Option<u64> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG,
        DXGI_ADAPTER_FLAG_SOFTWARE,
    };

    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    let mut idx: u32 = 0;
    let mut best: Option<u64> = None;
    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(idx) } {
            Ok(a) => a,
            Err(_) => break,
        };
        let desc = match unsafe { adapter.GetDesc1() } {
            Ok(d) => d,
            Err(_) => {
                idx += 1;
                continue;
            }
        };
        let flags = DXGI_ADAPTER_FLAG(desc.Flags as i32);
        let is_software = (flags.0 & DXGI_ADAPTER_FLAG_SOFTWARE.0) != 0;
        if !is_software {
            let mb = (desc.DedicatedVideoMemory as u64) / (1024 * 1024);
            best = Some(best.map_or(mb, |prev| prev.max(mb)));
        }
        idx += 1;
    }
    best
}

#[cfg(not(windows))]
pub fn dedicated_vram_mb() -> Option<u64> {
    None
}

/// V15.0 Phase H: lower per-thread I/O priority to "VeryLow" so the
/// tagging workers' bulk JPEG/PNG reads don't compete with foreground
/// apps (File Explorer browsing, video playback). Windows kernel I/O
/// scheduler honors thread-level priority hints for filesystem reads.
///
/// `THREAD_INFORMATION_CLASS::ThreadPowerThrottling` is the modern API
/// but it's complex. The simpler `THREAD_PRIORITY_LOWEST` via
/// `SetThreadPriority` works on every Windows version and has the same
/// effect on I/O scheduling on Win11. Best-effort: failures are
/// silently ignored.
#[cfg(windows)]
pub fn set_worker_background_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST,
    };
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) };
}

#[cfg(not(windows))]
pub fn set_worker_background_priority() {}
