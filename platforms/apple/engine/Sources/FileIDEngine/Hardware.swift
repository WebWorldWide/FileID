// Hardware tier detection. Ported from v1 Sources/Services/Hardware.swift, kept
// minimal for M1 — the worker-cap formula and memory readouts. Will grow as
// later milestones add per-tier thumbnail caches, Deep Analyze throttle, etc.
import Foundation
import Darwin

public enum Hardware {

    public static let physicalMemoryGB: Double = {
        Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824.0
    }()

    public static let coreCount: Int = ProcessInfo.processInfo.processorCount

    /// Logical thread count (Intel Hyper-Threading siblings included). On
    /// Apple Silicon this equals the physical core count — there is no SMT.
    public static let logicalCoreCount: Int = max(1, ProcessInfo.processInfo.activeProcessorCount)

    /// True when the kernel exposes Apple Silicon's P/E split (hw.perflevelN.*).
    /// False on Intel Macs, where we derive a physical core count and lean on
    /// the logical-core clamp instead. (F-C3-045)
    static let hasPerformanceLevels: Bool = {
        var size: size_t = MemoryLayout<Int32>.size
        var value: Int32 = 0
        return sysctlbyname("hw.perflevel0.physicalcpu", &value, &size, nil, 0) == 0
    }()

    public static let performanceCoreCount: Int = {
        // sysctl hw.perflevel0.physicalcpu — Apple Silicon P-cores.
        var size: size_t = MemoryLayout<Int32>.size
        var value: Int32 = 0
        if sysctlbyname("hw.perflevel0.physicalcpu", &value, &size, nil, 0) == 0 {
            return Int(value)
        }
        // Intel Macs / pre-M1: no perf levels. Use the PHYSICAL core count
        // (hw.physicalcpu) — NOT activeProcessorCount, which counts each
        // Hyper-Threading sibling as a core and doubled the P-count, sizing the
        // worker pool ~1.5x too large on every Intel Mac (F-C3-045).
        var phys: Int32 = 0
        var psize: size_t = MemoryLayout<Int32>.size
        if sysctlbyname("hw.physicalcpu", &phys, &psize, nil, 0) == 0, phys > 0 {
            return Int(phys)
        }
        // Last resort: logical count, which workerCap's clamp then bounds.
        return logicalCoreCount
    }()

    public static let efficiencyCoreCount: Int = {
        var size: size_t = MemoryLayout<Int32>.size
        var value: Int32 = 0
        if sysctlbyname("hw.perflevel1.physicalcpu", &value, &size, nil, 0) == 0 {
            return Int(value)
        }
        return 0
    }()

    /// Worker count for the tagging stage.
    ///
    /// Formula: P + E + max(1, P/2). On M1 Pro (8P + 2E) → 14. Mac Studio
    /// Ultra (16P + 8E) → 32. Capped at 32.
    ///
    /// Iteration 8 finding: bumping workers from 14 → 18 caused NAS read
    /// times to spike from 2 ms → 1510 ms (saturated network share). 14 is
    /// the sweet spot for this hardware tier — keeps ANE fed without
    /// overwhelming the file source. Faster local SSDs may benefit from
    /// more workers; revisit per-storage tier if needed.
    public static let workerCap: Int = computeWorkerCap(
        performanceCores: performanceCoreCount,
        efficiencyCores: efficiencyCoreCount,
        logicalCores: logicalCoreCount,
        hasPerformanceLevels: hasPerformanceLevels)

    /// Pure worker-cap formula (factored out so the topology is injectable in
    /// tests). `performanceCores`/`efficiencyCores` are PHYSICAL counts;
    /// `logicalCores` includes SMT siblings.
    ///
    /// On Apple Silicon (exact P/E split, `hasPerformanceLevels == true`) the
    /// `+P/2` oversubscription is the tuned sweet spot — M1 Pro 8P+2E → 14 —
    /// and is deliberately left above the 10 physical cores. On Intel (no perf
    /// levels) the counts are generic, so the pool is clamped to logical
    /// threads, mirroring the Windows `CpuTopology::worker_cap` clamp so a
    /// fallback mis-count can never oversubscribe SMT. (F-C3-045)
    static func computeWorkerCap(performanceCores: Int, efficiencyCores: Int,
                                 logicalCores: Int, hasPerformanceLevels: Bool) -> Int {
        let p = max(1, performanceCores)
        let e = max(0, efficiencyCores)
        let computed = p + e + max(1, p / 2)
        let bounded = hasPerformanceLevels ? computed : min(computed, max(1, logicalCores))
        return min(32, max(4, bounded))
    }

    /// Resident-set MB of the current process.
    public static func residentMB() -> Int {
        var info = mach_task_basic_info()
        var count = mach_msg_type_number_t(MemoryLayout<mach_task_basic_info>.size / MemoryLayout<integer_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(MACH_TASK_BASIC_INFO), $0, &count)
            }
        }
        guard kr == KERN_SUCCESS else { return 0 }
        return Int(info.resident_size / (1024 * 1024))
    }

    /// Available system memory in MB (free + inactive + speculative pages).
    public static func availableMemoryMB() -> Int {
        var stats = vm_statistics64()
        var count = mach_msg_type_number_t(MemoryLayout<vm_statistics64_data_t>.size / MemoryLayout<integer_t>.size)
        let kr = withUnsafeMutablePointer(to: &stats) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                host_statistics64(mach_host_self(), HOST_VM_INFO64, $0, &count)
            }
        }
        guard kr == KERN_SUCCESS else { return 0 }
        let pageSize = Int(getpagesize())
        let free = Int(stats.free_count) * pageSize
        let inactive = Int(stats.inactive_count) * pageSize
        let speculative = Int(stats.speculative_count) * pageSize
        return (free + inactive + speculative) / (1024 * 1024)
    }
}
