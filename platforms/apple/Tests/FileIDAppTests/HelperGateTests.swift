import FileIDShared
import Testing
@testable import FileID

@Suite("Helper-down gating and library counters")
struct HelperGateTests {
    private let ready = EngineClient.ConnectionState.ready(EngineInfo(version: "test", pid: 1, workerCap: 1, physicalMemoryGB: 16))

    @Test func onlyACrashedHelperIsDown() {
        #expect(HelperGate.isDown(.crashed(reason: "gone")))
        #expect(!HelperGate.isDown(.starting))
        #expect(!HelperGate.isDown(ready))
    }

    @Test func scanAndRescanAreDisabledWhileTheHelperIsDown() {
        #expect(HelperGate.scanDisabled(startRequested: false, helperDown: true))
        #expect(!HelperGate.scanDisabled(startRequested: false, helperDown: false))
        #expect(HelperGate.scanDisabled(startRequested: true, helperDown: false))
        #expect(HelperGate.rescanDisabled(startRequested: false, captionsRunning: false, faceClusteringInFlight: false, helperDown: true))
        #expect(!HelperGate.rescanDisabled(startRequested: false, captionsRunning: false, faceClusteringInFlight: false, helperDown: false))
        #expect(HelperGate.rescanDisabled(startRequested: false, captionsRunning: true, faceClusteringInFlight: false, helperDown: false))
        #expect(HelperGate.rescanDisabled(startRequested: false, captionsRunning: false, faceClusteringInFlight: true, helperDown: false))
    }

    @Test func titleBarPillSaysHelperStoppedOnlyWhenDown() {
        #expect(HelperGate.pillLabel(for: .crashed(reason: "gone")) == "Helper stopped")
        #expect(HelperGate.pillLabel(for: .starting) == nil)
        #expect(HelperGate.pillLabel(for: ready) == nil)
    }

    @Test func countersRefreshOnlyWhenTheUnfilteredListingOutrunsThem() {
        #expect(LibraryCounters.needsRefresh(isSimilaritySeed: false, kind: nil, query: "", listedRows: 120, scopedTotalFiles: 100))
        #expect(!LibraryCounters.needsRefresh(isSimilaritySeed: false, kind: nil, query: "", listedRows: 100, scopedTotalFiles: 100))
        #expect(!LibraryCounters.needsRefresh(isSimilaritySeed: false, kind: nil, query: "", listedRows: 90, scopedTotalFiles: 100))
        #expect(!LibraryCounters.needsRefresh(isSimilaritySeed: true, kind: nil, query: "", listedRows: 120, scopedTotalFiles: 100))
        #expect(!LibraryCounters.needsRefresh(isSimilaritySeed: false, kind: "image", query: "", listedRows: 120, scopedTotalFiles: 100))
        #expect(!LibraryCounters.needsRefresh(isSimilaritySeed: false, kind: nil, query: "beach", listedRows: 120, scopedTotalFiles: 100))
    }
}
