import Foundation
import Testing
import FileIDShared
@testable import FileID

@MainActor
@Suite("Scan lifecycle in the engine client")
struct ScanLifecycleTests {
    private func progress(_ phase: ScanPhase, session: String = "s") -> IPCEvent {
        IPCEvent(payload: .progress(ScanProgress(sessionID: session, phase: phase, total: 10, discovered: 10, processed: 4, failed: 0,
                                                 filesPerSecond: 1, etaSeconds: nil, residentMB: 1, availableMB: 1)))
    }

    @Test func aScanTheEngineRejectsBeforeAnyProgressReleasesTheSleepToken() {
        for ended in [ScanPhase.failed, .cancelled, .completed] {
            let engine = EngineClient()
            engine.beginScanActivity()
            #expect(engine.scanActivityHeld)
            engine.handleEvent(IPCEvent(payload: .phaseChanged(ended)))
            #expect(!engine.scanActivityHeld)
        }
    }

    @Test func aRejectedRescanAfterAFinishedScanReleasesTheSleepToken() {
        let engine = EngineClient()
        engine.handleEvent(progress(.completed))
        engine.beginScanActivity()
        engine.handleEvent(IPCEvent(payload: .phaseChanged(.failed)))
        #expect(!engine.scanActivityHeld)
    }

    @Test func aScanThatIsStillRunningKeepsTheSleepToken() {
        let engine = EngineClient()
        engine.beginScanActivity()
        engine.handleEvent(progress(.tagging))
        engine.handleEvent(IPCEvent(payload: .phaseChanged(.tagging)))
        #expect(engine.scanActivityHeld)
        engine.handleEvent(IPCEvent(payload: .phaseChanged(.cancelled)))
        #expect(!engine.scanActivityHeld)
    }

    @Test func aFolderWaitingForTheOldScanStartsOnlyWhenTheOldScanEnds() {
        let engine = EngineClient()
        engine.handleEvent(progress(.tagging))
        engine.setPendingScanRootForTesting(URL(fileURLWithPath: "/tmp/fileid-next"))
        engine.handleEvent(progress(.tagging))
        #expect(engine.pendingScanRoot != nil)
        engine.handleEvent(IPCEvent(payload: .phaseChanged(.cancelled)))
        #expect(engine.pendingScanRoot == nil)
    }
}
