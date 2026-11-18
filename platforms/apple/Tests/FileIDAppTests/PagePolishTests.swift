import Testing
@testable import FileID

@Suite("Page polish rules")
struct PagePolishTests {
    @Test func tileTagsDropTheOneThatRepeatsTheKindBadge() {
        #expect(TagLabel.tileChips(["Video", "Beach", "Sunset"], kind: "video") == ["Beach", "Sunset"])
        #expect(TagLabel.tileChips(["doc", "Invoice"], kind: "doc") == ["Invoice"])
        #expect(TagLabel.tileChips(["PDF"], kind: "pdf").isEmpty)
        #expect(TagLabel.tileChips(["Pumpkin", "Hay", "Man"], kind: "image") == ["Pumpkin", "Hay"])
    }

    @Test func relationChipOnlyMarksExceptions() {
        let same: [String?] = ["repeatedAttempts", "repeatedAttempts", "repeatedAttempts"]
        #expect(!TakeProposals.showsRelationChip("repeatedAttempts", among: same))
        let mixed: [String?] = ["repeatedAttempts", "repeatedAttempts", "uncertain"]
        #expect(!TakeProposals.showsRelationChip("repeatedAttempts", among: mixed))
        #expect(TakeProposals.showsRelationChip("uncertain", among: mixed))
        #expect(TakeProposals.showsRelationChip("alternateViews", among: ["alternateViews"]))
        #expect(TakeProposals.showsRelationChip("alternateViews", among: ["alternateViews", "repeatedAttempts"]))
    }

    @Test func startFailureCopyNeverCallsTheHelperAnEngine() {
        let causes: [EngineStartFailure.Cause] = [.missing, .signatureMismatch, .couldNotLaunch, .exitsAtStart, .keepsStopping]
        for cause in causes {
            #expect(!EngineStartFailure(cause).summary.lowercased().contains("engine"), "\(cause)")
            #expect(!EngineUnavailableCard.reinstallHint(for: EngineStartFailure(cause)).lowercased().contains("engine"), "\(cause)")
        }
        #expect(!EngineUnavailableCard.title.lowercased().contains("engine"))
    }

    @Test func disabledControlsKeepLegibleText() {
        #expect(Theme.disabledOpacity >= 0.55)
    }
}
