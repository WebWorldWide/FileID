import Testing
import FileIDShared
@testable import FileID

private func item(_ face: Int64, person: Int64 = 1, name: String? = "Alex", score: Double = 0.62, reason: String = "suggested",
                  runner: Int64? = nil, runnerName: String? = nil) -> FaceReviewItem {
    FaceReviewItem(faceID: face, fileID: face, path: "/photos/IMG_\(face).jpg", bbox: "b", personID: person, personName: name,
                   representativeFaceID: nil, score: score, reason: reason, runnerUpPersonID: runner, runnerUpName: runnerName,
                   runnerUpScore: runner == nil ? nil : 0.6, createdAt: 0)
}

@MainActor
private final class FakeEngine {
    var replies: [FaceReviewPage] = []
    var sent: [IPCCommand.Payload] = []

    func request(_ command: (String) -> IPCCommand.Payload) -> FaceReviewPage? {
        let payload = command("req")
        sent.append(payload)
        return replies.isEmpty ? nil : replies.removeFirst()
    }
}

private func page(_ action: String, status: String = "ok", total: Int = 0, offset: Int = 0, items: [FaceReviewItem] = [],
                  message: String? = nil, operation: String? = nil) -> FaceReviewPage {
    FaceReviewPage(requestID: "req", action: action, status: status, message: message, operationID: operation,
                   total: total, offset: offset, items: items)
}

@Suite("Face review wording")
struct FaceReviewTextTests {
    @Test func reasonsUseTheSharedSentences() {
        #expect(FaceReviewText.reason(item(1)) == "Looks like Alex (62% match).")
        #expect(FaceReviewText.reason(item(1, score: 0.474, reason: "uncertain")) == "Might be Alex, but the match is weak (47%).")
        #expect(FaceReviewText.reason(item(1, reason: "ambiguous", runner: 2, runnerName: "Sam")) == "Could be Alex or Sam; both look about as close.")
        #expect(FaceReviewText.reason(item(1, reason: "ambiguous", runner: 2)) == "Could be Alex or an unnamed person; both look about as close.")
        #expect(FaceReviewText.reason(item(1, name: nil, reason: "ambiguous")) == "Could be an unnamed person, but it's a close call.")
    }

    @Test func labelsNameTheFileAndPerson() {
        let row = item(7, name: "  ")
        #expect(FaceReviewText.name(row.personName) == "an unnamed person")
        #expect(FaceReviewText.faceLabel(row) == "Face in IMG_7.jpg")
        #expect(FaceReviewText.acceptLabel(item(7)) == "Accept: this is Alex")
        #expect(FaceReviewText.rejectLabel(item(7)) == "Not Alex")
        #expect(FaceReviewText.rowLabel(item(7)) == "Looks like Alex (62% match). In IMG_7.jpg.")
        #expect(FaceReviewText.header(3) == "Review suggestions, 3 waiting")
        #expect(FaceReviewText.percent(0.625) == 63)
    }
}

@Suite("Face review model")
@MainActor
struct FaceReviewModelTests {
    @Test func pagesResolvesAndUndoes() async {
        let fake = FakeEngine()
        let model = FaceReviewModel(personID: 4) { fake.request($0) }
        fake.replies = [page("list", total: 3, items: [item(1), item(2)])]
        await model.reload()
        #expect(model.items.map(\.faceID) == [1, 2] && model.hasMore)
        guard case .listFaceReview(_, let person, let offset, let limit) = fake.sent.last else { Issue.record("not a list"); return }
        #expect(person == 4 && offset == 0 && limit == 50)

        fake.replies = [page("list", total: 3, offset: 2, items: [item(2), item(3)])]
        await model.loadMore()
        #expect(model.items.map(\.faceID) == [1, 2, 3] && !model.hasMore)

        fake.replies = [page("resolve", total: 9, operation: "op-1")]
        await model.resolve(model.items[0], .accept)
        #expect(model.items.map(\.faceID) == [2, 3] && model.total == 2 && model.undoOperationID == "op-1")
        guard case .resolveFaceReview(_, 1, 1, "accept") = fake.sent.last else { Issue.record("not a resolve"); return }

        fake.replies = [page("undo", total: 3), page("list", total: 3, items: [item(1), item(2), item(3)])]
        await model.undo()
        #expect(model.undoOperationID == nil && model.items.count == 3)
        guard case .listFaceReview = fake.sent.last else { Issue.record("undo should reload"); return }
    }

    @Test func refusedResolveKeepsTheRowAndShowsTheMessage() async {
        let fake = FakeEngine()
        let model = FaceReviewModel { fake.request($0) }
        fake.replies = [page("list", total: 1, items: [item(1)])]
        await model.reload()
        fake.replies = [page("resolve", status: "refused", message: "That person is already in this photo.")]
        await model.resolve(model.items[0], .accept)
        #expect(model.items.count == 1 && model.message == "That person is already in this photo." && model.undoOperationID == nil)
    }

    @Test func noReplyIsReportedAndHidesNothing() async {
        let fake = FakeEngine()
        let model = FaceReviewModel { fake.request($0) }
        await model.reload()
        #expect(model.message == FaceReviewText.unreachable && model.isVisible && model.items.isEmpty)
    }
}
