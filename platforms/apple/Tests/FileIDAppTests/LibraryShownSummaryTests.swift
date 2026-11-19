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

    @Test func preview_position_never_reads_the_page_size_as_the_library_size() {
        #expect(LibraryView.previewPosition(index: 48, loaded: 300, moreToLoad: true, knownTotal: 469) == "49 of 469")
        #expect(LibraryView.previewPosition(index: 48, loaded: 300, moreToLoad: true, knownTotal: nil) == "49 of 300+")
        #expect(LibraryView.previewPosition(index: 1, loaded: 12, moreToLoad: false, knownTotal: nil) == "2 of 12")
        #expect(LibraryView.previewPosition(index: 1, loaded: 300, moreToLoad: true, knownTotal: 120) == "2 of 300")
    }
}
