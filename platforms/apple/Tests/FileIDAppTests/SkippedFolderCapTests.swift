import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Skipped folders")
struct SkippedFolderCapTests {
    @Test func theSkippedFolderListStopsAtWhatTheEngineAccepts() {
        let many = (0..<150).map { "/Volumes/Drive/folder\($0)" }
        let kept = DeepAnalyzeSettings.sanitizeExcludedFolders(many)
        #expect(kept.count == 100)
        #expect(DeepAnalyzeSettings.maxExcludedFolders == 100)
    }
}
