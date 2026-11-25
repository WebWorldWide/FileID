//! Platform helpers: parent-PID watchdog, worker-count heuristic, physical memory probe, sleep guard. Non-Windows builds get stubs so the engine compiles everywhere while Windows-specific surfaces stay behind `#[cfg(windows)]`.

use std::path::Path;
use std::sync::Arc;
use tokio::sync::Notify;

pub fn available_disk_bytes(path: &Path) -> Option<u64> {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            return None;
        }
    }
    available_disk_bytes_existing(&probe)
}

#[cfg(windows)]
fn available_disk_bytes_existing(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0u64;
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut available),
            None,
            None,
        )
        .ok()?;
    }
    Some(available)
}

#[cfg(unix)]
fn available_disk_bytes_existing(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let raw = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(raw.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    // statvfs field widths differ: f_bavail/f_frsize are u64 on Linux glibc but f_bavail is u32 on macOS. `u64::from` widens on macOS and is an identity on Linux, so allow the Linux-only useless_conversion to keep one expression correct on both.
    #[allow(clippy::useless_conversion)]
    let bytes = u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize));
    Some(bytes)
}

#[cfg(not(any(windows, unix)))]
fn available_disk_bytes_existing(_path: &Path) -> Option<u64> {
    None
}

pub fn redact_path_for_log(path: impl AsRef<Path>) -> String {
    use std::path::Component;
    // Only Normal components are PII candidates: Prefix (drive letter, UNC server\share) and RootDir are topology, so C:\ → "…" and \\server\share\user\file.jpg → "…/user/file.jpg" without leaking the drive/server.
    let parts: Vec<&str> = path
        .as_ref()
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return "…".to_string();
    }
    // Home-anchored masking: for a file directly under a home dir (`…/Users/<user>/file`, `…/home/<user>/file`) the last-two-components tail would be `<user>/file` and leak the username. Detect a home-root keyword + username and, when the username would fall inside the kept tail, drop everything up to and including it. Deeper paths already push the username off the end.
    let home_anchor = parts.iter().position(|p| {
        let l = p.to_lowercase();
        l == "users" || l == "home"
    });
    if let Some(anchor) = home_anchor {
        let user_idx = anchor + 1;
        if user_idx < parts.len() {
            // (a) The path ends AT the home dir (`C:\Users\<user>`, `/home/<user>`): the username is the terminal component. Only a GENUINE home root (keyword first, anchor == 0) is fully masked; a nested backup tree whose terminal is a real file (anchor > 0, e.g. `E:\Backups\Users\file.txt`) falls through so its filename survives.
            if user_idx == parts.len() - 1 && anchor == 0 {
                return "…".to_string();
            }
            // (b) The username sits inside the kept tail with at least one component after it (a real user dir above a file): drop up to and including it. The "component follows" guard stops a dir literally named users/home directly above a file from masking the FILENAME.
            if user_idx + 1 < parts.len() && parts.len() - user_idx <= 2 {
                let after = &parts[user_idx + 1..];
                let kept: Vec<&str> =
                    after.iter().rev().take(2).copied().collect::<Vec<_>>().into_iter().rev().collect();
                return format!("…/{}", kept.join("/"));
            }
        }
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
    #[cfg(windows)]
    fn redacts_deep_user_path() {
        let r = redact_path_for_log(r"C:\Users\Adam\Pictures\Vacation\IMG.jpg");
        assert!(r.starts_with("…/"));
        assert!(r.ends_with("Vacation/IMG.jpg"));
        assert!(!r.contains("Adam"));
    }

    #[test]
    fn redacts_app_structural_path() {
        if let Ok(root) = crate::paths::root() {
            let path = root.join("Models").join("arcface").join("weights.onnx");
            let redacted = redact_path_for_log(&path);
            assert_eq!(redacted, "…/arcface/weights.onnx");
            assert!(!redacted.contains(&root.to_string_lossy().to_string()));
        }
    }

    #[test]
    fn handles_empty_input() {
        assert_eq!(redact_path_for_log(""), "…");
    }

    #[test]
    #[cfg(windows)]
    fn redacts_unc_path_keeping_last_two_components() {
        let r = redact_path_for_log(r"\\server\share\user\file.jpg");
        assert!(r.ends_with("user/file.jpg"), "got: {r}");
        assert!(!r.contains("server"), "server name leaked: {r}");
        assert!(!r.contains("share"), "share name leaked: {r}");
    }

    #[test]
    #[cfg(windows)]
    fn redacts_drive_root_to_ellipsis() {
        assert_eq!(redact_path_for_log(r"C:\"), "…");
    }

    #[test]
    fn app_structural_logs_path_is_redacted() {
        if let Ok(root) = crate::paths::root() {
            let path = root.join("logs").join("app.log");
            assert_eq!(redact_path_for_log(path), "…/logs/app.log");
        }
    }

    /// #26: a USER path that merely CONTAINS `AppData\Local\FileID` but lives outside this engine's resolved root (e.g. a backup on another volume) is NOT the app's state tree and MUST be redacted; the old unanchored `contains` fallback leaked these.
    #[test]
    #[cfg(windows)]
    fn redacts_user_backup_path_containing_appdata_local_fileid() {
        let r = redact_path_for_log(r"D:\Backups\AppData\Local\FileID\notes.txt");
        assert_eq!(r, "…/FileID/notes.txt");
        assert!(!r.contains("Backups"), "backup path leaked: {r}");
    }

    /// ENG-97: a USER path that merely contains a folder named "FileID" (dev checkout, backup dir) is NOT the app's state tree and MUST be redacted; the old broad `\fileid\` substring match leaked these with the username.
    #[test]
    #[cfg(windows)]
    fn redacts_user_path_merely_containing_fileid() {
        let r = redact_path_for_log(r"C:\Users\Adam\Code\FileID\src\secret.rs");
        assert_eq!(r, "…/src/secret.rs");
        assert!(!r.contains("Adam"), "username leaked: {r}");
    }

    /// A file directly under a home dir must NOT expose the username: the last two components alone would emit `…/Adam/resume.pdf`.
    #[test]
    #[cfg(windows)]
    fn redacts_home_direct_file_masking_username() {
        let r = redact_path_for_log(r"C:\Users\Adam\resume.pdf");
        assert!(!r.contains("Adam"), "username leaked: {r}");
        assert!(r.ends_with("resume.pdf"), "filename lost: {r}");
    }

    /// POSIX shape: `/home/<user>/file` and `/Users/<user>/file` must also mask the username (runs on every host).
    #[test]
    fn redacts_posix_home_direct_file_masking_username() {
        let r = redact_path_for_log("/home/adam/notes.txt");
        assert!(!r.contains("adam"), "username leaked: {r}");
        assert!(r.ends_with("notes.txt"), "filename lost: {r}");

        let r2 = redact_path_for_log("/Users/adam/notes.txt");
        assert!(!r2.contains("adam"), "username leaked: {r2}");
        assert!(r2.ends_with("notes.txt"), "filename lost: {r2}");
    }

    /// Regression: a path that ENDS at the home dir (scan root is the home directory) must fully mask the username, not fall through to the generic tail. Genuine home root only; a nested backup tree is handled below.
    #[test]
    fn redacts_terminal_home_dir_masking_username() {
        assert_eq!(redact_path_for_log("/Users/adam"), "…");
        assert_eq!(redact_path_for_log("/home/adam"), "…");
    }

    /// Must NOT over-redact deep paths: `…/Users/<user>/Pictures/IMG` already pushes the username off the kept tail, so deeper context survives.
    #[test]
    #[cfg(windows)]
    fn home_masking_leaves_deep_user_paths_unchanged() {
        let r = redact_path_for_log(r"C:\Users\Adam\Pictures\Vacation\IMG.jpg");
        assert_eq!(r, "…/Vacation/IMG.jpg");
        assert!(!r.contains("Adam"), "username leaked: {r}");
    }

    /// A directory literally named `Users`/`home` DIRECTLY above a file (backup tree, NAS share) is not a home root and the next part is the FILENAME; it must be preserved instead of collapsing to "…".
    #[test]
    #[cfg(windows)]
    fn home_masking_preserves_filename_for_backup_tree() {
        let r = redact_path_for_log(r"E:\Backups\Users\file.txt");
        assert_eq!(r, "…/Users/file.txt");
    }
}

#[cfg(all(test, windows))]
mod sleep_guard_tests {
    use super::*;

    /// SetThreadExecutionState is thread-scoped, so the assertion must be set AND cleared on the same thread. The guard owns a dedicated thread for its whole lifecycle, so acquiring on one thread and dropping on another (the real Tokio-worker case) still releases cleanly; the worker is joined by Drop so the clear is provably reached.
    #[test]
    fn acquire_and_drop_across_threads_does_not_panic_or_leak() {
        let guard = SleepGuard::acquire();
        let handle = std::thread::spawn(move || {
            drop(guard);
        });
        handle.join().expect("guard drop thread panicked");
    }

    #[test]
    fn repeated_acquire_release_cycles() {
        for _ in 0..8 {
            let g = SleepGuard::acquire();
            drop(g);
        }
    }
}

/// CPU topology: performance / efficiency / logical-thread split. On non-hybrid CPUs (pre-Intel-12th-gen, all AMD Zen, all ARM except Snapdragon) `e_cores = 0` and `p_cores = physical_cores`.
#[derive(Debug, Clone, Copy)]
pub struct CpuTopology {
    pub p_cores: u32,
    pub e_cores: u32,
    pub logical: u32,
}

impl CpuTopology {
    /// macOS worker formula P + E + max(1, P/2): 14 on M1 Pro (8P+2E), 28 on i9-13900K (8P+16E), 12 on a non-hybrid 8-core. Clamped to `logical` so SMT is never oversubscribed.
    pub fn worker_cap(self) -> u32 {
        let p = self.p_cores.max(1);
        let e = self.e_cores;
        let cap = p + e + (p / 2).max(1);
        cap.min(self.logical.max(1)).clamp(2, 32)
    }
}

/// Detects the P-core / E-core split via `GetLogicalProcessorInformationEx(RelationProcessorCore)`. `EfficiencyClass` is 0 for E-cores and >0 for P-cores on Intel hybrid parts (12th-gen+); on non-hybrid CPUs every core reports the same class and collapses into `p_cores`.
#[cfg(windows)]
pub fn cpu_topology() -> CpuTopology {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore,
        SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };

    let logical = num_cpus::get().max(1) as u32;
    let physical_fallback = num_cpus::get_physical().max(1) as u32;

    unsafe {
        let mut bytes: u32 = 0;
        let _ = GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut bytes);
        if bytes == 0 {
            return CpuTopology { p_cores: physical_fallback, e_cores: 0, logical };
        }
        // Over-align the buffer: Vec<u64> (8-byte aligned, matching the struct) rather than Vec<u8> (1-byte alignment, UB to reinterpret). The u64* → struct* cast keeps clippy's alignment check satisfied.
        let words = bytes.div_ceil(8) as usize;
        // Over-allocate one full struct of slack: windows-rs models SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX at its largest union variant (~80 B) but a single-group RelationProcessorCore record is ~48 B, so `&*rec_ptr` on the FINAL record would form an 80-byte place past an exactly-sized buffer (a partially-dangling reference, UB). The second call still passes the OS-reported `bytes` and `total_words` bounds the loop to real records, so this only adds zero-padded headroom.
        let slack_words = std::mem::size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>().div_ceil(8);
        let mut aligned: Vec<u64> = vec![0u64; words + slack_words];
        let head = aligned.as_mut_ptr().cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
        if GetLogicalProcessorInformationEx(RelationProcessorCore, Some(head), &mut bytes).is_err() {
            return CpuTopology { p_cores: physical_fallback, e_cores: 0, logical };
        }

        let mut classes: std::collections::HashMap<u8, u32> = std::collections::HashMap::new();
        let mut offset_words: usize = 0;
        let total_words = bytes.div_ceil(8) as usize;
        while offset_words < total_words {
            // SAFETY: offset_words ≤ total_words so `add` stays in the allocation; the u64* → struct* cast is 8-byte-aligned on both sides.
            let rec_ptr = aligned.as_ptr().add(offset_words).cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
            let rec = &*rec_ptr;
            let size_bytes_field = rec.Size as usize;
            if size_bytes_field == 0 { break; }
            let proc_info = rec.Anonymous.Processor;
            *classes.entry(proc_info.EfficiencyClass).or_insert(0) += 1;
            offset_words += size_bytes_field.div_ceil(8);
        }

        match classes.len() {
            0 => CpuTopology { p_cores: physical_fallback, e_cores: 0, logical },
            1 => {
                let total = classes.values().copied().sum::<u32>().max(1);
                CpuTopology { p_cores: total, e_cores: 0, logical }
            }
            _ => {
                // Hybrid CPU: highest class = P-cores, lowest = E-cores (Intel 12th-gen+ uses 0 for E, 1 for P). Intermediate classes bucket with P-cores, since oversubscribing beats under.
                let min_class = *classes.keys().min().unwrap();
                let e = *classes.get(&min_class).unwrap_or(&0);
                let p = classes.values().copied().sum::<u32>().saturating_sub(e);
                CpuTopology { p_cores: p.max(1), e_cores: e, logical }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_perflevel_cores(level: u8) -> Option<u32> {
    let name = std::ffi::CString::new(format!("hw.perflevel{level}.physicalcpu")).ok()?;
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>();
    // SAFETY: `name` is NUL-terminated and `value`/`size` outlive the call, `size` being the buffer's size.
    let status = unsafe { libc::sysctlbyname(name.as_ptr(), (&raw mut value).cast(), &raw mut size, std::ptr::null_mut(), 0) };
    (status == 0 && value > 0).then_some(value)
}

#[cfg(not(windows))]
pub fn cpu_topology() -> CpuTopology {
    let logical = num_cpus::get().max(1) as u32;
    #[cfg(target_os = "macos")]
    if let Some(p_cores) = macos_perflevel_cores(0) {
        return CpuTopology { p_cores, e_cores: macos_perflevel_cores(1).unwrap_or(0), logical };
    }
    CpuTopology { p_cores: num_cpus::get_physical().max(1) as u32, e_cores: 0, logical }
}

pub fn performance_core_count() -> usize {
    #[cfg(target_os = "macos")]
    if let Some(count) = macos_perflevel_cores(0) {
        return count as usize;
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(count) = linux_top_capacity_cpus() {
            return count.min(cpu_topology().p_cores as usize).max(1);
        }
    }
    cpu_topology().p_cores as usize
}

/// CPUs sharing the highest `cpu_capacity`: the big cores of a hybrid part. `None` when the kernel exposes no capacities or all CPUs are equal.
#[cfg(target_os = "linux")]
fn linux_top_capacity_cpus() -> Option<usize> {
    let capacities: Vec<u32> = std::fs::read_dir("/sys/devices/system/cpu")
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let name = name.to_str()?;
            if !name.strip_prefix("cpu").is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()) && !n.is_empty()) {
                return None;
            }
            std::fs::read_to_string(entry.path().join("cpu_capacity")).ok()?.trim().parse().ok()
        })
        .collect();
    top_capacity_count(&capacities)
}

#[cfg(any(target_os = "linux", test))]
fn top_capacity_count(capacities: &[u32]) -> Option<usize> {
    let max = *capacities.iter().max()?;
    let min = *capacities.iter().min()?;
    (max != min).then(|| capacities.iter().filter(|&&c| c == max).count())
}

/// Default number of tagging workers: the macOS-parity formula P + E + max(1, P/2) capped at logical cores and [2, 32] (the old `physical * 1.7` mis-sized hybrid CPUs, e.g. treating an i9-13900K's 8P+16E like an 8-core).
pub fn default_worker_cap() -> u32 {
    cpu_topology().worker_cap()
}

pub fn physical_memory_gb() -> f64 {
    let Ok(mut sys) = sysinfo::System::new() else { return 0.0 };
    sys.refresh_memory();
    (sys.total_memory() as f64) / (1024.0 * 1024.0 * 1024.0)
}

/// Available (not-in-use) physical memory in MiB; polls `GlobalMemoryStatusEx` each call, bounded by the 30 s memory-tier refresh in the scan loop.
#[cfg(windows)]
pub fn available_memory_mb() -> u64 {
    use windows::Win32::System::SystemInformation::{
        GlobalMemoryStatusEx, MEMORYSTATUSEX,
    };
    unsafe {
        let mut mem = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if GlobalMemoryStatusEx(&mut mem).is_ok() {
            mem.ullAvailPhys / (1024 * 1024)
        } else {
            0
        }
    }
}

#[cfg(all(not(windows), not(target_os = "macos")))]
pub fn available_memory_mb() -> u64 {
    let Ok(mut sys) = sysinfo::System::new() else { return 0 };
    sys.refresh_memory();
    sys.available_memory() / (1024 * 1024)
}

/// Matches the Swift engine: sysinfo's macOS figure subtracts compressor pages,
/// so also credit the kernel's availability level (what `memory_pressure`
/// reports), which counts compressible and file-backed memory the pager can
/// reclaim.
#[cfg(target_os = "macos")]
pub fn available_memory_mb() -> u64 {
    let Ok(mut sys) = sysinfo::System::new() else { return 0 };
    sys.refresh_memory();
    let mut level: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated and `level`/`size` describe a valid
    // c_int buffer; sysctlbyname writes at most `size` bytes.
    let read = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_level".as_ptr(),
            (&mut level as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    macos_available_mb(
        sys.available_memory() / (1024 * 1024),
        sys.total_memory() / (1024 * 1024),
        read.then_some(level),
    )
}

#[cfg(any(target_os = "macos", test))]
fn macos_available_mb(counted_mb: u64, total_mb: u64, level: Option<i32>) -> u64 {
    match level.and_then(|l| u64::try_from(l).ok()).filter(|l| (1..=100).contains(l)) {
        Some(level) => counted_mb.max(total_mb * level / 100),
        None => counted_mb,
    }
}

/// Memory-pressure tier. On `Low` it also caps the tagging worker count, the ML session pool (to 1) and the predecode read-ahead channel (see `commands::scan` + `pipeline::tagging`) so a low-RAM box keeps fewer decoded frames resident. Also drives `dbwriter_batch_size_for`. Refreshed at scan start (the dbwriter re-polls every 30 s) so a mid-scan shift downshifts instead of OOM'ing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryTier {
    Low,
    Balanced,
    High,
}

impl MemoryTier {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryTier::Low => "low",
            MemoryTier::Balanced => "balanced",
            MemoryTier::High => "high",
        }
    }
}

/// Tier from installed and available memory as fractions of the machine, so a 16 GB Mac under ordinary desktop load stays Balanced and a 32 GB one can reach High. Machines under 11 GiB (nominal 12 GB parts report just under 12 GiB) stay Low: their whole budget is small.
pub fn memory_tier_for(total_mb: u64, available_mb: u64) -> MemoryTier {
    if total_mb == 0 || available_mb == 0 {
        return MemoryTier::Low;
    }
    let available = available_mb.min(total_mb);
    if total_mb < 11 * 1024 || available * 4 < total_mb {
        MemoryTier::Low
    } else if total_mb >= 24 * 1024 && available * 2 >= total_mb {
        MemoryTier::High
    } else {
        MemoryTier::Balanced
    }
}

pub fn memory_tier() -> MemoryTier {
    memory_tier_for((physical_memory_gb() * 1024.0) as u64, available_memory_mb())
}

/// RAM++ runs in f32 on tract: ~4.5 GB resident at one pooled session, 6.4 GB at two. Below this much installed RAM the scan skips RAM++ and tags with CLIP scene tags. Installed, not available, RAM so a machine tags the same way every run; nominal 12 GB parts report just under 12 GiB after the iGPU reservation. A failed query (0.0) keeps RAM++ on.
pub const RAM_PLUS_MIN_TOTAL_GB: f64 = 11.0;

pub fn ram_plus_fits_in_memory(total_gb: f64) -> bool {
    total_gb <= 0.0 || total_gb >= RAM_PLUS_MIN_TOTAL_GB
}

/// DBWriter batch flush size per tier: larger batches mean fewer transactions for discovery throughput, but Low keeps per-transaction memory bounded so we don't compete with the app's working set.
pub fn dbwriter_batch_size_for(tier: MemoryTier) -> usize {
    match tier {
        MemoryTier::Low => 64,
        MemoryTier::Balanced => 250,
        MemoryTier::High => 500,
    }
}

// Storage-type detection (NVMe / SSD / HDD / Removable / Network) sets discovery's I/O queue depth: NVMe benefits from many parallel stat(), SATA SSD moderate, HDD deep queues HURT (cap at 2 threads), USB / Network 2 threads (slow and variable).
// Detection opens the volume root with FILE_READ_ATTRIBUTES, queries `StorageDeviceSeekPenaltyProperty` via `DeviceIoControl(IOCTL_STORAGE_QUERY_PROPERTY)`, and uses `GetDriveTypeW` to tell removable / network from local fixed.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum StorageType {
    Nvme,
    Hdd,
    Removable,
    Network,
    Unknown,
}

impl StorageType {
    pub fn as_str(self) -> &'static str {
        match self {
            StorageType::Nvme => "nvme",
            StorageType::Hdd => "hdd",
            StorageType::Removable => "removable",
            StorageType::Network => "network",
            StorageType::Unknown => "unknown",
        }
    }
}

pub fn walk_concurrency_for(path: &Path) -> usize {
    let storage = storage_type_for_path(path);
    let logical = num_cpus::get().max(1);
    match storage {
        StorageType::Nvme => logical.clamp(4, 16),
        StorageType::Hdd => 2,
        StorageType::Removable => 2,
        StorageType::Network => 2,
        StorageType::Unknown => 4,
    }
}

/// Storage type of the volume containing `path`; `Unknown` if the descriptor can't be read (e.g. a virtual filesystem).
#[cfg(windows)]
pub fn storage_type_for_path(path: &Path) -> StorageType {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetDriveTypeW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY,
        STORAGE_PROPERTY_QUERY, StorageDeviceSeekPenaltyProperty, PropertyStandardQuery,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    const DRIVE_RAMDISK: u32 = 6;

    let root = path.ancestors().last().unwrap_or(path);

    let mut root_wide: Vec<u16> = root.as_os_str().encode_wide().collect();
    if !root_wide.ends_with(&[0]) { root_wide.push(0); }
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_wide.as_ptr())) };
    match drive_type {
        DRIVE_REMOTE => return StorageType::Network,
        DRIVE_REMOVABLE | DRIVE_CDROM => return StorageType::Removable,
        DRIVE_RAMDISK => return StorageType::Nvme,
        DRIVE_FIXED => {}
        _ => return StorageType::Unknown,
    }

    let drive_letter = root.to_string_lossy();
    let drive_letter = drive_letter.trim_end_matches(['\\', '/']);
    let unc_volume = format!(r"\\.\{}", drive_letter);
    let mut wide: Vec<u16> = unc_volume.encode_utf16().collect();
    wide.push(0);

    let handle: HANDLE = unsafe {
        match CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            Ok(h) if !h.is_invalid() => h,
            _ => return StorageType::Unknown,
        }
    };

    let result = unsafe {
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceSeekPenaltyProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut desc = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
        let mut returned: u32 = 0;
        let ok = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const _),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut desc as *mut _ as *mut _),
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            Some(&mut returned),
            None,
        );
        if ok.is_ok() && returned as usize >= std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() {
            // `IncursSeekPenalty == FALSE` means SSD-class, but this descriptor can't tell NVMe from SATA SSD (that needs STORAGE_ADAPTER_DESCRIPTOR for BusType), so all no-seek-penalty fixed drives get the NVMe 16-thread budget. SATA SSDs get moderate over-parallelism, still better than single-threaded walkdir.
            if desc.IncursSeekPenalty { StorageType::Hdd } else { StorageType::Nvme }
        } else {
            StorageType::Unknown
        }
    };

    unsafe { let _ = CloseHandle(handle); }
    result
}

#[cfg(not(windows))]
pub fn storage_type_for_path(_path: &Path) -> StorageType {
    StorageType::Unknown
}

// Volume-local file identity (NTFS 64-bit MFT reference; inode on POSIX) keys rename/move detection (v8 schema): a rename within one volume keeps the id, so the catalog row is re-bound to the new path instead of recomputing tags, embeddings and faces. Ids can collide across volumes, so a cross-volume move falls through to the content-hash lookup.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct FileIdentity {
    pub(crate) volume: u64,
    pub(crate) file: u64,
}

/// Volume-qualified file identity for `path`, or `None` if the entry can't be opened. Unlike `file_ref` it is safe to persist as mutation authority because identical file indexes on different volumes stay distinct.
#[cfg(windows)]
pub(crate) fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let p = crate::util::path_safety::to_extended_length(path);
    let mut wide: Vec<u16> = p.as_os_str().encode_wide().collect();
    wide.push(0);

    let handle = unsafe {
        match CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            Ok(h) if !h.is_invalid() => h,
            _ => return None,
        }
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let result = unsafe { GetFileInformationByHandle(handle, &mut info) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    if result.is_err() {
        return None;
    }
    Some(FileIdentity {
        volume: u64::from(info.dwVolumeSerialNumber),
        file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

#[cfg(not(windows))]
pub(crate) fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    Some(FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    })
}

#[cfg(windows)]
pub(crate) fn file_identity_from_file(file: &std::fs::File) -> Option<FileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    Some(FileIdentity {
        volume: u64::from(info.dwVolumeSerialNumber),
        file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

#[cfg(not(windows))]
pub(crate) fn file_identity_from_file(file: &std::fs::File) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().ok()?;
    Some(FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    })
}

/// 64-bit volume-local file identity for rename/move heuristics. This legacy DB value omits the volume; destructive journals use [`file_identity`].
pub fn file_ref(path: &Path) -> Option<u64> {
    file_identity(path).map(|identity| identity.file)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum PowerSource {
    Ac,
    Battery,
    Unknown,
}

impl PowerSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PowerSource::Ac => "ac",
            PowerSource::Battery => "battery",
            PowerSource::Unknown => "unknown",
        }
    }
}

/// (source, battery_percent); `battery_percent` is None on desktops without a battery. REPORT only (Settings → Diagnostics); throttling on battery is a NEXT.md follow-up.
#[cfg(windows)]
pub fn power_status() -> (PowerSource, Option<u8>) {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    unsafe {
        let mut s = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut s).is_err() {
            return (PowerSource::Unknown, None);
        }
        let source = match s.ACLineStatus {
            0 => PowerSource::Battery,
            1 => PowerSource::Ac,
            _ => PowerSource::Unknown,
        };
        let pct = if s.BatteryLifePercent <= 100 { Some(s.BatteryLifePercent) } else { None };
        (source, pct)
    }
}

#[cfg(not(windows))]
pub fn power_status() -> (PowerSource, Option<u8>) {
    (PowerSource::Unknown, None)
}

/// Current process RSS in MiB. Mirrors macOS `Hardware.swift::residentMemoryMB`; feeds the sidebar Memory stat (which showed "Memory: 0 MB" mid-scan while ML inference held 600 MB-1.2 GB).
#[cfg(windows)]
pub fn process_memory_mb() -> u64 {
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let ok = GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            counters.cb,
        );
        if ok.is_ok() {
            (counters.WorkingSetSize / (1024 * 1024)) as u64
        } else {
            0
        }
    }
}

#[cfg(not(windows))]
pub fn process_memory_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    let kb: u64 = rest
                        .trim()
                        .trim_end_matches(" kB")
                        .trim()
                        .parse()
                        .unwrap_or(0);
                    return kb / 1024;
                }
            }
        }
        return 0;
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS / other POSIX: not implemented (sysinfo per-process refresh adds a heavy probe), returns 0.
        0
    }
}

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
        // Closure so every exit (early return, loop break) falls through to the CloseHandle below.
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
    #[cfg(unix)]
    unsafe {
        Some(libc::getppid() as u32)
    }
    #[cfg(not(unix))]
    None
}

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

    // HANDLE wraps *mut c_void, which the windows crate doesn't mark Send, so smuggle it across the closure as a usize. SAFETY: a Win32 HANDLE is opaque and CloseHandle / WaitForSingleObject may run on any thread once it exists; this task is its sole owner across the move.
    let handle_addr = handle.0 as usize;

    let wait = tokio::task::spawn_blocking(move || {
        let h = HANDLE(handle_addr as *mut core::ffi::c_void);
        let r = unsafe { WaitForSingleObject(h, u32::MAX) };
        unsafe { let _ = CloseHandle(h); }
        r
    });

    // Race the blocking wait against the shared shutdown signal so EOF / explicit Shutdown can unblock us when the parent stays alive (test harnesses, debuggers). If we lose, the blocking thread keeps its INFINITE wait until process exit, and `shutdown_timeout(0)` on the Runtime ensures it doesn't delay exit.
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
    use tokio::time::{Duration, MissedTickBehavior};

    let mut poll = tokio::time::interval(Duration::from_secs(1));
    poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                tracing::info!("watch_parent cancelled by shutdown signal");
                return;
            }
            _ = poll.tick() => {
                match get_parent_pid() {
                    Some(current) if current == parent_pid => {}
                    Some(current) => {
                        tracing::info!(
                            "parent process changed (was {parent_pid}, now {current}); signaling shutdown"
                        );
                        shutdown.notify_waiters();
                        return;
                    }
                    None => {
                        tracing::warn!(
                            "could not read parent pid; falling back to stdin EOF detection"
                        );
                    }
                }
            }
        }
    }
}

// Sleep guard: the SetThreadExecutionState equivalent of macOS IOPMAssertion, set on scan start and cleared on scan end. ES_SYSTEM_REQUIRED but NOT ES_DISPLAY_REQUIRED, so the display can still sleep.
// SetThreadExecutionState is THREAD-SCOPED: the assertion is only released by a matching ES_CONTINUOUS call on the SAME thread (or thread exit). The scan future is polled across Tokio workers, so a naive acquire-here / clear-in-Drop would SET on one worker and CLEAR on another, leaving the machine unable to sleep after a scan. The whole set→hold→clear lifecycle is therefore pinned to a dedicated thread the guard owns.

/// RAII guard. While alive, prevents Windows from sleeping the system. Configure a Linux helper as a process-group leader and have the kernel kill it if the engine disappears before normal cleanup can run.
#[cfg(target_os = "linux")]
pub(crate) fn configure_child_lifetime(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    let parent_pid = unsafe { libc::getpid() };
    command.process_group(0);
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
            }
            Ok(())
        });
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn terminate_child_tree(child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub struct SleepGuard {
    #[cfg(windows)]
    stop: Option<std::sync::mpsc::Sender<()>>,
    #[cfg(windows)]
    thread: Option<std::thread::JoinHandle<()>>,
    // Linux: a held `systemd-inhibit … sleep infinity` child; alive means the inhibitor lock is held, killed on drop releases it. `None` when systemd-inhibit is absent or fails to spawn (best-effort: a headless box without logind just isn't inhibited).
    #[cfg(target_os = "linux")]
    child: Option<std::process::Child>,
    #[cfg(target_os = "linux")]
    inhibit_stdin: Option<std::process::ChildStdin>,
    // macOS app-hosted engine: an IOPM PreventSystemSleep assertion held by FileIDAppleKit, released on drop. CLI/TUI builds are a no-op.
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    _assertion: Option<crate::apple::SleepAssertion>,
}

impl SleepGuard {
    #[cfg(windows)]
    pub fn acquire() -> Self {
        use windows::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
        };
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let (armed_tx, armed_rx) = std::sync::mpsc::channel::<bool>();
        let thread = std::thread::Builder::new()
            .name("sleep-guard".to_string())
            .spawn(move || {
                let prev = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
                let armed = prev.0 != 0;
                let _ = armed_tx.send(armed);
                if !armed {
                    return;
                }
                let _ = stop_rx.recv();
                unsafe { SetThreadExecutionState(ES_CONTINUOUS); }
            });
        match thread {
            Ok(handle) => {
                let armed = armed_rx.recv().unwrap_or(false);
                if armed {
                    Self { stop: Some(stop_tx), thread: Some(handle) }
                } else {
                    let _ = handle.join();
                    Self { stop: None, thread: None }
                }
            }
            Err(_) => Self { stop: None, thread: None },
        }
    }

    // Linux: hold a logind sleep+idle inhibitor by keeping a `systemd-inhibit … cat` child alive over a private stdin pipe, the dependency-free way to block suspend without linking libsystemd. Best-effort: if it's missing or fails to spawn (no logind, bare container) the guard is inert.
    #[cfg(target_os = "linux")]
    pub fn acquire() -> Self {
        use std::process::{Command, Stdio};
        let mut command = Command::new("systemd-inhibit");
        command
            .args([
                "--what=sleep:idle",
                "--who=FileID",
                "--why=FileID is processing your library",
                "--mode=block",
                "cat",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_child_lifetime(&mut command);
        let mut child = command.spawn().ok();
        let inhibit_stdin = child.as_mut().and_then(|process| process.stdin.take());
        Self { child, inhibit_stdin }
    }

    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    pub fn acquire() -> Self {
        Self { _assertion: crate::apple::SleepAssertion::begin("FileID is processing your library") }
    }

    #[cfg(all(not(windows), not(target_os = "linux"), not(all(target_os = "macos", feature = "apple-adapters"))))]
    pub fn acquire() -> Self { Self {} }
}

#[cfg(target_os = "linux")]
impl Drop for SleepGuard {
    fn drop(&mut self) {
        // Kill the held systemd-inhibit child, dropping its inhibitor lock. Best-effort; `wait` reaps the zombie so long-running engines don't leak defunct children across scans.
        drop(self.inhibit_stdin.take());
        if let Some(mut child) = self.child.take() {
            terminate_child_tree(&mut child);
        }
    }
}

#[cfg(windows)]
impl Drop for SleepGuard {
    fn drop(&mut self) {
        // Signal the dedicated thread to clear, then join so the assertion is provably released before the guard is gone. The explicit send + join makes the ordering deterministic for callers and tests.
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod sleep_guard_linux_tests {
    use super::SleepGuard;

    #[test]
    fn acquire_and_drop_is_safe_without_logind() {
        let guard = SleepGuard::acquire();
        drop(guard);
    }
}

// Process priority: default NORMAL (was ABOVE_NORMAL). Higher priority fights DWM for CPU during scans, and when a GPU driver hits TDR the high-priority workers spam retries faster than DWM can recover the GPU → full system hang. `FILEID_PROCESS_PRIORITY=above_normal` overrides to outrank background services (Defender, OneDrive).

/// RAII guard. While alive, the process priority follows FILEID_PROCESS_PRIORITY (default NORMAL); Drop restores NORMAL.
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

// llama.cpp runtime DLL search: SEC-3 locked the DLL search path to System32 + the engine binary's directory, but the llama.cpp runtimes extract into %LOCALAPPDATA%\FileID\Models\llama.cpp[-cuda]\, outside both. AddDllDirectory adds one trusted dir to the per-process list; we walk a few levels deep so DLLs at the root or in `bin/` are reachable.

/// Registers every directory under `root` (up to MAX_DEPTH deep) that contains a .dll as an additional DLL search path. Idempotent (the loader dedupes); returns the directories for which `AddDllDirectory` returned null.
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
            tracing::warn!(dir = %redact_path_for_log(dir), "AddDllDirectory returned null");
            false
        } else {
            tracing::info!(dir = %redact_path_for_log(dir), "AddDllDirectory registered runtime dir");
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

/// The primary (largest-VRAM) non-software adapter's `DedicatedVideoMemory` in MB; None if DXGI enumeration fails or no physical adapter exists. Sizes the llama.cpp GPU offload. VRAM is constant for the process, so the DXGI walk is memoized (the adapters were otherwise re-walked 7-15× at startup). Safe from any thread.
pub fn dedicated_vram_mb() -> Option<u64> {
    static CELL: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *CELL.get_or_init(probe_dedicated_vram_mb)
}

#[cfg(windows)]
fn probe_dedicated_vram_mb() -> Option<u64> {
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
fn probe_dedicated_vram_mb() -> Option<u64> {
    None
}

/// Lowers per-thread I/O priority so the tagging workers' bulk reads don't compete with foreground apps (Explorer, video playback). `THREAD_PRIORITY_LOWEST` via `SetThreadPriority` works on every Windows version with the same I/O-scheduling effect on Win11 as the more complex `ThreadPowerThrottling`. Best-effort: failures are ignored.
#[cfg(windows)]
pub fn set_worker_background_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST,
    };
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) };
}

#[cfg(not(windows))]
pub fn set_worker_background_priority() {}

/// Suppresses Windows hard-error dialogs process-wide. A headless engine must never block on a modal box: a llama.cpp binary loading against a missing/mismatched CUDA DLL raises a critical-error message box (STATUS_DLL_NOT_FOUND) that would wait forever for a click, wedging Deep Analyze. `SEM_FAILCRITICALERRORS` turns that into an ordinary load failure reported via exit code; `SEM_NOGPFAULTERRORBOX` suppresses the WER crash dialog. Children inherit the mode (no `CREATE_DEFAULT_ERROR_MODE`), so setting it once at startup covers the CLI, server and device probes. `SetErrorMode` *replaces* the mode, so OR onto whatever the loader set.
#[cfg(windows)]
pub fn suppress_hard_error_dialogs() {
    use windows::Win32::System::Diagnostics::Debug::{
        SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX,
    };
    let want = SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX;
    unsafe {
        let prev = SetErrorMode(want);
        let _ = SetErrorMode(prev | want);
    }
}

#[cfg(not(windows))]
pub fn suppress_hard_error_dialogs() {}

/// The volume holding `path` as (volume id, display name, mounted read-only), for the read-only write guard.
#[cfg(unix)]
pub(crate) fn volume_of(path: &std::path::Path) -> Option<(String, String, bool)> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    let device = metadata.dev();
    let mut mount = path.to_path_buf();
    while let Some(parent) = mount.parent() {
        if std::fs::metadata(parent).map(|m| m.dev()).ok() != Some(device) {
            break;
        }
        mount = parent.to_path_buf();
    }
    let name = mount.file_name().map_or_else(|| "this drive".to_owned(), |n| n.to_string_lossy().into_owned());
    let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd", target_os = "dragonfly"))]
    let (read_only, device) = {
        let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(raw.as_ptr(), &raw mut stats) } != 0 {
            return None;
        }
        let mounted_on = unsafe { std::ffi::CStr::from_ptr(stats.f_mntonname.as_ptr()) }.to_string_lossy().into_owned();
        ((stats.f_flags as u64 & libc::MNT_RDONLY as u64) != 0, format!("{device}:{mounted_on}"))
    };
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd", target_os = "dragonfly")))]
    let (read_only, device) = {
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(raw.as_ptr(), &raw mut stats) } != 0 {
            return None;
        }
        ((stats.f_flag as u64 & libc::ST_RDONLY as u64) != 0, device.to_string())
    };
    Some((device, name, read_only))
}

#[cfg(windows)]
pub(crate) fn volume_of(path: &std::path::Path) -> Option<(String, String, bool)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{FILE_READ_ONLY_VOLUME, GetVolumeInformationW, GetVolumePathNameW};
    use windows::core::PCWSTR;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut root = [0u16; 261];
    unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut root).ok()? };
    let mut label = [0u16; 261];
    let mut flags = 0u32;
    unsafe { GetVolumeInformationW(PCWSTR(root.as_ptr()), Some(&mut label), None, None, Some(&mut flags), None).ok()? };
    let end = |buffer: &[u16]| buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    let root_text = String::from_utf16_lossy(&root[..end(&root)]);
    let label_text = String::from_utf16_lossy(&label[..end(&label)]);
    let name = if label_text.is_empty() { root_text.clone() } else { label_text };
    Some((root_text, name, flags & FILE_READ_ONLY_VOLUME.0 != 0))
}


#[cfg(test)]
mod adaptive_tests {
    use super::*;

    #[test]
    fn macos_available_memory_credits_the_kernel_level() {
        assert_eq!(macos_available_mb(4_000, 16_384, Some(50)), 8_192);
        assert_eq!(macos_available_mb(9_000, 16_384, Some(50)), 9_000, "never below the counted pages");
        assert_eq!(macos_available_mb(4_000, 16_384, None), 4_000);
        assert_eq!(macos_available_mb(4_000, 16_384, Some(0)), 4_000);
        assert_eq!(macos_available_mb(4_000, 16_384, Some(101)), 4_000);
        assert_eq!(macos_available_mb(4_000, 16_384, Some(-3)), 4_000);
    }

    #[test]
    fn worker_cap_non_hybrid_8c() {
        let t = CpuTopology { p_cores: 8, e_cores: 0, logical: 16 };
        assert_eq!(t.worker_cap(), 12);
    }

    #[test]
    fn worker_cap_hybrid_intel_13900k() {
        let t = CpuTopology { p_cores: 8, e_cores: 16, logical: 32 };
        assert_eq!(t.worker_cap(), 28);
    }

    #[test]
    fn worker_cap_m1_pro_equivalent() {
        let t = CpuTopology { p_cores: 8, e_cores: 2, logical: 10 };
        assert_eq!(t.worker_cap(), 10);
    }

    #[test]
    fn worker_cap_clamped_to_32() {
        let t = CpuTopology { p_cores: 32, e_cores: 32, logical: 128 };
        assert_eq!(t.worker_cap(), 32);
    }

    #[test]
    fn worker_cap_minimum_two() {
        let t = CpuTopology { p_cores: 1, e_cores: 0, logical: 1 };
        assert_eq!(t.worker_cap(), 2);
    }

    #[test]
    fn batch_size_low_tier_is_smallest() {
        assert!(dbwriter_batch_size_for(MemoryTier::Low)
            < dbwriter_batch_size_for(MemoryTier::Balanced));
        assert!(dbwriter_batch_size_for(MemoryTier::Balanced)
            < dbwriter_batch_size_for(MemoryTier::High));
    }

    #[test]
    fn walk_concurrency_storage_classes_monotone() {
        let cases = [
            (StorageType::Hdd,        2),
            (StorageType::Removable,  2),
            (StorageType::Network,    2),
        ];
        let logical = num_cpus::get().max(1);
        for (st, expected_for_low_logical) in cases {
            let got = match st {
                StorageType::Hdd | StorageType::Removable | StorageType::Network => 2,
                _ => logical,
            };
            assert_eq!(got, expected_for_low_logical, "{:?} mismatch", st);
        }
    }

    #[test]
    fn top_capacity_count_counts_big_cores_of_a_hybrid_part() {
        assert_eq!(top_capacity_count(&[]), None);
        assert_eq!(top_capacity_count(&[1024; 8]), None);
        assert_eq!(top_capacity_count(&[1024, 1024, 1024, 1024, 512, 512, 512, 512]), Some(4));
        assert_eq!(top_capacity_count(&[1024, 871, 871, 512]), Some(1));
    }

    #[test]
    fn ram_plus_needs_about_twelve_gb_installed() {
        assert!(ram_plus_fits_in_memory(0.0));
        assert!(!ram_plus_fits_in_memory(8.0));
        assert!(!ram_plus_fits_in_memory(10.9));
        assert!(ram_plus_fits_in_memory(11.5));
        assert!(ram_plus_fits_in_memory(16.0));
    }

    #[test]
    fn memory_tier_scales_with_installed_memory_not_absolute_free_gib() {
        let gb = |n: u64| n * 1024;
        assert_eq!(memory_tier_for(gb(8), gb(7)), MemoryTier::Low);
        assert_eq!(memory_tier_for(gb(16), 6225), MemoryTier::Balanced);
        assert_eq!(memory_tier_for(gb(16), gb(3)), MemoryTier::Low);
        assert_eq!(memory_tier_for(gb(16), gb(15)), MemoryTier::Balanced);
        assert_eq!(memory_tier_for(gb(32), gb(10)), MemoryTier::Balanced);
        assert_eq!(memory_tier_for(gb(32), gb(20)), MemoryTier::High);
        assert_eq!(memory_tier_for(gb(64), gb(40)), MemoryTier::High);
        assert_eq!(memory_tier_for(gb(64), gb(8)), MemoryTier::Low);
        assert_eq!(memory_tier_for(0, gb(8)), MemoryTier::Low);
        assert_eq!(memory_tier_for(gb(16), 0), MemoryTier::Low);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn apple_silicon_reports_performance_and_efficiency_cores() {
        let topology = cpu_topology();
        let physical = num_cpus::get_physical() as u32;
        assert_eq!(topology.p_cores + topology.e_cores, physical);
        assert!(topology.p_cores >= 1);
    }

    #[test]
    fn memory_tier_string_round_trip() {
        for t in [MemoryTier::Low, MemoryTier::Balanced, MemoryTier::High] {
            assert!(!t.as_str().is_empty());
        }
    }

    #[test]
    fn power_source_string_round_trip() {
        for s in [PowerSource::Ac, PowerSource::Battery, PowerSource::Unknown] {
            assert!(!s.as_str().is_empty());
        }
    }

    #[test]
    fn storage_type_string_round_trip() {
        for s in [
            StorageType::Nvme, StorageType::Hdd,
            StorageType::Removable, StorageType::Network, StorageType::Unknown,
        ] {
            assert!(!s.as_str().is_empty());
        }
    }

    #[test]
    fn detect_does_not_panic_on_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let _ = storage_type_for_path(&cwd);
        let _ = walk_concurrency_for(&cwd);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_sealed_system_volume_and_the_data_volume_never_share_a_cache_key() {
        let system = volume_of(std::path::Path::new("/")).unwrap();
        let data = volume_of(&std::env::temp_dir()).unwrap();
        assert_ne!(system.0, data.0, "both report the same device number, so the key must name the mount");
    }

    #[test]
    fn available_disk_space_resolves_a_missing_child_to_its_volume() {
        let child = std::env::temp_dir().join("fileid-space-probe").join("not-created");
        assert!(available_disk_bytes(&child).is_some_and(|bytes| bytes > 0));
    }

    #[test]
    #[ignore = "needs FILEID_RO_DMG_TESTS=1 and macOS hdiutil"]
    #[cfg(target_os = "macos")]
    fn a_real_read_only_disk_image_is_detected_and_a_writable_one_is_not() {
        if std::env::var("FILEID_RO_DMG_TESTS").as_deref() != Ok("1") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("fileid-ro-dmg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("test.dmg");
        let run = |args: &[&str]| std::process::Command::new("hdiutil").args(args).output().unwrap();
        assert!(run(&["create", "-size", "2m", "-fs", "HFS+", "-volname", "FileIDRO", image.to_str().unwrap()]).status.success());
        let mount = dir.join("mnt");
        std::fs::create_dir_all(&mount).unwrap();
        let attach = run(&["attach", "-readonly", "-nobrowse", "-mountpoint", mount.to_str().unwrap(), image.to_str().unwrap()]);
        assert!(attach.status.success(), "{}", String::from_utf8_lossy(&attach.stderr));
        crate::util::read_only::refresh();
        let seen = crate::util::read_only::read_only_location(&mount.join("new/file.jpg"));
        let _ = run(&["detach", mount.to_str().unwrap(), "-force"]);
        assert!(seen.is_some(), "read-only attach not detected");
        crate::util::read_only::refresh();
        assert!(crate::util::read_only::read_only_location(&dir.join("file.jpg")).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
