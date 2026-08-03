import FileIDShared
import Testing
@testable import FileID

@Suite("Scan timing presentation")
@MainActor
struct ScanTimingPresentationTests {
    @Test("discovery reports a live count before ETA is knowable")
    func discoveryCount() {
        #expect(ProcessingControl.scanTimingText(progress(
            phase: .discovering,
            total: 0,
            discovered: 42
        )) == "Counting files — 42 found")
    }

    @Test("known work reports that the ETA is being estimated")
    func estimating() {
        #expect(ProcessingControl.scanTimingText(progress(
            phase: .tagging,
            total: 100,
            discovered: 100
        )) == "Tagging — estimating…")
    }

    @Test("known ETA is always visible with a stable duration")
    func knownETA() {
        #expect(ProcessingControl.scanTimingText(progress(
            phase: .tagging,
            total: 100,
            discovered: 100,
            etaSeconds: 125
        )) == "Tagging — 2m 5s left")
    }

    @Test("post-scan work has a useful estimate label")
    func postScanEstimate() {
        #expect(ProcessingControl.scanTimingText(progress(
            phase: .postScan,
            total: 0,
            discovered: 0
        )) == "Finishing up — estimating…")
    }

    private func progress(
        phase: ScanPhase,
        total: Int,
        discovered: Int,
        etaSeconds: Double? = nil
    ) -> ScanProgress {
        ScanProgress(
            sessionID: "test",
            phase: phase,
            total: total,
            discovered: discovered,
            processed: 0,
            failed: 0,
            filesPerSecond: 0,
            etaSeconds: etaSeconds,
            residentMB: 256,
            availableMB: 8_192
        )
    }
}
