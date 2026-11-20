import Testing
@testable import FileID

@Suite("Cleanup header summary")
struct CleanupHeaderTests {
    @Test func counts_extra_copies_not_groups() {
        let text = CleanupView.exactSummary(groups: 4, extraCopies: 5, bytes: 22_400_000)
        #expect(text.hasPrefix("5 extra copies in 4 groups · "))
        #expect(text.hasSuffix("can be freed"))
    }

    @Test func singular_and_empty() {
        #expect(CleanupView.exactSummary(groups: 1, extraCopies: 1, bytes: 1_000).hasPrefix("1 extra copy in 1 group · "))
        #expect(CleanupView.exactSummary(groups: 0, extraCopies: 0, bytes: 0) == "No extra copies found")
    }

    @Test func says_how_many_unreadable_files_are_missing_from_the_results() {
        #expect(CleanupView.unreadableNote(0) == nil)
        #expect(CleanupView.unreadableNote(1) == "1 file couldn’t be read, so it’s not in these results")
        #expect(CleanupView.unreadableNote(5) == "5 files couldn’t be read, so they’re not in these results")
    }

    @Test func promises_a_retry_only_for_files_that_ran_out_of_time() {
        #expect(CleanupView.unreadableNote(2, timedOut: 2) == "2 files couldn’t be read yet, so they’re not in these results")
        #expect(CleanupView.unreadableNote(1, timedOut: 1) == "1 file couldn’t be read yet, so it’s not in these results")
        let mixed = CleanupView.unreadableNote(5, timedOut: 2)
        #expect(mixed == "5 files couldn’t be read, so they’re not in these results. 2 of them ran out of time and may read next time")
        #expect(CleanupView.unreadableNote(3, timedOut: 9) == "3 files couldn’t be read yet, so they’re not in these results")
    }
}
