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
        #expect(LocationText.summary(root(files: 1, bytes: 2_048, scanned: 1_700_000_000)).hasPrefix("1 file · "))
        let text = LocationText.summary(root(files: 0, bytes: 0, scanned: nil))
        #expect(text.hasPrefix("0 files · "))
        #expect(text.hasSuffix("not scanned yet"))
    }

    @Test func filesWithoutAScanStampSayTagsAreStillBeingAdded() {
        #expect(LocationText.summary(root(files: 130_108, scanned: nil)).hasSuffix("tags still being added"))
    }
}

@MainActor
@Suite struct LocationRescanGuardTests {
    private func progress(_ phase: ScanPhase) -> ScanProgress {
        ScanProgress(sessionID: "s", phase: phase, total: 400, discovered: 400, processed: 120, failed: 0,
                     filesPerSecond: 42, etaSeconds: 7, residentMB: 300, availableMB: 9000)
    }

    @Test func aRunningOrPausedScanIsInProgressAndAFinishedOneIsNot() {
        let engine = EngineClient()
        #expect(!engine.scanInProgress)
        engine.handleEvent(IPCEvent(payload: .progress(progress(.tagging))))
        #expect(engine.scanInProgress)
    }

    @Test func rescanIsDisabledWhileAScanRuns() throws {
        let source = try appSource("Views/LocationsCard.swift")
        #expect(source.contains(".disabled(!root.online || engine.scanInProgress)"))
    }
}
