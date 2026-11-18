// Hardware tier detection: the worker-cap formula and memory readouts.
import Foundation
import Darwin

public enum Hardware {

    public static let physicalMemoryGB: Double = {
        Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824.0
    }()

    public static let coreCount: Int = ProcessInfo.processInfo.processorCount

    /// Logical thread count, including Hyper-Threading siblings on Intel. On Apple Silicon it equals the physical core count.
    public static let logicalCoreCount: Int = max(1, ProcessInfo.processInfo.activeProcessorCount)

    /// True when the kernel exposes Apple Silicon's P/E split (hw.perflevelN.*). On Intel the physical count is derived and the logical clamp applies.
    package static let hasPerformanceLevels: Bool = {
        var size: size_t = MemoryLayout<Int32>.size
        var value: Int32 = 0
        return sysctlbyname("hw.perflevel0.physicalcpu", &value, &size, nil, 0) == 0
    }()

    public static let performanceCoreCount: Int = {
        var size: size_t = MemoryLayout<Int32>.size
        var value: Int32 = 0
        if sysctlbyname("hw.perflevel0.physicalcpu", &value, &size, nil, 0) == 0 {
            return Int(value)
        }
        // Intel has no perf levels: use the physical count (hw.physicalcpu). activeProcessorCount counts Hyper-Threading siblings and sized the pool ~1.5x too large.
        var phys: Int32 = 0
        var psize: size_t = MemoryLayout<Int32>.size
        if sysctlbyname("hw.physicalcpu", &phys, &psize, nil, 0) == 0, phys > 0 {
            return Int(phys)
        }
        // Last resort: the logical count, bounded by workerCap's clamp.
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

    /// Worker count for the tagging stage: P + E + max(1, P/2), capped at 32. M1 Pro (8P + 2E) → 14; Mac Studio Ultra (16P + 8E) → 32.
    /// Raising M1 Pro from 14 to 18 saturated a NAS share (read latency 2 ms → 1.5 s), so 14 is the sweet spot for this tier.
    /// Default ONNX/CoreML model inference concurrency dynamically scaled
    /// to Apple Neural Engine (ANE) / GPU performance core count (4..12).
    public static let defaultInferenceConcurrency: Int = {
        max(4, min(12, performanceCoreCount))
    }()

    public static let workerCap: Int = computeWorkerCap(
        performanceCores: performanceCoreCount,
        efficiencyCores: efficiencyCoreCount,
        logicalCores: logicalCoreCount,
        hasPerformanceLevels: hasPerformanceLevels)

    /// Pure worker-cap formula, injectable for tests. P/E are physical counts; `logicalCores` includes SMT siblings.
    /// Apple Silicon keeps `+P/2` above the physical core count (the tuned sweet spot). Intel's generic counts are clamped to logical threads, mirroring the Windows CpuTopology clamp.
    package static func computeWorkerCap(performanceCores: Int, efficiencyCores: Int,
                                 logicalCores: Int, hasPerformanceLevels: Bool) -> Int {
        let p = max(1, performanceCores)
        let e = max(0, efficiencyCores)
        let computed = p + e + max(1, p / 2)
        let bounded = hasPerformanceLevels ? computed : min(computed, max(1, logicalCores))
        return min(32, max(4, bounded))
    }

    /// Coarse RAM tier for sizing in-memory budgets. Mirrors the Windows engine's `memory_tier`. Static at startup; runtime memory-pressure adaptation is not implemented.
    public enum MemoryTier: Sendable { case low, balanced, high }

    public static let memoryTier: MemoryTier = {
        let gb = physicalMemoryGB
        if gb < 12 { return .low }
        if gb < 48 { return .balanced }
        return .high
    }()

    /// Resident-set MB of the current process.
    public static func residentMB() -> Int {
        // proc_pidinfo(getpid(), …) instead of the Darwin mach_task_self_ global, which Swift 6 on the Xcode 16 SDK rejects as shared mutable state.
        var info = proc_taskinfo()
        let size = Int32(MemoryLayout<proc_taskinfo>.size)
        let got = proc_pidinfo(getpid(), PROC_PIDTASKINFO, 0, &info, size)
        guard got == size else { return 0 }
        return Int(info.pti_resident_size / (1024 * 1024))
    }

    /// Available system memory in MB (free + inactive pages).
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
        let reclaimable = Int(stats.free_count) + Int(stats.inactive_count)
            + Int(stats.speculative_count) + Int(stats.purgeable_count)
        let counted = reclaimable * pageSize / (1024 * 1024)
        // The kernel's availability level (what memory_pressure reports) also credits compressible and file-backed memory.
        var level: Int32 = 0
        var size = MemoryLayout<Int32>.size
        guard sysctlbyname("kern.memorystatus_level", &level, &size, nil, 0) == 0, (1...100).contains(level) else { return counted }
        let kernelMB = Int(ProcessInfo.processInfo.physicalMemory / 1_048_576) * Int(level) / 100
        return max(counted, kernelMB)
    }
}
