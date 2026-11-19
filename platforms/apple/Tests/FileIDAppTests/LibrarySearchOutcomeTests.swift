import Testing
@testable import FileID

@Suite("Library search outcome")
struct LibrarySearchOutcomeTests {
    private func decide(_ message: String?, keywords: Int = 0, showWeaker: Bool = false) -> LibrarySearchOutcome.Decision {
        LibrarySearchOutcome.decide(message: message, keywordCount: keywords, showWeaker: showWeaker)
    }

    @Test func parsesTheEnginesSentences() {
        let both = LibrarySearchOutcome.Strength(message: "3 matches (2 strong visual), 1 weaker.")
        #expect(both?.good == 3 && both?.strong == 2 && both?.weaker == 1 && both?.text == "3 matches (2 strong visual), 1 weaker.")
        #expect(LibrarySearchOutcome.Strength(message: "1 match (1 strong visual).")?.good == 1)
        #expect(LibrarySearchOutcome.Strength(message: "No good matches, 60 weaker.")?.weaker == 60)
        #expect(LibrarySearchOutcome.Strength(message: "No matches.")?.good == 0)
        #expect(LibrarySearchOutcome.Strength(message: "Matches by name, text and what's in the picture.") == nil)
        #expect(LibrarySearchOutcome.Strength(message: "Visual similarity results.") == nil)
        #expect(LibrarySearchOutcome.Strength(message: nil) == nil)
    }

    @Test func goodMatchesShowTheEnginesListAndSummary() {
        let d = decide("4 matches (3 strong visual), 20 weaker.", keywords: 2)
        #expect(d.source == .engine && d.addKeywordRows && d.hiddenWeaker == 0 && d.label == "4 matches (3 strong visual), 20 weaker.")
    }

    @Test func aSummaryThatFillsTheRequestLimitSaysItWasCut() {
        #expect(decide("60 matches (7 strong visual).").label == "60+ matches (7+ strong visual).")
        #expect(decide("45 matches (7 strong visual), 15 weaker.").label == "45 matches (7 strong visual), 15+ weaker.")
        #expect(decide("8 matches (2 strong visual), 51 weaker.").label == "8 matches (2 strong visual), 51 weaker.")
        #expect(decide("No good matches, 60 weaker.", showWeaker: true).label == "No good matches, 60+ weaker.")
        #expect(decide("59 matches (0 strong visual).").label == "59 matches (0 strong visual).")
    }

    @Test func keywordMatchesOnlyKeepWeakerBehindTheAction() {
        let d = decide("No good matches, 60 weaker.", keywords: 3)
        #expect(d.source == .keyword && d.hiddenWeaker == 60 && d.label == "3 by name, tag or text")
    }

    @Test func weakerOnlyShowsNothingUntilAsked() {
        let d = decide("No good matches, 60 weaker.", keywords: 0)
        #expect(d.source == .keyword && d.hiddenWeaker == 60 && d.label == "No good matches")
        let shown = decide("No good matches, 60 weaker.", keywords: 0, showWeaker: true)
        #expect(shown.source == .engine && shown.hiddenWeaker == 0)
    }

    @Test func nothingAtAll() {
        let d = decide("No matches.", keywords: 0)
        #expect(d.source == .keyword && d.hiddenWeaker == 0 && d.label == "No matches")
    }

    @Test func noSummaryKeepsTodaysBehaviour() {
        let d = decide("Matches by name, text and what's in the picture.", keywords: 0)
        #expect(d.source == .engine && d.addKeywordRows && d.hiddenWeaker == 0 && d.label == LibrarySearchOutcome.plainLabel)
        #expect(decide(nil) == d)
    }
    @Test func aKindFilterRecountsTheSummaryForTheRowsItKeeps() {
        let hits: [Int64] = [1, 2, 3, 4, 5, 6]
        #expect(LibrarySearchOutcome.narrowed(message: "3 matches (2 strong visual), 3 weaker.", hitIDs: hits, keptIDs: [2, 5]) == "1 match, 1 weaker.")
        #expect(LibrarySearchOutcome.narrowed(message: "3 matches (2 strong visual), 3 weaker.", hitIDs: hits, keptIDs: [1, 2]) == "2 matches.")
        #expect(LibrarySearchOutcome.narrowed(message: "3 matches (2 strong visual), 3 weaker.", hitIDs: hits, keptIDs: [6]) == "No good matches, 1 weaker.")
        #expect(LibrarySearchOutcome.narrowed(message: "3 matches (2 strong visual), 3 weaker.", hitIDs: hits, keptIDs: []) == "No matches.")
        #expect(LibrarySearchOutcome.narrowed(message: "3 matches (2 strong visual), 3 weaker.", hitIDs: [1, 1, 2, 4, 4, 5], keptIDs: [1, 4]) == "1 match, 1 weaker.")
        #expect(LibrarySearchOutcome.narrowed(message: "Something else", hitIDs: hits, keptIDs: [1]) == nil)
        let filtered = LibrarySearchOutcome.decide(message: LibrarySearchOutcome.narrowed(message: "8 matches (3 strong visual), 52 weaker.", hitIDs: hits, keptIDs: []),
                                                   keywordCount: 0, showWeaker: false)
        #expect(filtered.label == "No matches" && filtered.hiddenWeaker == 0)
    }

    @Test("the Moments header names the weaker look-alikes it left out, with a plus when the limit cut the list")
    func weakerLabelMarksATruncatedCount() {
        #expect(LibrarySearchOutcome.Strength(message: "3 good matches, 1 weaker.")?.weakerLabel == "1")
        let limit = LibrarySearchOutcome.resultLimit
        let cut = LibrarySearchOutcome.Strength(message: "10 good matches, \(limit - 10) weaker.")
        #expect(cut?.weakerLabel == "\(limit - 10)+")
    }
}
