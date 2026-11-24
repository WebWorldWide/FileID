import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Organize undo after a relaunch")
struct RestructureUndoJournalTests {
    private func folderWithJournal(_ text: String?) throws -> URL {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("undo-journal-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        if let text {
            try text.write(to: folder.appendingPathComponent(RestructureUndoJournal.fileName), atomically: true, encoding: .utf8)
        }
        return folder
    }

    private let header = #"{"version":3,"library_root":"/Users/me/Photos"}"#
    private let move = #"{"file_id":6,"from":"/Users/me/Photos/Sorted/a.jpg","to":"/Users/me/Photos/a.jpg"}"#

    @Test("a journal with moves names the folder whose Organize can still be undone")
    func journalWithMovesIsPending() throws {
        let folder = try folderWithJournal(header + "\n" + move + "\n")
        defer { try? FileManager.default.removeItem(at: folder) }
        #expect(RestructureUndoJournal.pendingRoot(in: folder) == "/Users/me/Photos")
    }

    @Test("no journal, an empty one, or one with only its header is nothing to undo")
    func emptyJournalsAreNotPending() throws {
        for text in [nil, "", header + "\n", "not json\n" + move + "\n"] {
            let folder = try folderWithJournal(text)
            defer { try? FileManager.default.removeItem(at: folder) }
            #expect(RestructureUndoJournal.pendingRoot(in: folder) == nil)
        }
    }

    @Test("the Undo offer only applies to the folder the journal names")
    func undoFollowsTheJournalsFolder() {
        #expect(RestructureView.sameFolder("/Users/me/Photos", "/Users/me/Photos/"))
        #expect(!RestructureView.sameFolder("/Users/me/Photos", "/Users/me/Other"))
    }

    @Test("the notice for a skipped file name shows the engine's own sentence, even with an underscore in the name")
    func skippedNameNoticeIsShownAsWritten() {
        let message = "1 file stayed where it is because FileID can't use its name in a new folder, for example \"IMG_0001\n.jpg\". Rename it, then organize again. The other files were handled as usual."
        #expect(Sidebar.errorTitle("apply_restructure_skipped", message: message) == "Some files weren’t moved")
        #expect(Sidebar.errorText(kind: "apply_restructure_skipped", message: message) == message)
    }
}

@MainActor
@Suite("Organize move or undo the engine rejects")
struct RestructureRejectionTests {
    @Test("a rejected move or undo is an error that ends the move with no result event")
    func rejectionsReleaseTheMovePage() {
        for kind in ["apply_restructure", "undo_restructure", "db_unavailable"] {
            #expect(EngineClient.endsRestructureWithoutResult(kind))
        }
        for kind in ["plan_restructure_failed", "library_busy", "apply_restructure_skipped"] {
            #expect(!EngineClient.endsRestructureWithoutResult(kind))
        }
    }

    @Test("a rejected undo does not make the next move's result read as an undo")
    func rejectedUndoDoesNotLatchTheUndoFlag() {
        let engine = EngineClient()
        engine.undoRestructureInFlight = true
        engine.handleEvent(IPCEvent(payload: .error(EngineError(kind: "undo_restructure", message: "Organize Folders couldn't undo the move"))))
        #expect(!engine.undoRestructureInFlight)
        engine.handleEvent(IPCEvent(payload: .restructureApplyResult(RestructureApplyResult(applied: 4, failed: 0, cancelled: false))))
        #expect(engine.canUndoRestructure)
    }
}
