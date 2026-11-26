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

    @Test("the work left is the owed and done counts the engine sends, while the scan's own counts stay the library size")
    func workIsTheEnginesCounts() {
        let started = progress(total: 130_718, discovered: 130_718, processed: 130_718, owed: 78_539, done: 0).postScanWork
        #expect(started == PostScanWork(owed: 78_539, done: 0))
        let later = progress(total: 130_718, discovered: 130_718, processed: 130_718, owed: 78_539, done: 20_000).postScanWork
        #expect(later == PostScanWork(owed: 78_539, done: 20_000))
        #expect(later?.left == 58_539)
    }

    @Test("a snapshot with nothing owed, or no counts at all, has no post-scan work")
    func noWorkWithoutCounts() {
        #expect(progress(total: 100, discovered: 100, processed: 100).postScanWork == nil)
        #expect(progress(total: 100, discovered: 100, processed: 100, owed: 0, done: 0).postScanWork == nil)
        #expect(progress(total: 100, discovered: 100, processed: 100, owed: 5, done: 9).postScanWork == PostScanWork(owed: 5, done: 5))
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

    @Test("the baseline is the files the scan read, from the first post-scan snapshot of the session")
    func baselineIsTheFilesRead() {
        let first = progress(total: 130_718, discovered: 130_718, processed: 130_718, owed: 78_539, done: 20_000)
        #expect(PostScanBaseline.observing(first, current: nil) == PostScanBaseline(sessionID: "s", filesRead: 130_718))
        #expect(PostScanBaseline.observing(first, current: PostScanBaseline(sessionID: "s", filesRead: 100))?.filesRead == 100)
        #expect(PostScanBaseline.observing(first, current: PostScanBaseline(sessionID: "other", filesRead: 100))?.filesRead == 130_718)
    }
}
