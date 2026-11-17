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
        #expect(LibraryReadiness.needsScan.chatMessage.hasPrefix("Chat answers questions about the files FileID has scanned."))
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
}
