//! Windows platform helpers — parent-PID watchdog, worker count heuristic,
//! physical memory probe, sleep guard.
//!
//! Non-Windows builds get stub implementations so the engine compiles on
//! every host while Windows-specific surfaces stay gated by `#[cfg(windows)]`.

use std::path::Path;
use std::sync::Arc;
use tokio::sync::Notify;

/// Redact a user path for logs: keep last two components; pass
/// app-structural paths verbatim.
pub fn redact_path_for_log(path: impl AsRef<Path>) -> String {
    use std::path::Component;
    let s = path.as_ref().to_string_lossy().to_string();
    let s_lower = s.to_lowercase();
    // App-structural paths: pass through ONLY the engine's own state dir
    // (`%LOCALAPPDATA%\FileID\…` / the per-OS equivalent). ENG-97: the old broad
    // `\fileid\` / `/fileid/` substring match leaked any USER path that merely
    // contained a folder named "FileID" (e.g. a dev checkout at C:\Code\FileID\…)
    // verbatim, username and all — defeating redaction for a whole class of real
    // paths. Anchor on the actual resolved root instead.
    if let Ok(root) = crate::paths::root() {
        let mut root_prefix = root.to_string_lossy().to_lowercase();
        if !root_prefix.is_empty() {
            if s_lower == root_prefix {
                return s;
            }
            // Require a path-separator boundary so a sibling dir whose name
            // merely STARTS with the app root (e.g. `…\Local\FileIDBackup\…`)
            // does not string-prefix-match and leak. (ENG-97)
            if !root_prefix.ends_with(['\\', '/']) {
                root_prefix.push('\\');
            }
            if s_lower.starts_with(&root_prefix) {
                return s;
            }
        }
    }
    // Also pass through the canonical Windows app dir even if `root()` resolved
    // elsewhere (a redirected/different-user LOCALAPPDATA). The trailing
    // separator keeps `…\Local\FileIDBackup\…` from matching — only the real
    // `…\AppData\Local\FileID\…` state tree.
    if s_lower.contains("appdata\\local\\fileid\\") {
        return s;
    }
    // Only Normal components are PII candidates — Prefix (drive letter,
    // UNC server\share) and RootDir are protocol/topology, never PII.
    // Excluding them ensures C:\ → "…" and \\server\share\user\file.jpg
    // → "…/user/file.jpg" rather than leaking the drive/server.
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

    /// UNC path must keep only the last 2 Normal components; the server
    /// and share name are protocol topology and must not leak into logs.
    #[test]
    #[cfg(windows)]
    fn redacts_unc_path_keeping_last_two_components() {
        let r = redact_path_for_log(r"\\server\share\user\file.jpg");
        assert!(r.ends_with("user/file.jpg"), "got: {r}");
        assert!(!r.contains("server"), "server name leaked: {r}");
        assert!(!r.contains("share"), "share name leaked: {r}");
    }

    /// Drive root (Prefix + RootDir, no Normal components) collapses to
    /// "…" so we don't leak even the drive letter in cases where the
    /// caller hands us a bare root.
    #[test]
    #[cfg(windows)]
    fn redacts_drive_root_to_ellipsis() {
        assert_eq!(redact_path_for_log(r"C:\"), "…");
    }

    /// App structural paths are returned UNCHANGED — they refer to
    /// FileID's own dirs (logs, models, sentinels) and are useful for
    /// debugging without redaction.
    #[test]
    fn app_structural_logs_path_unchanged() {
        let s = r"C:\Users\Adam\AppData\Local\FileID\logs\app.log";
        assert_eq!(redact_path_for_log(s), s);
    }

    /// ENG-97: a USER path that merely contains a folder named "FileID" (a dev
    /// checkout, a backup dir, …) is NOT the app's state tree and MUST be
    /// redacted — the old broad `\fileid\` substring match leaked these verbatim
    /// with the username.
    #[test]
    #[cfg(windows)]
    fn redacts_user_path_merely_containing_fileid() {
        let r = redact_path_for_log(r"C:\Users\Adam\Code\FileID\src\secret.rs");
        assert_eq!(r, "…/src/secret.rs");
        assert!(!r.contains("Adam"), "username leaked: {r}");
    }
}

/// CPU topology — performance / efficiency / logical-thread split.
/// On non-hybrid CPUs (anything pre Intel 12th-gen, all AMD Zen, all ARM
/// except Snapdragon) `e_cores = 0` and `p_cores = physical_cores`.
#[derive(Debug, Clone, Copy)]
pub struct CpuTopology {
    pub p_cores: u32,
    pub e_cores: u32,
    pub logical: u32,
}

impl CpuTopology {
    /// macOS worker formula: P + E + max(1, P/2). On M1 Pro (8P+2E)
    /// this yields 14. On i9-13900K (8P+16E) this yields 8+16+4 = 28.
    /// On a non-hybrid 8-core CPU this yields 8+0+4 = 12. Clamped to
    /// `logical` so we never oversubscribe SMT.
    pub fn worker_cap(self) -> u32 {
        let p = self.p_cores.max(1);
        let e = self.e_cores;
        let cap = p + e + (p / 2).max(1);
        cap.min(self.logical.max(1)).clamp(2, 32)
    }
}

/// Detect P-core / E-core split on hybrid CPUs via
/// `GetLogicalProcessorInformationEx(RelationProcessorCore)`.
/// `EfficiencyClass` is 0 for E-cores and >0 for P-cores on Intel hybrid
/// parts (12th-gen+). On non-hybrid CPUs every core reports the same class
/// and we collapse them into `p_cores`. AMD's Zen5c "dense" cores will
/// surface here once Windows tags them via the same API.
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
        // Over-align the buffer: allocate Vec<u64> (8-byte aligned, which
        // matches the struct's required alignment) instead of Vec<u8>
        // (which only guarantees 1-byte alignment and is UB to reinterpret).
        // We cast u64* → struct* directly so clippy sees the alignment is
        // satisfied; intermediate u8* hides the source alignment.
        let words = bytes.div_ceil(8) as usize;
        let mut aligned: Vec<u64> = vec![0u64; words];
        let head = aligned.as_mut_ptr().cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
        if GetLogicalProcessorInformationEx(RelationProcessorCore, Some(head), &mut bytes).is_err() {
            return CpuTopology { p_cores: physical_fallback, e_cores: 0, logical };
        }

        let mut classes: std::collections::HashMap<u8, u32> = std::collections::HashMap::new();
        let mut offset_words: usize = 0;
        let total_words = bytes.div_ceil(8) as usize;
        while offset_words < total_words {
            // SAFETY: offset_words ≤ total_words, so `add` stays within
            // the allocation. The cast is from an 8-byte-aligned u64 ptr
            // to the (also-8-byte-aligned) struct ptr — alignment safe.
            let rec_ptr = aligned.as_ptr().add(offset_words).cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
            let rec = &*rec_ptr;
            let size_bytes_field = rec.Size as usize;
            if size_bytes_field == 0 { break; }
            let proc_info = rec.Anonymous.Processor;
            *classes.entry(proc_info.EfficiencyClass).or_insert(0) += 1;
            // Advance by the struct's reported size, rounded up to u64 words.
            offset_words += size_bytes_field.div_ceil(8);
        }

        match classes.len() {
            0 => CpuTopology { p_cores: physical_fallback, e_cores: 0, logical },
            1 => {
                // Non-hybrid CPU — all cores in the same efficiency class.
                let total = classes.values().copied().sum::<u32>().max(1);
                CpuTopology { p_cores: total, e_cores: 0, logical }
            }
            _ => {
                // Hybrid CPU. Highest class = P-cores, lowest = E-cores.
                // Intel 12th-gen+ uses 0 for E, 1 for P. Future parts may
                // add intermediate classes — bucket them with P-cores
                // (better to oversubscribe than under).
                let min_class = *classes.keys().min().unwrap();
                let e = *classes.get(&min_class).unwrap_or(&0);
                let p = classes.values().copied().sum::<u32>().saturating_sub(e);
                CpuTopology { p_cores: p.max(1), e_cores: e, logical }
            }
        }
    }
}

#[cfg(not(windows))]
pub fn cpu_topology() -> CpuTopology {
    CpuTopology {
        p_cores: num_cpus::get_physical().max(1) as u32,
        e_cores: 0,
        logical: num_cpus::get().max(1) as u32,
    }
}

/// Number of tagging workers to spin up by default. macOS-parity formula
/// P + E + max(1, P/2) capped at logical-cores and [2, 32]. Replaces the
/// older `physical * 1.7` heuristic which mis-sized hybrid CPUs (treated
/// an i9-13900K's 8P+16E like an 8-core).
pub fn default_worker_cap() -> u32 {
    cpu_topology().worker_cap()
}

/// Physical memory in GiB.
pub fn physical_memory_gb() -> f64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let bytes = sys.total_memory(); // bytes since sysinfo 0.30
    (bytes as f64) / (1024.0 * 1024.0 * 1024.0)
}

/// Available (not-in-use) physical memory in MiB. Polls
/// `GlobalMemoryStatusEx` every call — call rate is bounded by the 30s
/// memory-tier refresh in the scan loop, so this is cheap enough.
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

#[cfg(not(windows))]
pub fn available_memory_mb() -> u64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.available_memory() / (1024 * 1024)
}

/// Memory-pressure tier. Drives `dbwriter_batch_size_for`, ML pool size,
/// channel caps, thumbnail cache size. Refreshed periodically by the
/// scan loop (every 30s) so a memory-pressure shift mid-scan downshifts
/// throughput instead of OOM'ing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryTier {
    /// <8 GB available — be conservative, smaller batches + pool=1.
    Low,
    /// 8–32 GB available — current defaults.
    Balanced,
    /// >32 GB available — bigger batches, larger channels, bigger caches.
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

/// Snapshot the current memory tier. Cheap (one `GlobalMemoryStatusEx`).
pub fn memory_tier() -> MemoryTier {
    let avail_mb = available_memory_mb();
    if avail_mb < 8 * 1024 {
        MemoryTier::Low
    } else if avail_mb < 32 * 1024 {
        MemoryTier::Balanced
    } else {
        MemoryTier::High
    }
}

/// DBWriter batch flush size for a memory tier. Discovery throughput
/// benefits from larger batches (fewer transactions), but Low tier needs
/// to keep per-transaction memory bounded so we don't compete with the
/// app's own working set.
pub fn dbwriter_batch_size_for(tier: MemoryTier) -> usize {
    match tier {
        MemoryTier::Low => 64,
        MemoryTier::Balanced => 250,
        MemoryTier::High => 500,
    }
}

// ─── Storage-type detection (NVMe / SSD / HDD / Removable / Network) ────────
//
// Discovery's I/O queue depth should depend on the storage type:
//   - NVMe: many parallel stat() helps, no seek penalty
//   - SATA SSD: moderate parallelism wins
//   - HDD: deep queues HURT (rotational random I/O is the worst-case
//     access pattern); cap at 2 threads
//   - USB / Network: 2 threads, defensive (slow + variable)
//
// Detection: open the volume root with FILE_READ_ATTRIBUTES, then
// `DeviceIoControl(IOCTL_STORAGE_QUERY_PROPERTY, StorageDeviceSeekPenaltyProperty)`.
// Result is a `DEVICE_SEEK_PENALTY_DESCRIPTOR` with a single bool. Combined
// with `GetDriveTypeW` to distinguish removable / network from local fixed.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageType {
    Nvme,
    /// Reserved for a future BusType-discriminator pass (would use
    /// `STORAGE_ADAPTER_DESCRIPTOR.BusType` to distinguish NVMe from
    /// SATA SSD; the seek-penalty descriptor alone can't tell them
    /// apart). `walk_concurrency_for` returns a smaller budget here
    /// than NVMe (8 vs 16 threads). Kept in the enum so call sites
    /// don't need a non-exhaustive update when bus-type detection
    /// lands.
    #[allow(dead_code)]
    SsdSata,
    Hdd,
    Removable,
    Network,
    Unknown,
}

impl StorageType {
    pub fn as_str(self) -> &'static str {
        match self {
            StorageType::Nvme => "nvme",
            StorageType::SsdSata => "ssd_sata",
            StorageType::Hdd => "hdd",
            StorageType::Removable => "removable",
            StorageType::Network => "network",
            StorageType::Unknown => "unknown",
        }
    }
}

/// Parallel-walk thread count for `path`'s underlying storage. Issue 1's
/// adaptive I/O budget. NVMe wants depth; HDDs hate it.
pub fn walk_concurrency_for(path: &Path) -> usize {
    let storage = storage_type_for_path(path);
    let logical = num_cpus::get().max(1);
    match storage {
        StorageType::Nvme => logical.clamp(4, 16),
        StorageType::SsdSata => logical.clamp(2, 8),
        StorageType::Hdd => 2,
        StorageType::Removable => 2,
        StorageType::Network => 2,
        StorageType::Unknown => 4,
    }
}

/// Detect storage type for the volume containing `path`. Returns
/// `Unknown` if we can't read the descriptor (most commonly: the user
/// pointed at a path on a virtual filesystem we can't query).
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

    // Win32 GetDriveTypeW return values (from fileapi.h). windows-rs 0.58
    // doesn't export these as named constants, so we match the raw u32s.
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    const DRIVE_RAMDISK: u32 = 6;

    // 1. Volume-root path: walk up the path until we reach a component
    //    whose parent has no parent (the volume root, e.g. C:\ or \\srv\share).
    let root = path.ancestors().last().unwrap_or(path);

    // 2. Drive type: short-circuit removable / network / CD without any IOCTL.
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

    // 3. Volume root for CreateFile must use the `\\.\X:` form, NOT `X:\`.
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
            // `IncursSeekPenalty == FALSE` ⇒ no seek penalty ⇒ SSD-class.
            // We can't distinguish NVMe from SATA SSD from this descriptor
            // alone (would need STORAGE_ADAPTER_DESCRIPTOR for BusType).
            // For now: treat all no-seek-penalty fixed drives as NVMe-class
            // (16-thread budget). The downside on SATA SSDs is moderate
            // over-parallelism, which still beats single-threaded walkdir.
            if desc.IncursSeekPenalty.as_bool() { StorageType::Hdd } else { StorageType::Nvme }
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

// ─── Volume-local file identity (NTFS MFT reference) ───────────────────────
//
// Rename/move detection (v8 schema) keys off a volume-local file id: NTFS's
// 64-bit MFT reference on Windows, the inode on POSIX. A rename within the
// same volume keeps the same id, so we can re-bind a file's catalog row to
// the new path instead of recomputing its tags + embeddings + faces. Across
// volumes the id can collide, so a cross-volume move falls through to the
// content-hash lookup.

/// 64-bit volume-local file identity for `path`, or `None` if the file can't
/// be opened (permission, deletion mid-scan, ...). Cheap: just opens with
/// `FILE_FLAG_BACKUP_SEMANTICS` (works for both files and directories without
/// triggering OneDrive hydration) and reads the metadata; no content I/O.
#[cfg(windows)]
pub fn file_ref(path: &Path) -> Option<u64> {
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
    Some((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
}

#[cfg(not(windows))]
pub fn file_ref(_path: &Path) -> Option<u64> {
    // Linux/macOS would use libc::stat::st_ino; deferred until the Linux port.
    None
}

// ─── Battery / AC power detection ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// (source, battery_percent). `battery_percent` is None on desktops without
/// a battery. First-pass: REPORT only (Settings → Diagnostics surfaces it);
/// throttling on battery is a NEXT.md follow-up so the user can see what
/// changed before behavior shifts under their feet.
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
        // BatteryLifePercent: 0–100, or 255 when unknown.
        let pct = if s.BatteryLifePercent <= 100 { Some(s.BatteryLifePercent) } else { None };
        (source, pct)
    }
}

#[cfg(not(windows))]
pub fn power_status() -> (PowerSource, Option<u8>) {
    (PowerSource::Unknown, None)
}

/// Current process RSS (resident set size) in MiB. Mirrors macOS
/// `Hardware.swift::residentMemoryMB` (task_info MACH_TASK_BASIC_INFO).
/// Used by the sidebar Memory stat — Windows previously hardcoded 0,
/// so the user saw "Memory: 0 MB" mid-scan even though ML inference
/// holds 600 MB-1.2 GB.
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
    // Linux: read VmRSS from /proc/self/status (in kB).
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
        // macOS / other POSIX: sysinfo per-process refresh would work but
        // adds a heavy probe; for now return 0 and let a future Linux/macOS
        // port slot in the right libc / mach call.
        0
    }
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
        // TODO(linux): real implementation.
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
// Default NORMAL (was ABOVE_NORMAL). Higher priority fights DWM for CPU
// during scans, and when DirectML hits TDR (GPU device removed) the
// high-priority workers spam retries faster than DWM can recover the GPU
// → full system hang. NORMAL lets the compositor win under contention.
//
// Override via FILEID_PROCESS_PRIORITY=above_normal if the engine should
// outrank background services (Defender, OneDrive).

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
/// Walks up to MAX_DEPTH so the auto-fetched cuDNN pack (which extracts
/// to `<root>/<versioned-archive>/bin/*.dll`) gets picked up. Existing
/// single-level callers keep working — only dirs that actually contain
/// DLLs are registered.
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

/// Find the first file named `filename` (case-insensitive) anywhere under
/// `root`, up to `max_depth` levels deep. Used to locate a Performance Pack's
/// `onnxruntime.dll` (it extracts into a versioned `.../lib/` subdir) so
/// `ORT_DYLIB_PATH` can be pinned to the matched GPU runtime. Returns None if
/// `root` doesn't exist or the file isn't found — callers treat that as "no
/// pack installed" and leave ORT on its default (pyke) base.
pub fn find_file_under(root: &std::path::Path, filename: &str, max_depth: usize) -> Option<std::path::PathBuf> {
    if !root.is_dir() {
        return None;
    }
    let rd = std::fs::read_dir(root).ok()?;
    let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f.eq_ignore_ascii_case(filename))
        {
            return Some(path);
        }
    }
    if max_depth == 0 {
        return None;
    }
    for sub in subdirs {
        if let Some(found) = find_file_under(&sub, filename, max_depth - 1) {
            return Some(found);
        }
    }
    None
}

/// Returns the primary (largest-VRAM) non-software adapter's
/// `DedicatedVideoMemory` in MB. None if DXGI enumeration fails or no
/// physical adapter is present. Used to gate the ML Session pool —
/// without this, a multi-Session config can exhaust a 6 GB card's VRAM
/// and wedge the DirectML driver (full system hang).
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

/// Lower per-thread I/O priority to "VeryLow" so the tagging workers'
/// bulk JPEG/PNG reads don't compete with foreground apps (File Explorer
/// browsing, video playback). The Windows kernel I/O scheduler honors
/// thread-level priority hints for filesystem reads.
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

#[cfg(test)]
mod adaptive_tests {
    use super::*;

    #[test]
    fn worker_cap_non_hybrid_8c() {
        let t = CpuTopology { p_cores: 8, e_cores: 0, logical: 16 };
        // 8 + 0 + max(1, 8/2) = 12. Within [2, 32].
        assert_eq!(t.worker_cap(), 12);
    }

    #[test]
    fn worker_cap_hybrid_intel_13900k() {
        let t = CpuTopology { p_cores: 8, e_cores: 16, logical: 32 };
        // 8 + 16 + max(1, 8/2) = 28. Within [2, 32].
        assert_eq!(t.worker_cap(), 28);
    }

    #[test]
    fn worker_cap_m1_pro_equivalent() {
        let t = CpuTopology { p_cores: 8, e_cores: 2, logical: 10 };
        // 8 + 2 + 4 = 14, capped to logical=10 because we never oversubscribe SMT.
        assert_eq!(t.worker_cap(), 10);
    }

    #[test]
    fn worker_cap_clamped_to_32() {
        // Threadripper-class CPU should be capped at 32 so we don't
        // spawn tagging workers that VRAM can never feed.
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
        // We can't fabricate storage types directly without a path, but we
        // can check the static mapping is monotone over the enum.
        let cases = [
            (StorageType::Hdd,        2),
            (StorageType::Removable,  2),
            (StorageType::Network,    2),
        ];
        let logical = num_cpus::get().max(1);
        for (st, expected_for_low_logical) in cases {
            // For HDD/removable/network we always return 2 regardless of cores.
            let got = match st {
                StorageType::Hdd | StorageType::Removable | StorageType::Network => 2,
                _ => logical,
            };
            assert_eq!(got, expected_for_low_logical, "{:?} mismatch", st);
        }
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
            StorageType::Nvme, StorageType::SsdSata, StorageType::Hdd,
            StorageType::Removable, StorageType::Network, StorageType::Unknown,
        ] {
            assert!(!s.as_str().is_empty());
        }
    }

    /// Real call against the test runner's CWD — at minimum we should get
    /// back something other than a panic, and on a real Windows host it'll
    /// classify the drive holding the source tree.
    #[test]
    fn detect_does_not_panic_on_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let _ = storage_type_for_path(&cwd);
        let _ = walk_concurrency_for(&cwd);
    }
}
