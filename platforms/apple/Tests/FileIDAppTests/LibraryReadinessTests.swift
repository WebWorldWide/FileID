import Foundation
import FileIDShared
import Testing
@testable import FileID

@Suite("Pages that need a scanned library")
struct LibraryReadinessTests {
    @Test("no catalogued files and no scan: Chat asks for a scan and stays closed")
    func needsScan() {
        for phase in [nil, .idle, .completed, .cancelled, .failed] as [ScanPhase?] {
            let readiness = LibraryReadiness(catalogedFiles: 0, scanPhase: phase)
            #expect(readiness == .needsScan, "\(String(describing: phase))")
            #expect(!readiness.hasFiles)
            #expect(readiness.chatComposerPlaceholder == "Scan a folder first")
        }
        #expect(LibraryReadiness.needsScan.chatTitle == "Scan a folder to start chatting")
        #expect(LibraryReadiness.needsScan.chatMessage(chosenFolder: nil).hasPrefix("Chat answers questions about the files FileID has scanned."))
    }

    @Test("a chosen but unscanned folder is named, and the button scans it; with no folder it asks for one")
    func chosenButUnscannedFolder() {
        #expect(LibraryReadiness.scanButtonTitle(chosenFolder: "Eckerts 10-4-26") == "Scan Eckerts 10-4-26")
        #expect(LibraryReadiness.scanButtonTitle(chosenFolder: nil) == "Choose a folder to scan")
        let chosen = LibraryReadiness.needsScan.chatMessage(chosenFolder: "Eckerts 10-4-26")
        #expect(chosen.contains("FileID hasn’t read Eckerts 10-4-26 yet."))
        #expect(chosen.hasPrefix("Chat answers questions about the files FileID has scanned."))
        let none = LibraryReadiness.needsScan.chatMessage(chosenFolder: nil)
        #expect(none.contains("Choose a folder") && !none.contains("hasn’t read"))
    }

    @Test("the first scan with nothing read yet waits, then opens on the first files")
    func firstScan() {
        for phase in [.discovering, .tagging, .postScan] as [ScanPhase] {
            let waiting = LibraryReadiness(catalogedFiles: 0, scanPhase: phase)
            #expect(waiting == .reading)
            #expect(!waiting.hasFiles)
            let open = LibraryReadiness(catalogedFiles: 1, scanPhase: phase)
            #expect(open == .readyWhileReading)
            #expect(open.hasFiles && open.chatComposerPlaceholder == nil)
            #expect(open.readingNote == "FileID is still reading your files. Answers cover the files read so far.")
        }
    }

    @Test("a scanned library is open with no note")
    func ready() {
        let readiness = LibraryReadiness(catalogedFiles: 1200, scanPhase: .completed)
        #expect(readiness == .ready && readiness.hasFiles)
        #expect(readiness.readingNote == nil && readiness.chatComposerPlaceholder == nil)
        #expect(LibraryReadiness(catalogedFiles: 5, scanPhase: nil) == .ready)
    }

    @Test("an unknown count (counters not loaded yet, or the reader failed) counts as a library with files, never as empty")
    func unknownCountIsReady() {
        for phase in [nil, .idle, .completed, .cancelled, .failed] as [ScanPhase?] {
            let readiness = LibraryReadiness(catalogedFiles: nil, scanPhase: phase)
            #expect(readiness == .ready, "\(String(describing: phase))")
            #expect(readiness.hasFiles && readiness.chatComposerPlaceholder == nil)
        }
        for phase in [.discovering, .tagging, .postScan] as [ScanPhase] {
            #expect(LibraryReadiness(catalogedFiles: nil, scanPhase: phase) == .readyWhileReading)
        }
        #expect(LibraryReadiness(catalogedFiles: 0, scanPhase: nil) == .needsScan)
    }
}

@Suite("Counter refresh during a scan")
struct ScanCounterThrottleTests {
    private let start = Date(timeIntervalSince1970: 1_000_000)

    private func ask(_ throttle: inout ScanCounterThrottle, _ phase: ScanPhase?, at seconds: Double) -> Bool {
        throttle.shouldRefresh(scanPhase: phase, now: start.addingTimeInterval(seconds))
    }

    @Test("refreshes at most once per interval while a scan runs")
    func throttles() {
        var throttle = ScanCounterThrottle()
        let asked = [ask(&throttle, .tagging, at: 0), ask(&throttle, .tagging, at: 0.5), ask(&throttle, .tagging, at: 1.99),
                     ask(&throttle, .tagging, at: 2), ask(&throttle, .postScan, at: 3)]
        #expect(asked == [true, false, false, true, false])
    }

    @Test("never refreshes when no scan is running, and the next scan refreshes at once")
    func idleAndRestart() {
        var throttle = ScanCounterThrottle()
        #expect(ask(&throttle, .discovering, at: 0))
        let idle = ([nil, .idle, .completed, .cancelled, .failed] as [ScanPhase?]).map { ask(&throttle, $0, at: 0.1) }
        #expect(idle == [false, false, false, false, false])
        #expect(ask(&throttle, .discovering, at: 0.2))
    }
}
