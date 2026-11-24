import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Chat trash result")
struct ChatTrashResultTests {
    @Test func anEmptyMoveSaysNothingMovedAndWhy() {
        var outcome = DuplicateTrashOutcome()
        outcome.failed = 8
        #expect(ExactDuplicateVerifier.nothingMovedSummary(outcome, skippedAsChanged: 0)
                == "Nothing was moved to the Trash: 8 couldn’t be moved to the Trash. Your files are untouched.")
        outcome.failureMessage = "The Trash folder isn't available."
        outcome.protectedLocation = 1
        let text = ExactDuplicateVerifier.nothingMovedSummary(outcome, skippedAsChanged: 2)
        #expect(text.hasPrefix("Nothing was moved to the Trash: 2 changed since the scan, so FileID left them alone; 1 is in a managed media library"))
        #expect(text.contains("The Trash folder isn't available."))
        #expect(!text.contains("freed"))
        outcome.readOnlyDrive = "Adlon"
        let drive = ExactDuplicateVerifier.nothingMovedSummary(outcome, skippedAsChanged: 0)
        #expect(drive.contains("\u{201C}Adlon\u{201D} is a read-only drive"))
        #expect(!drive.contains("Photos library") && !drive.contains("example data"))
    }

    @Test func noReasonStillSaysNothingMoved() {
        #expect(ExactDuplicateVerifier.nothingMovedSummary(DuplicateTrashOutcome(), skippedAsChanged: 0)
                == "Nothing was moved to the Trash. Your files are untouched.")
    }
}
