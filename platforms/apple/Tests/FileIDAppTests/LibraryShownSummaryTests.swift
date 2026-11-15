import Testing
@testable import FileID

@Suite("Library shown summary")
struct LibraryShownSummaryTests {
    @Test func total_never_below_rows() {
        #expect(LibraryView.shownSummary(rows: 65, total: 63) == "65 of 65 shown")
        #expect(LibraryView.shownSummary(rows: 65, total: 63, short: true) == "65/65")
    }

    @Test func normal_counts_pass_through() {
        #expect(LibraryView.shownSummary(rows: 200, total: 5000) == "200 of 5,000 shown")
        #expect(LibraryView.shownSummary(rows: 0, total: 0) == "0 of 0 shown")
    }
}
