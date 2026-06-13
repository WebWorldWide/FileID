// Worker-cap topology tests. The formula is factored into a pure function so
// a known CPU topology can be supplied here regardless of the CI host's real
// hardware. (F-C3-045)
import Testing
import Foundation
@testable import FileIDEngine

@Suite("Hardware — worker cap")
struct HardwareWorkerCapTests {

    @Test("Intel 6C/12T fallback uses the physical-core target, no SMT double-count")
    func intelNoSMTDoubleCount() {
        // Intel: no perf levels. P from hw.physicalcpu = 6, E = 0, logical = 12.
        // 6 + 0 + max(1,3) = 9, clamped to logical 12 → 9. The old fallback
        // counted SMT siblings as P-cores (P=12 → 12+6 = 18 ≈ 1.5x the cap a
        // 6C/12T Mac can feed).
        let cap = Hardware.computeWorkerCap(performanceCores: 6, efficiencyCores: 0,
                                            logicalCores: 12, hasPerformanceLevels: false)
        #expect(cap == 9)
    }

    @Test("Intel fallback clamps a degenerate SMT-doubled P-count to logical")
    func intelDegradedClampsToLogical() {
        // If physical detection fails and P degrades to the logical count (12),
        // the Windows-style logical clamp must still bound the pool — never 18.
        let cap = Hardware.computeWorkerCap(performanceCores: 12, efficiencyCores: 0,
                                            logicalCores: 12, hasPerformanceLevels: false)
        #expect(cap == 12)
    }

    @Test("Apple Silicon M1 Pro keeps the tuned 14 (oversubscription not clamped)")
    func appleSiliconKeepsTunedCap() {
        // Exact P/E split → the +P/2 oversubscription is the tuned sweet spot
        // and is intentionally left above the 10 physical cores.
        let cap = Hardware.computeWorkerCap(performanceCores: 8, efficiencyCores: 2,
                                            logicalCores: 10, hasPerformanceLevels: true)
        #expect(cap == 14)
    }

    @Test("Mac Studio Ultra caps at 32")
    func ultraClampedTo32() {
        let cap = Hardware.computeWorkerCap(performanceCores: 16, efficiencyCores: 8,
                                            logicalCores: 24, hasPerformanceLevels: true)
        #expect(cap == 32)
    }

    @Test("cap is bounded to [4, 32]")
    func capBounds() {
        #expect(Hardware.computeWorkerCap(performanceCores: 1, efficiencyCores: 0,
                                          logicalCores: 1, hasPerformanceLevels: true) == 4)
        #expect(Hardware.computeWorkerCap(performanceCores: 64, efficiencyCores: 64,
                                          logicalCores: 256, hasPerformanceLevels: true) == 32)
    }

    @Test("the live worker cap is sane on whatever host runs CI")
    func liveCapSane() {
        // Real-hardware sanity: never below the floor, never above the ceiling,
        // and never above the logical thread count by more than the tuned
        // Apple-Silicon oversubscription headroom.
        #expect(Hardware.workerCap >= 4)
        #expect(Hardware.workerCap <= 32)
        if !Hardware.hasPerformanceLevels {
            #expect(Hardware.workerCap <= Hardware.logicalCoreCount,
                    "Intel/SMT host must never oversubscribe logical threads")
        }
    }
}
