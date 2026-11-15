import Testing
import FileIDShared
@testable import FileID

struct BulkResultMatcherTests {
    private func result(_ action: String, id: String?) -> BulkActionResult {
        BulkActionResult(action: action, succeeded: 1, failed: 0, messages: [], commandID: id)
    }

    @Test func matchingIDIsAccepted() {
        let matcher = BulkResultMatcher(actionPrefix: "trashFiles", sentID: "a")
        #expect(matcher.accepts(result("trashFiles:batch", id: "a")))
    }

    @Test func otherIDIsRefusedEvenWithTheSameAction() {
        let matcher = BulkResultMatcher(actionPrefix: "trashFiles", sentID: "a")
        #expect(!matcher.accepts(result("trashFiles:batch", id: "b")))
    }

    @Test func resultWithoutIDFallsBackToActionPrefix() {
        let matcher = BulkResultMatcher(actionPrefix: "trashFiles", sentID: "a")
        #expect(matcher.accepts(result("trashFiles:batch", id: nil)))
        #expect(matcher.accepts(result("trashFiles", id: "")))
        #expect(!matcher.accepts(result("restoreFromTrash", id: nil)))
    }

    @Test func waiterThatNeverSawAnIDFallsBackToActionPrefix() {
        let matcher = BulkResultMatcher(actionPrefix: "trashFiles")
        #expect(matcher.accepts(result("trashFiles:batch", id: "z")))
        #expect(!matcher.accepts(result("restoreFromTrash", id: "z")))
        #expect(BulkResultMatcher(actionPrefix: "trashFiles", sentID: "").accepts(result("trashFiles", id: "z")))
    }

    @Test func twoSameActionRequestsEachGetTheirOwnResult() {
        let first = BulkResultMatcher(actionPrefix: "renamePerson", sentID: "1")
        let second = BulkResultMatcher(actionPrefix: "renamePerson", sentID: "2")
        let forFirst = result("renamePerson", id: "1")
        let forSecond = result("renamePerson", id: "2")
        #expect(first.accepts(forFirst) && !first.accepts(forSecond))
        #expect(second.accepts(forSecond) && !second.accepts(forFirst))
    }
}
