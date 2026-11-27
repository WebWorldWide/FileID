import Foundation
import FileIDShared
import Testing
@testable import FileID

@Suite("Helper-down gating and library counters")
struct HelperGateTests {
    private let ready = EngineClient.ConnectionState.ready(EngineInfo(version: "test", pid: 1, workerCap: 1, physicalMemoryGB: 16))

    @Test func onlyACrashedHelperIsDown() {
        #expect(HelperGate.isDown(.crashed(reason: "gone")))
        #expect(!HelperGate.isDown(.starting))
        #expect(!HelperGate.isDown(ready))
    }

    @Test func scanAndRescanAreDisabledWhileTheHelperIsDown() {
        #expect(HelperGate.scanDisabled(startRequested: false, helperDown: true))
        #expect(!HelperGate.scanDisabled(startRequested: false, helperDown: false))
        #expect(HelperGate.scanDisabled(startRequested: true, helperDown: false))
        #expect(HelperGate.rescanDisabled(startRequested: false, captionsRunning: false, faceClusteringInFlight: false, helperDown: true))
        #expect(!HelperGate.rescanDisabled(startRequested: false, captionsRunning: false, faceClusteringInFlight: false, helperDown: false))
        #expect(HelperGate.rescanDisabled(startRequested: false, captionsRunning: true, faceClusteringInFlight: false, helperDown: false))
        #expect(HelperGate.rescanDisabled(startRequested: false, captionsRunning: false, faceClusteringInFlight: true, helperDown: false))
    }

    @Test func anUnsentRequestSaysTheHelperStoppedInsteadOfPromisingItWillBeReady() {
        #expect(HelperGate.unsentNote(helperDown: true) == "FileID’s helper stopped. Restart it, then try again.")
        #expect(HelperGate.unsentNote(helperDown: false) == "FileID is still getting ready. Try again in a moment.")
    }

    @Test func noPageHardCodesTheGettingReadyNoteForAnUnsentRequest() throws {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let sources = root.appendingPathComponent("platforms/apple/app/Sources/FileID")
        let files = try #require(FileManager.default.enumerator(at: sources, includingPropertiesForKeys: nil))
        var hardCoded: [String] = []
        for case let url as URL in files where url.pathExtension == "swift" && url.lastPathComponent != "HelperGate.swift" {
            if try String(contentsOf: url, encoding: .utf8).contains("FileID is still getting ready. Try again in a moment.") { hardCoded.append(url.lastPathComponent) }
        }
        #expect(hardCoded == ["RestructureView.swift"], "A request the helper never received must use HelperGate.unsentNote so a stopped helper is not told to wait: \(hardCoded)")
    }

    @Test func titleBarPillSaysHelperStoppedOnlyWhenDown() {
        #expect(HelperGate.pillLabel(for: .crashed(reason: "gone")) == "Helper stopped")
        #expect(HelperGate.pillLabel(for: .starting) == nil)
        #expect(HelperGate.pillLabel(for: ready) == nil)
    }

    @Test func countersRefreshOnlyWhenTheUnfilteredListingOutrunsThem() {
        #expect(LibraryCounters.needsRefresh(isSimilaritySeed: false, kind: nil, query: "", listedRows: 120, scopedTotalFiles: 100))
        #expect(!LibraryCounters.needsRefresh(isSimilaritySeed: false, kind: nil, query: "", listedRows: 100, scopedTotalFiles: 100))
        #expect(!LibraryCounters.needsRefresh(isSimilaritySeed: false, kind: nil, query: "", listedRows: 90, scopedTotalFiles: 100))
        #expect(!LibraryCounters.needsRefresh(isSimilaritySeed: true, kind: nil, query: "", listedRows: 120, scopedTotalFiles: 100))
        #expect(!LibraryCounters.needsRefresh(isSimilaritySeed: false, kind: "image", query: "", listedRows: 120, scopedTotalFiles: 100))
        #expect(!LibraryCounters.needsRefresh(isSimilaritySeed: false, kind: nil, query: "beach", listedRows: 120, scopedTotalFiles: 100))
    }

    @Test func modelDownloadsStayOffWhileTheHelperIsDownAndSayWhy() {
        #expect(HelperGate.downloadsUnavailableNote.contains("helper isn’t running"))
        #expect(HelperGate.downloadsUnavailableNote.contains("Restart the helper"))
    }

    @Test func welcomeDownloadButtonsAreOffWhileTheHelperIsDownOrTheyAlreadyRan() {
        #expect(HelperGate.downloadDisabled(inProgress: false, helperDown: true))
        #expect(HelperGate.downloadDisabled(inProgress: true, helperDown: false))
        #expect(!HelperGate.downloadDisabled(inProgress: false, helperDown: false))
    }

    @Test func oneModelReadsTheSameSizeOnEveryScreen() {
        let qwen8B: Int64 = 5_780_000_000
        #expect(ModelSize.label(qwen8B) == ByteCountFormatter.string(fromByteCount: qwen8B, countStyle: .file))
        #expect(ModelSize.label(qwen8B) == "5.78 GB")
        #expect(ModelSize.downloadBadge(qwen8B) == "5.78 GB download")
        #expect(!ModelSize.label(qwen8B).contains("5.4"), "5.78 GB is 5.4 GiB; the old badge labelled GiB as GB")
    }

    @Test func aMissingDescriptionModelIsAFailureTheDescribePageShows() {
        let error = EngineError(kind: "vlm_model_missing", message: "The qwen3_vl_2b model isn't installed yet.", modelKind: "qwen3_vl_2b")
        #expect(ModelFailureText.isModelFailure(error))
        #expect(ModelFailureText.plain(error) == "This AI model isn’t downloaded yet. Try again to download it first.")
    }

    @MainActor
    @Test func photoExportsOfferOnlyJPEGAndPNGAndRAWCanNotBeChosen() {
        let session = ToolsSession()
        session.kind = "photo"
        func hit(_ id: Int64, _ path: String) -> CatalogHit { CatalogHit(fileID: id, path: path, kind: "image", text: "") }
        let raw = hit(1, "/p/DSC01776.ARW"), jpeg = hit(2, "/p/DSC01776.JPG"), png = hit(3, "/p/a.png"), heic = hit(4, "/p/b.heic")
        session.allHits = [raw, jpeg, png, heic]
        session.applyKindFilter()
        #expect(session.hits.map(\.fileID) == [1, 2, 3, 4])
        #expect(session.chooseableHits.map(\.fileID) == [2, 3])
        #expect(!session.canChoose(raw) && !session.canChoose(heic))
        session.selection = [1, 2]
        session.applyKindFilter()
        #expect(session.selection == [2])
        session.kind = "video"
        #expect(session.canChoose(hit(5, "/p/c.mov")))
    }
}
