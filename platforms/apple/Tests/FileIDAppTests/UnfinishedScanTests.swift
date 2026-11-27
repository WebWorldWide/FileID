import Foundation
import Testing
@testable import FileID

@Suite("A scan that stopped before it finished")
struct UnfinishedScanTests {
    private func session(_ root: String, _ status: String, at second: Double) -> ReadStore.ScanSessionRow {
        ReadStore.ScanSessionRow(
            id: "\(root)-\(second)", rootPath: root, startedAt: Date(timeIntervalSince1970: second),
            completedAt: nil, lastFileIndex: nil, totalFiles: nil, status: status)
    }

    private let folder = URL(fileURLWithPath: "/Users/me/Photos")

    @Test("a cancelled or failed newest scan of this folder is unfinished")
    func interruptedScanIsFound() {
        for status in ["cancelled", "failed"] {
            let found = PipelineBlock.unfinishedScan(in: [session("/Users/me/Photos", status, at: 20), session("/Users/me/Photos", "completed", at: 10)], root: folder)
            #expect(found?.status == status)
        }
    }

    @Test("a finished newest scan, a running one, or another folder's scan is not")
    func finishedScansAreNotFlagged() {
        #expect(PipelineBlock.unfinishedScan(in: [session("/Users/me/Photos", "completed", at: 20), session("/Users/me/Photos", "failed", at: 10)], root: folder) == nil)
        #expect(PipelineBlock.unfinishedScan(in: [session("/Users/me/Photos", "running", at: 20)], root: folder) == nil)
        #expect(PipelineBlock.unfinishedScan(in: [session("/Users/me/Other", "failed", at: 20)], root: folder) == nil)
        #expect(PipelineBlock.unfinishedScan(in: [], root: folder) == nil)
    }

    @Test("the folder match ignores a trailing slash")
    func trailingSlashStillMatches() {
        #expect(PipelineBlock.unfinishedScan(in: [session("/Users/me/Photos/", "cancelled", at: 5)], root: folder) != nil)
    }

    @Test("the sentence says how many files were read and how to read the rest")
    func detailWording() {
        #expect(PipelineBlock.unfinishedScanDetail(filesRead: 45) == "45 files read and ready to browse. Press Scan again to finish adding tags.")
        #expect(PipelineBlock.unfinishedScanDetail(filesRead: 1).hasPrefix("1 file read and ready"))
        #expect(PipelineBlock.unfinishedScanDetail(filesRead: 0).hasPrefix("Press Scan again to read the rest"))
    }

    @Test("a library with files in it says tags are being added, not that the scan failed")
    func titleForReadFiles() {
        #expect(PipelineBlock.unfinishedScanTitle(filesRead: 130_108) == "Tags aren’t finished yet")
        #expect(PipelineBlock.unfinishedScanTitle(filesRead: 0) == "The scan didn’t finish")
    }
}
