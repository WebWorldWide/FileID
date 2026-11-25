import Foundation
import FileIDShared
import Testing
@testable import FileID

@Suite("Post-scan progress in plain words")
struct PostScanProgressTests {
    private func progress(total: Int, discovered: Int, processed: Int, eta: Double? = nil, phase: ScanPhase = .postScan,
                          owed: Int? = nil, done: Int? = nil) -> ScanProgress {
        ScanProgress(sessionID: "s", phase: phase, total: total, discovered: discovered, processed: processed, failed: 0,
                     filesPerSecond: 8, etaSeconds: eta, residentMB: 0, availableMB: 0, postScanOwed: owed, postScanDone: done)
    }

    @Test("the shared engine counts post-scan work on top of the files, so the work left is measured from the files already read")
    func workIsMeasuredFromTheBaseline() {
        let baseline = PostScanBaseline(sessionID: "s", filesRead: 130_718)
        let started = progress(total: 209_257, discovered: 209_257, processed: 130_718).postScanWork(baseline: baseline)
        #expect(started == PostScanWork(owed: 78_539, done: 0))
        let later = progress(total: 209_257, discovered: 209_257, processed: 130_718 + 20_000).postScanWork(baseline: baseline)
        #expect(later == PostScanWork(owed: 78_539, done: 20_000))
        #expect(later?.left == 58_539)
    }

    @Test("without a baseline the files found stand in for it, and a baseline from another scan is ignored")
    func fallbackWithoutABaseline() {
        let lifted = progress(total: 150, discovered: 100, processed: 120)
        #expect(lifted.postScanWork(baseline: nil) == PostScanWork(owed: 50, done: 20))
        let other = PostScanBaseline(sessionID: "other", filesRead: 10)
        #expect(lifted.postScanWork(baseline: other) == PostScanWork(owed: 50, done: 20))
        #expect(progress(total: 100, discovered: 100, processed: 100).postScanWork(baseline: nil) == nil)
    }

    @Test("the sidebar says how many files are done, how many are owed and how long is left")
    func detailHasCountsAndTime() {
        let text = PipelineBlock.postScanDetail(PostScanWork(owed: 78_539, done: 20_000), etaSeconds: 7_200)
        #expect(text.hasPrefix("20,000 of 78,539 files"))
        #expect(text.contains("left"))
        #expect(PipelineBlock.postScanDetail(PostScanWork(owed: 10, done: 0), etaSeconds: nil) == "0 of 10 files")
    }

    @Test("a stop after every file was read says tags are what is left, not that the scan failed or never ran")
    func stoppedDuringPostScanWording() {
        let text = PipelineBlock.stoppedDuringPostScanDetail(filesRead: 130_718, left: 58_539)
        #expect(text.hasPrefix("130,718 files read and ready to browse."))
        #expect(text.contains("58,539 files still need tags"))
        #expect(!text.localizedCaseInsensitiveContains("didn’t finish"))
        #expect(!text.localizedCaseInsensitiveContains("not scanned"))
    }

    @Test("an app that joins mid post-scan reads the owed and done counts the engine sends, not a baseline from the snapshot it happened to see first")
    func lateJoinerUsesTheEnginesCounts() {
        let late = progress(total: 209_257, discovered: 209_257, processed: 130_718 + 20_000, owed: 78_539, done: 20_000)
        let wrongBaseline = PostScanBaseline(sessionID: "s", filesRead: 150_718)
        #expect(late.postScanWork(baseline: wrongBaseline) == PostScanWork(owed: 78_539, done: 20_000))
        #expect(late.postScanWork(baseline: nil) == PostScanWork(owed: 78_539, done: 20_000))
    }

    @Test("the baseline follows the engine's counts on every snapshot, and falls back to the first snapshot seen for an engine that sends none")
    func baselineFromCounts() {
        let late = progress(total: 209_257, discovered: 209_257, processed: 150_718, owed: 78_539, done: 20_000)
        let seen = PostScanBaseline.observing(late, current: PostScanBaseline(sessionID: "s", filesRead: 150_718))
        #expect(seen == PostScanBaseline(sessionID: "s", filesRead: 130_718))
        let plain = progress(total: 150, discovered: 100, processed: 120)
        #expect(PostScanBaseline.observing(plain, current: nil) == PostScanBaseline(sessionID: "s", filesRead: 120))
        #expect(PostScanBaseline.observing(plain, current: PostScanBaseline(sessionID: "s", filesRead: 100))?.filesRead == 100)
    }
}
