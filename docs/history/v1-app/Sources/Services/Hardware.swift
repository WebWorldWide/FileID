import Foundation
import Darwin
import Dispatch

// MARK: - Hardware

// RAM- and core-scaled runtime limits, computed once at launch.
enum Hardware {
    // MARK: Memory pressure

    private final class PressureMonitor: @unchecked Sendable {
        static let shared = PressureMonitor()
        private let lock = NSLock()
        private var source: DispatchSourceMemoryPressure?
        private var _pressure: Int32 = 0  // 0 normal, 1 warning, 2 critical

        func install() {
            lock.lock()
            defer { lock.unlock() }
            guard source == nil else { return }
            let s = DispatchSource.makeMemoryPressureSource(
                eventMask: [.warning, .critical, .normal],
                queue: .global(qos: .utility)
            )
            s.setEventHandler { [weak self] in
                guard let self else { return }
                let flags = s.data
                if flags.contains(.critical) {
                    self._pressure = 2
                    NSLog("FileID memory pressure: CRITICAL at \(Hardware.residentMB()) MB resident")
                } else if flags.contains(.warning) {
                    self._pressure = 1
                    NSLog("FileID memory pressure: warning at \(Hardware.residentMB()) MB resident")
                } else {
                    self._pressure = 0
                }
            }
            s.resume()
            source = s
        }

        var level: Int32 { _pressure }
    }

    static func installMemoryPressureMonitor() {
        PressureMonitor.shared.install()
    }

    static var isUnderMemoryPressure: Bool {
        PressureMonitor.shared.level > 0
    }

    static var isUnderCriticalMemoryPressure: Bool {
        PressureMonitor.shared.level >= 2
    }

    // Process resident memory in MB. Returns `-1` on failure so callers
    // (memory-pressure gates, logs) can distinguish "no memory used" (0)
    // from "couldn't query" (-1). All current call sites are diagnostic
    // (NSLog / scan.log lines), but keeping the sentinel disciplined here
    // means future gates can rely on it.
    static func residentMB() -> Int {
        var info  = mach_task_basic_info()
        var count = mach_msg_type_number_t(MemoryLayout<mach_task_basic_info>.size) / 4
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: 1) {
                task_info(mach_task_self_, task_flavor_t(MACH_TASK_BASIC_INFO), $0, &count)
            }
        }
        guard kr == KERN_SUCCESS else { return -1 }
        return Int(info.resident_size / 1_048_576)
    }

    // System-wide free+inactive+purgeable memory, in MB. Used to gate
    // large MLX model loads on low-RAM systems so we don't get Jetsam-killed
    // during the 3 GB Qwen2-VL weight upload.
    static func availableMemoryMB() -> Int {
        var stats = vm_statistics64_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<vm_statistics64_data_t>.size
                                           / MemoryLayout<integer_t>.size)
        let kr = withUnsafeMutablePointer(to: &stats) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                host_statistics64(mach_host_self(), HOST_VM_INFO64, $0, &count)
            }
        }
        guard kr == KERN_SUCCESS else { return -1 }
        // getpagesize() is a concurrency-clean function; the Darwin global
        // `vm_kernel_page_size` is a shared mutable var and trips strict-concurrency.
        let pageSize = UInt64(getpagesize())
        let usable = (UInt64(stats.free_count)
                    + UInt64(stats.inactive_count)
                    + UInt64(stats.purgeable_count)) * pageSize
        return Int(usable / 1_048_576)
    }

    // Gate for loading Qwen2-VL / Gemma / other VLMs. A fully loaded 4-bit
    // Qwen2-VL 2B is ~2.5 GB of MLX GPU memory + ~300 MB scaffolding. On
    // a 16 GB Mac with the user's browser, IDE, and Vision workers already
    // resident, a naive `loadContainer` is the classic Jetsam trigger —
    // the window vanishes with no .ips report because SIGKILL doesn't dump.
    //
    // Thresholds:
    //   16 GB tier → need ≥3 GB free; otherwise defer
    //   24 GB tier → need ≥2 GB free (usually fine)
    //   48 GB+ tier → always allowed
    static func canSafelyLoadLargeModel() -> Bool {
        if physicalMemoryGB >= 48 { return true }
        let required = physicalMemoryGB >= 24 ? 2_000 : 3_000
        let avail = availableMemoryMB()
        // Treat sentinel (-1, mach call failed) as "don't risk it" rather
        // than as "0 MB free → block." The check is intentionally
        // pessimistic: if we can't measure, we err toward caution to avoid
        // the SIGKILL crash mode the gate exists to prevent.
        guard avail >= 0 else { return false }
        return avail >= required
    }

    // Auto-run Deep Analyze after scan only on machines where a 3 GB model
    // load isn't a Jetsam risk. Users on 16 GB Macs have to opt in from
    // Settings or hit "Run Deep Analyze on current library" manually.
    static var deepAnalyzeAutoDefaultOn: Bool { physicalMemoryGB >= 24 }

    // MARK: Hardware profile
    static let physicalMemoryGB: Double = {
        let bytes = Double(ProcessInfo.processInfo.physicalMemory)
        return (bytes / 1_073_741_824).rounded()
    }()

    static let coreCount: Int = ProcessInfo.processInfo.activeProcessorCount

    // Pin Vision work to P-cores; .utility on E-cores was leaving P-cores idle.
    static let performanceCoreCount: Int = {
        var n: Int32 = 0
        var size = MemoryLayout<Int32>.size
        if sysctlbyname("hw.perflevel0.physicalcpu", &n, &size, nil, 0) == 0, n > 0 {
            return Int(n)
        }
        return max(2, coreCount - 2)
    }()

    static let efficiencyCoreCount: Int = {
        var n: Int32 = 0
        var size = MemoryLayout<Int32>.size
        if sysctlbyname("hw.perflevel1.physicalcpu", &n, &size, nil, 0) == 0, n > 0 {
            return Int(n)
        }
        return max(0, coreCount - performanceCoreCount)
    }()

    // Vision workers: P-cores + ALL E-cores + extra "queue depth" for ANE.
    //
    // Each worker spends ~half its wall time blocked on Vision (ANE) and CLIP
    // (ANE/GPU) calls. While a worker is in ANE, its P-core is idle. To keep
    // P-cores pinned during the CPU stages (decode, dHash, EXIF, face-print
    // archive, CLIP classify) we want MORE workers than P-cores so a
    // CPU-stage worker is always ready to run when a P-core frees up.
    //
    // Formula: `performanceCoreCount + efficiencyCoreCount + (P/2)` extra
    // queue-depth workers. On M1 Pro (8P + 2E) → 14 workers (was 9). On Mac
    // Studio Ultra (16P + 8E) → 32 workers. Capped at 32 to avoid runaway
    // memory on hypothetical future 64-P-core machines.
    //
    // Memory cost per worker: ~20 KB of reusable VNRequest objects. 14
    // workers = ~280 KB. Negligible vs. the ANE saturation win.
    static let workerCap: Int = {
        let computed = performanceCoreCount + efficiencyCoreCount + max(1, performanceCoreCount / 2)
        return min(32, max(4, computed))
    }()

    // Diagnostic reference only — no longer gates the worker pool.
    // Tiers extend up through Mac Pro / Mac Studio Ultra hardware so a future
    // user with a 192 GB or 512 GB machine isn't artificially constrained.
    static let visionCeilingMB: Int = {
        if physicalMemoryGB >= 192 { return 48_000 }
        if physicalMemoryGB >= 96  { return 28_000 }
        if physicalMemoryGB >= 48  { return 12_000 }
        if physicalMemoryGB >= 24  { return 7_000  }
        return 3_500
    }()

    // Memory budget reality check on a 16 GB Mac:
    //   FileID baseline ~500 MB + Vision workers ~600 MB + MLX cache when CLIP
    //   loaded ~300 MB + thumbnail cache + face print cache + SwiftData WAL.
    //   Once Deep Analyze loads Qwen 3 GB the headroom is ~9 GB total. After
    //   the OS, browser, IDE, the user can hit memory pressure fast.
    //
    // Tiers kept conservative on the 16 GB end and aggressive on the high
    // end — a Mac Studio Ultra (192 GB) shouldn't be running with a 1 200 MB
    // thumbnail cache when it has the headroom for an order of magnitude more.
    // Per-tier values are picked so resident memory stays under ~30 % of
    // physical RAM during a 100 K-file scan, leaving headroom for the OS,
    // browser, IDE, and the user's other work.
    // Bumped across all tiers (Batch 17): externalStorage on FileRecord blobs
    // keeps the SQLite row light, so the in-memory thumbnail cache can claim
    // more of the available RAM without tipping into pressure during scan.
    static let thumbnailCacheMB: Int = {
        if physicalMemoryGB >= 192 { return 12_000 }
        if physicalMemoryGB >= 96  { return 6_000  }
        if physicalMemoryGB >= 48  { return 3_000  }
        if physicalMemoryGB >= 24  { return 1_800  }
        return 900
    }()

    static let thumbnailCountLimit: Int = {
        if physicalMemoryGB >= 192 { return 18_000 }
        if physicalMemoryGB >= 96  { return 9_000  }
        if physicalMemoryGB >= 48  { return 4_500  }
        if physicalMemoryGB >= 24  { return 2_500  }
        return 1_200
    }()

    // Records per SwiftData batch save. Larger batches cut WAL fsync overhead
    // but inflate the in-memory ModelContext between commits. With Batch 14's
    // WAL checkpoint + Batch 15's externalStorage, the ModelContext footprint
    // per record is much smaller — bumped batch sizes across the board.
    static let saveEvery: Int = {
        if physicalMemoryGB >= 192 { return 6_000 }
        if physicalMemoryGB >= 96  { return 3_500 }
        if physicalMemoryGB >= 48  { return 2_000 }
        if physicalMemoryGB >= 24  { return 1_000 }
        return 600
    }()

    // SwiftData @Query fetchLimit for grids (Library, Cleanup) — set per tier
    // so a 16 GB Mac doesn't try to materialize 100 K FileRecords into the
    // grid query at once, but a Mac Pro can show more without paginating.
    // Read by view code that owns the FetchDescriptor; safe to query at
    // launch since it's a static let.
    static let gridFetchLimit: Int = {
        if physicalMemoryGB >= 192 { return 20_000 }
        if physicalMemoryGB >= 96  { return 10_000 }
        if physicalMemoryGB >= 48  { return 5_000  }
        if physicalMemoryGB >= 24  { return 3_000  }
        return 2_000
    }()
}
