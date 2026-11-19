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
}
