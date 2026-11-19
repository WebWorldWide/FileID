import Testing
@testable import FileID

@Suite("Best Takes primary button title")
struct TakeReviewTitleTests {
    @Test func says_so_instead_of_counting_zero_while_checking() {
        #expect(TakeReview.primaryTitle(loading: true, hasTakes: false, kept: 0, others: 0, keepsOnlyBest: false) == "Checking these takes…")
        #expect(TakeReview.primaryTitle(loading: false, hasTakes: false, kept: 0, others: 0, keepsOnlyBest: false) == "Move the others to Trash")
    }

    @Test func counts_once_there_are_takes() {
        #expect(TakeReview.primaryTitle(loading: false, hasTakes: true, kept: 1, others: 3, keepsOnlyBest: true) == "Keep the best, move the other 3 to Trash")
        #expect(TakeReview.primaryTitle(loading: false, hasTakes: true, kept: 2, others: 2, keepsOnlyBest: true) == "Keep 2, move the other 2 to Trash")
    }
}
