import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Offline locations")
struct OfflineRootsTests {
    private func root(_ id: Int64, _ path: String, online: Bool) -> LibraryRoot {
        LibraryRoot(id: id, path: path, display: path, kind: "external", enabled: true, online: online,
                    fileCount: 1, totalBytes: 1, lastScanAt: nil, lastStatus: nil)
    }

    @Test func filesUnderAnOfflineRootAreOfflineAndNeighboursAreNot() {
        let roots = OfflineRoots([root(1, "/Volumes/Camera", online: false), root(2, "/Users/me/Pictures", online: true)])
        #expect(roots.isOffline("/Volumes/Camera/2024/a.jpg"))
        #expect(roots.isOffline("/Volumes/Camera"))
        #expect(!roots.isOffline("/Volumes/Camera2/a.jpg"))
        #expect(!roots.isOffline("/Volumes/Cameras/a.jpg"))
        #expect(!roots.isOffline("/Users/me/Pictures/b.jpg"))
        #expect(!roots.isOffline("/elsewhere/c.jpg"))
    }

    @Test func theMostSpecificLocationDecides() {
        let roots = OfflineRoots([root(1, "/Volumes/NAS", online: false), root(2, "/Volumes/NAS/Synced", online: true)])
        #expect(roots.isOffline("/Volumes/NAS/Old/a.jpg"))
        #expect(!roots.isOffline("/Volumes/NAS/Synced/a.jpg"))
        let flipped = OfflineRoots([root(1, "/Volumes/NAS", online: true), root(2, "/Volumes/NAS/Gone/", online: false)])
        #expect(flipped.isOffline("/Volumes/NAS/Gone/a.jpg"))
        #expect(!flipped.isOffline("/Volumes/NAS/Here/a.jpg"))
    }

    @Test func allOnlineMeansNothingToCheck() {
        let roots = OfflineRoots([root(1, "/Volumes/Camera", online: true)])
        #expect(roots.isEmpty)
        #expect(roots == .none)
        #expect(!roots.isOffline("/Volumes/Camera/a.jpg"))
        #expect(OfflineRoots([]).isEmpty)
    }
}

@Suite("Capture date window")
struct CaptureWindowTests {
    private var utc: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }

    private func day(_ y: Int, _ m: Int, _ d: Int, hour: Int = 15) -> Date {
        utc.date(from: DateComponents(year: y, month: m, day: d, hour: hour))!
    }

    @Test func emptyWindowSendsNoBounds() {
        let window = CaptureWindow()
        #expect(!window.isActive)
        let request = window.searchRequest(id: "r", query: "beach", calendar: utc)
        #expect(request.capturedFrom == nil && request.capturedTo == nil)
        #expect(request.action == "search" && request.query == "beach")
        #expect(window.summary(calendar: utc, locale: Locale(identifier: "en_US")) == "Any date")
    }

    @Test func boundsCoverWholeDays() {
        let window = CaptureWindow(from: day(2024, 3, 3), to: day(2024, 3, 9))
        let bounds = window.bounds(calendar: utc)
        #expect(bounds.from == day(2024, 3, 3, hour: 0).timeIntervalSince1970)
        #expect(bounds.to == day(2024, 3, 10, hour: 0).timeIntervalSince1970 - 0.001)
        let request = window.searchRequest(id: "r", query: "beach", calendar: utc)
        #expect(request.capturedFrom == bounds.from && request.capturedTo == bounds.to)
    }

    @Test func openEndedAndBackwardsWindowsStayValid() {
        let onlyFrom = CaptureWindow(from: day(2024, 3, 3), to: nil).bounds(calendar: utc)
        #expect(onlyFrom.from == day(2024, 3, 3, hour: 0).timeIntervalSince1970 && onlyFrom.to == nil)
        let onlyTo = CaptureWindow(from: nil, to: day(2024, 3, 3)).bounds(calendar: utc)
        #expect(onlyTo.from == nil && onlyTo.to == day(2024, 3, 4, hour: 0).timeIntervalSince1970 - 0.001)
        let backwards = CaptureWindow(from: day(2024, 3, 9), to: day(2024, 3, 3)).bounds(calendar: utc)
        #expect(backwards.from! <= backwards.to!)
        let old = CaptureWindow(from: day(1960, 1, 1), to: day(1965, 1, 1)).bounds(calendar: utc)
        #expect(old.from == 0 && old.to == 0)
    }

    @Test func summaryReadsTheRange() {
        let en = Locale(identifier: "en_US")
        #expect(CaptureWindow(from: day(2024, 3, 3), to: nil).summary(calendar: utc, locale: en) == "From Mar 3, 2024")
        #expect(CaptureWindow(from: nil, to: day(2024, 3, 3)).summary(calendar: utc, locale: en) == "Until Mar 3, 2024")
        #expect(CaptureWindow(from: day(2024, 3, 3), to: day(2024, 3, 9)).summary(calendar: utc, locale: en) == "Mar 3, 2024 – Mar 9, 2024")
        #expect(CaptureWindow(from: day(2024, 3, 3, hour: 1), to: day(2024, 3, 3)).summary(calendar: utc, locale: en) == "Mar 3, 2024")
    }
}
