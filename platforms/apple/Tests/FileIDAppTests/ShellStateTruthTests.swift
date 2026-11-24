import Foundation
import Testing
import FileIDShared
@testable import FileID

@MainActor
@Suite("Shell states tell the truth")
struct ShellStateTruthTests {
    @Test func theTitleBarPillSaysPausedAndStaysStillWhenTheScanIsPaused() {
        let paused = TitleBar.scanPill(paused: true, phase: .discovering, processed: 0, total: 0)
        #expect(paused.label == "Paused" && paused.paused && paused.progress == nil)
        let running = TitleBar.scanPill(paused: false, phase: .tagging, processed: 5, total: 10)
        #expect(running.label == "Scanning" && !running.paused && running.progress == 0.5)
    }

    @Test func aTaggingScanWithNoTotalYetShowsNoPercentage() {
        #expect(TitleBar.scanPill(paused: false, phase: .tagging, processed: 0, total: 0).progress == nil)
    }

    @Test func startingAScanDropsThePreviousDescribeResult() {
        let engine = EngineClient()
        engine.handleEvent(IPCEvent(payload: .deepAnalyzeComplete(
            DeepAnalyzeComplete(processed: 120, failed: 0, totalSeconds: 5, modelKind: "m", cancelled: false))))
        #expect(engine.deepAnalyzeComplete != nil)
        engine.startScan(rootURL: URL(fileURLWithPath: "/tmp/fileid-scan"))
        #expect(engine.deepAnalyzeComplete == nil)
    }

    @Test func theFolderMenuRescanWaitsForAScanDescribeFaceGroupingAndAHelperThatIsDown() {
        #expect(HelperGate.folderMenuRescanDisabled(scanRunning: true, captionsRunning: false, faceClusteringInFlight: false, helperDown: false))
        #expect(HelperGate.folderMenuRescanDisabled(scanRunning: false, captionsRunning: true, faceClusteringInFlight: false, helperDown: false))
        #expect(HelperGate.folderMenuRescanDisabled(scanRunning: false, captionsRunning: false, faceClusteringInFlight: true, helperDown: false))
        #expect(HelperGate.folderMenuRescanDisabled(scanRunning: false, captionsRunning: false, faceClusteringInFlight: false, helperDown: true))
        #expect(!HelperGate.folderMenuRescanDisabled(scanRunning: false, captionsRunning: false, faceClusteringInFlight: false, helperDown: false))
    }
}
