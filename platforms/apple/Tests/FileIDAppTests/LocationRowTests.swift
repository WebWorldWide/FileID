import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Location rows")
struct LocationRowTests {
    private func root(files: Int64 = 1200, bytes: Int64 = 3_400_000_000, scanned: Double? = 1_700_000_000, online: Bool = true) -> LibraryRoot {
        LibraryRoot(id: 1, path: "/Volumes/Photos", display: "Photos", kind: "external", enabled: true, online: online,
                    fileCount: files, totalBytes: bytes, lastScanAt: scanned, lastStatus: "completed")
    }

    @Test func summaryNamesCountSizeAndLastScan() {
        let now = Date(timeIntervalSince1970: 1_700_000_000 + 3 * 86_400)
        let text = LocationText.summary(root(), now: now)
        #expect(text.hasPrefix("1,200 files · "))
        #expect(text.contains("3.4 GB") || text.contains("3,4 GB"))
        #expect(text.hasSuffix("3 days ago") || text.contains("ago"))
        #expect(text.contains("scanned"))
    }

    @Test func singularFileAndNeverScannedRead() {
        let text = LocationText.summary(root(files: 1, bytes: 2_048, scanned: nil))
        #expect(text.hasPrefix("1 file · "))
        #expect(text.hasSuffix("not scanned yet"))
    }
}
