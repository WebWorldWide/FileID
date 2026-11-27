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

    @Test func header_counts_the_files_the_grid_lists_and_names_the_unreadable_ones() {
        #expect(Detail.libraryMeta(total: 472, unreadable: 3, location: nil) == "469 files · 3 couldn’t be read")
        #expect(Detail.libraryMeta(total: 20, unreadable: 16, location: nil) == "4 files · 16 couldn’t be read")
        #expect(Detail.libraryMeta(total: 472, unreadable: 0, location: "Photos") == "472 files in Photos")
        #expect(Detail.libraryMeta(total: 10, unreadable: 10, location: "Photos") == "0 files in Photos · 10 couldn’t be read")
        #expect(Detail.libraryMeta(total: 0, unreadable: 0, location: nil) == "")
    }

    @Test func a_search_or_filter_never_borrows_the_whole_librarys_size() {
        #expect(LibraryView.shownSummary(rows: 12, total: nil) == "12 shown")
        #expect(LibraryView.shownSummary(rows: 300, total: nil, moreToLoad: true) == "300+ shown")
        #expect(LibraryView.shownSummary(rows: 300, total: nil, moreToLoad: true, short: true) == "300+")
    }

    @Test func the_preview_sheet_fits_the_smallest_window() {
        #expect(FilePreviewSheet.minimumWidth <= WindowSizes.minimum.width)
        #expect(FilePreviewSheet.minimumWidth >= 340 + 480)
    }
}
