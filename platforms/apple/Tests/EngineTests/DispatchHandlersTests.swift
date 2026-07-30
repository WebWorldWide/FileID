// F-C3-032 + F-C3-021-wiring regression for the engine command dispatcher.
//
// 032: startScan rejected for a db-unavailable engine must emit a scan-TERMINAL
// event (scanComplete), not only an error — otherwise the app's auto-pilot is
// stranded on "Scanning…" forever (it advances only on a scan-terminal event).
//
// 021-wiring: planRestructure / applyRestructure must call the engine butler
// (Restructure.proposeAll / Restructure.apply) and emit restructurePlan /
// restructureApplyResult — not the old not_implemented_yet error.
import Testing
import Foundation
@testable import FileIDEngine
import FileIDShared

@Suite("Engine dispatch handlers (F-C3-032/021)", .serialized)
struct DispatchHandlersTests {

    private func waitFor(_ needles: [Data], in cap: WireCapture,
                         timeout: TimeInterval = 10) async -> Data {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            let out = cap.bytes()
            if needles.allSatisfy({ out.range(of: $0) != nil }) { return out }
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        return cap.bytes()
    }

    @Test("startScan with no database emits a terminal scanComplete, not just an error")
    func startScanDbUnavailableEmitsTerminal() async throws {
        let cap = WireCapture()
        let sink = cap.sink
        let cmd = IPCCommand(payload: .startScan(
            rootPath: "/tmp/does-not-matter", rootDisplay: nil,
            rescan: false, excludedPaths: nil))

        await FileIDEngineMain.dispatch(cmd, coordinator: ScanCoordinator(),
                                        sink: sink, database: nil)

        let errNeedle = Data("\"db_unavailable\"".utf8)
        let doneNeedle = Data("\"scanComplete\"".utf8)
        let out = await waitFor([errNeedle, doneNeedle], in: cap)
        await cap.finish()

        #expect(out.range(of: errNeedle) != nil, "db_unavailable error must still be emitted")
        #expect(out.range(of: doneNeedle) != nil,
                "a scan-terminal event must follow so the app leaves the scanning state")
    }

    @Test("planRestructure / applyRestructure round-trip through the engine butler")
    func restructureRoundTrip() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDRestructure-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))

        let cap = WireCapture()
        let sink = cap.sink

        // Empty library → an empty (but real) plan. Proves the dead IPC is wired
        // to proposeAll instead of returning not_implemented_yet.
        await FileIDEngineMain.dispatch(
            IPCCommand(payload: .planRestructure(
                libraryRoot: tmp.path, supportsPagedPlans: false)),
            coordinator: ScanCoordinator(), sink: sink, database: db)
        let planNeedle = Data("\"restructurePlan\"".utf8)
        let notImpl = Data("\"not_implemented_yet\"".utf8)
        var out = await waitFor([planNeedle], in: cap)
        #expect(out.range(of: planNeedle) != nil, "planRestructure must emit a restructurePlan event")
        #expect(out.range(of: notImpl) == nil, "planRestructure must no longer be not_implemented_yet")

        // applyRestructure with no moves → a real (zero) result.
        await FileIDEngineMain.dispatch(
            IPCCommand(payload: .applyRestructure(
                libraryRoot: tmp.path, moves: [], useSymlinks: false, planID: nil)),
            coordinator: ScanCoordinator(), sink: sink, database: db)
        let applyNeedle = Data("\"restructureApplyResult\"".utf8)
        out = await waitFor([applyNeedle], in: cap)
        await cap.finish()

        #expect(out.range(of: applyNeedle) != nil,
                "applyRestructure must emit a restructureApplyResult event")
    }

    // Audit R3: the schema promises shortcutUndoToken "undo[es] only the
    // shortcut-mode run identified by this opaque token; never consume the
    // real-move undo journal" — but macOS has no shortcut/symlink-apply mode
    // at all, so there is no shortcut journal to replay. The engine must fail
    // closed (reject) instead of silently falling through to undoLast() and
    // replaying the real-move journal, which would violate that contract.
    @Test("undoRestructure with a shortcutUndoToken fails closed and doesn't wedge the reservation")
    func undoRestructureShortcutTokenFailsClosed() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUndoShortcutToken-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        let coordinator = ScanCoordinator()

        let cap = WireCapture()
        let sink = cap.sink

        await FileIDEngineMain.dispatch(
            IPCCommand(payload: .undoRestructure(
                libraryRoot: tmp.path, shortcutUndoToken: UUID().uuidString)),
            coordinator: coordinator, sink: sink, database: db)

        let errNeedle = Data("\"undo_restructure\"".utf8)
        let resultNeedle = Data("\"restructureApplyResult\"".utf8)
        let out = await waitFor([errNeedle, resultNeedle], in: cap)

        #expect(out.range(of: errNeedle) != nil,
                "a non-nil shortcutUndoToken must be rejected with an undo_restructure error")
        #expect(out.range(of: resultNeedle) != nil,
                "the rejection must still emit a terminal restructureApplyResult so " +
                "EngineClient's undoRestructureInFlight flag clears (audit R2-app) instead " +
                "of latching forever")

        // Exactly ONE terminal result must appear — if the rejection had fallen
        // through to undoLast() instead of returning early, THAT call would emit
        // a SECOND restructureApplyResult, which this catches.
        let text = String(decoding: out, as: UTF8.self)
        let resultCount = text.components(separatedBy: "\"restructureApplyResult\"").count - 1
        #expect(resultCount == 1,
                "undoLast() must not also have run and emitted its own result")

        // The reservation must not be wedged: a normal restructure command right
        // after must be accepted, not bounced with restructure_busy. Reusing the
        // SAME coordinator is the point — a leaked reservation from the rejected
        // undo would still be held here.
        await FileIDEngineMain.dispatch(
            IPCCommand(payload: .planRestructure(
                libraryRoot: tmp.path, supportsPagedPlans: false)),
            coordinator: coordinator, sink: sink, database: db)
        let planNeedle = Data("\"restructurePlan\"".utf8)
        let busyNeedle = Data("\"restructure_busy\"".utf8)
        let out2 = await waitFor([planNeedle], in: cap)
        await cap.finish()

        #expect(out2.range(of: planNeedle) != nil,
                "a normal plan request right after the rejection must go through")
        #expect(out2.range(of: busyNeedle) == nil,
                "the rejected undo must not have left a stale restructure reservation")
    }

    @Test("restructurePlan DTO maps proposals and rolls up category counts")
    func restructurePlanDTOMapping() throws {
        let proposals = [
            RestructureProposal(fileID: 1, oldPath: "/a/1.jpg",
                                newPath: "/lib/People/Mom/1.jpg", bucket: "People/Mom",
                                confidence: "auto", reason: "Named person: Mom"),
            RestructureProposal(fileID: 2, oldPath: "/a/2.jpg",
                                newPath: "/lib/People/Mom/2.jpg", bucket: "People/Mom",
                                confidence: "auto", reason: nil),
            RestructureProposal(fileID: 3, oldPath: "/a/3.pdf",
                                newPath: "/lib/Documents/3.pdf", bucket: "Documents",
                                confidence: "review", reason: "Document"),
        ]
        // Build the PlanResult the way proposeAll does — classify on the full set,
        // derive tiers + counts (no exemption here) — so the mapper sees real tiers.
        let folderClass = Restructure.classifyFolders(proposals)
        let tiers = Restructure.folderTiersAndCounts(classified: folderClass, exempt: [])
        let planResult = Restructure.PlanResult(
            proposals: proposals, tierByFolder: tiers.tierByFolder,
            anchorFolders: tiers.anchor, mixedFolders: tiers.mixed, junkFolders: tiers.junk)
        let plan = FileIDEngineMain.restructurePlan(from: planResult, libraryRoot: "/lib")

        #expect(plan.libraryRoot == "/lib")
        #expect(plan.moves.count == 3)
        let first = try #require(plan.moves.first)
        #expect(first.source == "/a/1.jpg")
        #expect(first.destination == "/lib/People/Mom/1.jpg")
        #expect(first.category == "People/Mom")
        #expect(first.confidence == "auto")
        // All three proposals live in "/a": 2× People/Mom + 1× Documents = 67%
        // homogeneity (< 80%) over 3 files → the source folder is Mixed.
        #expect(first.tier == "Mixed")
        // Counts: People/Mom=2 (most), Documents=1; descending by count.
        #expect(plan.categoryCounts.first?.category == "People/Mom")
        #expect(plan.categoryCounts.first?.count == 2)
        #expect(plan.categoryCounts.reduce(0) { $0 + $1.count } == 3)
        // folderClassifications is now engine-authoritative: one Mixed folder.
        #expect(plan.folderClassifications?.mixedFolders == 1)
        #expect(plan.folderClassifications?.anchorFolders == 0)
        #expect(plan.folderClassifications?.junkFolders == 0)
    }

    @Test("restructurePlan DTO publishes complete confidence totals")
    func restructurePlanConfidenceCounts() throws {
        let proposals = [
            RestructureProposal(fileID: 1, oldPath: "/a/1.jpg",
                                newPath: "/lib/1.jpg", bucket: "photo",
                                confidence: "auto"),
            RestructureProposal(fileID: 2, oldPath: "/a/2.jpg",
                                newPath: "/lib/2.jpg", bucket: "photo",
                                confidence: "AUTO"),
            RestructureProposal(fileID: 3, oldPath: "/a/3.jpg",
                                newPath: "/lib/3.jpg", bucket: "photo",
                                confidence: "review"),
            RestructureProposal(fileID: 4, oldPath: "/a/4.jpg",
                                newPath: "/lib/4.jpg", bucket: "photo",
                                confidence: "ask"),
            RestructureProposal(fileID: 5, oldPath: "/a/5.jpg",
                                newPath: "/lib/5.jpg", bucket: "photo",
                                confidence: "")
        ]
        let planResult = Restructure.PlanResult(
            proposals: proposals, tierByFolder: [:],
            anchorFolders: 0, mixedFolders: 1, junkFolders: 0)
        let plan = FileIDEngineMain.restructurePlan(
            from: planResult, libraryRoot: "/lib")
        let counts = try #require(plan.confidenceCounts)

        #expect(counts.auto == 2)
        #expect(counts.review == 1)
        #expect(counts.ask == 1)
        #expect(counts.unknown == 1)
        #expect(counts.auto + counts.review + counts.ask + counts.unknown == plan.moves.count)
    }

    // F-C1-004 lockstep: a homogeneous source folder the semantic butler actively
    // relocated classifies Anchor, but because it is being EMPTIED (not kept) it must
    // be remapped to Mixed when exempt — so it neither inflates the "Keep" tile nor
    // badges its surviving moves Anchor. Mirrors the Windows engine's
    // handle_plan_restructure exemption loop. (audit)
    @Test("folderTiersAndCounts remaps an exempt (semantic-claimed) Anchor folder to Mixed")
    func exemptAnchorFolderBecomesMixed() {
        // 5 files in /inbox/dogs all routed into one content group → the source folder
        // classifies Anchor (100% homogeneity, >2 files, non-generic name).
        let moves = (1...5).map { i in
            RestructureProposal(
                fileID: Int64(i), oldPath: "/inbox/dogs/\(i).jpg",
                newPath: "/lib/Dogs/\(i).jpg", bucket: "Dogs", confidence: "auto", reason: nil)
        }
        let classified = Restructure.classifyFolders(moves)
        // Not exempt → Anchor (would wrongly inflate the Keep tile + badge moves Anchor).
        let plain = Restructure.folderTiersAndCounts(classified: classified, exempt: [])
        #expect(plain.anchor == 1 && plain.mixed == 0)
        #expect(plain.tierByFolder["/inbox/dogs"] == "Anchor")
        // Exempt (the butler is relocating these, not keeping them) → Mixed.
        let exempt = Restructure.folderTiersAndCounts(
            classified: classified, exempt: ["/inbox/dogs"])
        #expect(exempt.anchor == 0 && exempt.mixed == 1)
        #expect(exempt.tierByFolder["/inbox/dogs"] == "Mixed")
    }
}
