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
        #expect(PipelineBlock.unfinishedScanDetail(filesRead: 45) == "45 files read so far. Press Scan again to read the rest.")
        #expect(PipelineBlock.unfinishedScanDetail(filesRead: 1).hasPrefix("1 file read so far"))
        #expect(PipelineBlock.unfinishedScanDetail(filesRead: 0).hasPrefix("Press Scan again to read the rest"))
    }
}
