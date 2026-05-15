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

    public static let performanceCoreCount: Int = {
        // sysctl hw.perflevel0.physicalcpu — Apple Silicon P-cores.
        var size: size_t = MemoryLayout<Int32>.size
        var value: Int32 = 0
        if sysctlbyname("hw.perflevel0.physicalcpu", &value, &size, nil, 0) == 0 {
            return Int(value)
        }
        // Fallback for Intel Macs / pre-M1 systems: assume all cores are P.
        return ProcessInfo.processInfo.activeProcessorCount
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
    public static let workerCap: Int = {
        let computed = performanceCoreCount + efficiencyCoreCount + max(1, performanceCoreCount / 2)
        return min(32, max(4, computed))
    }()

    /// V15.2.1: snapshot the kernel task handle once at static-init.
    /// `mach_task_self_` is declared as a global `var` in Darwin even
    /// though its value never changes for the process lifetime; Swift 6
    /// strict concurrency flags reads of a global `var` as
    /// "concurrency-unsafe shared mutable state". Caching as a `let`
    /// turns it into a Sendable constant and bypasses the diagnostic.
    nonisolated(unsafe) private static let cachedTaskSelf: task_t = mach_task_self_

    /// Resident-set MB of the current process.
    public static func residentMB() -> Int {
        var info = mach_task_basic_info()
        var count = mach_msg_type_number_t(MemoryLayout<mach_task_basic_info>.size / MemoryLayout<integer_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(cachedTaskSelf, task_flavor_t(MACH_TASK_BASIC_INFO), $0, &count)
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
