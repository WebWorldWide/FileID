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
        #expect(FaceReviewText.reason(item(1)) == "This is very likely Alex.")
        #expect(FaceReviewText.reason(item(1, score: 0.474, reason: "uncertain")) == "This might be Alex, but it's hard to tell.")
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
        #expect(FaceReviewText.rowLabel(item(7)) == "This is very likely Alex. In IMG_7.jpg.")
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
        guard case .listFaceReview(_, let person, let offset, let limit, _) = fake.sent.last else { Issue.record("not a list"); return }
        #expect(person == 4 && offset == 0 && limit == 50)

        fake.replies = [page("list", total: 3, offset: 2, items: [item(2), item(3)])]
        await model.loadMore()
        #expect(model.items.map(\.faceID) == [1, 2, 3] && !model.hasMore)

        fake.replies = [page("resolve", total: 9, operation: "op-1")]
        await model.resolve(model.items[0], .accept)
        #expect(model.items.map(\.faceID) == [2, 3] && model.total == 2 && model.undoOperationID == "op-1")
        guard case .resolveFaceReview(_, 1?, 1, "accept", nil) = fake.sent.last else { Issue.record("not a resolve"); return }

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

@Suite("Face review queues in the app")
@MainActor
struct FaceQueueModelTests {
    private func merge() -> FaceMergeSuggestion {
        FaceMergeSuggestion(personID: 2, personName: "Alex", representativeFaceID: 20, faceCount: 5, otherPersonID: 3,
                            otherName: nil, otherRepresentativeFaceID: 30, otherFaceCount: 1, score: 0.71, createdAt: 0)
    }

    private func split() -> FaceSplitSuggestion {
        FaceSplitSuggestion(personID: 4, personName: "Kim", representativeFaceID: 40, score: 0.2, keepCount: 4, splitCount: 3,
                            keepFaceIDs: [40, 41], splitFaceIDs: [50], createdAt: 0)
    }

    @Test func acceptAllSendsOneRequestAndReportsSkips() async {
        let fake = FakeEngine()
        let model = FaceReviewModel(personID: 1) { fake.request($0) }
        fake.replies = [page("list", total: 3, items: [item(1), item(2), item(3, reason: "uncertain")])]
        await model.reload()
        #expect(model.suggestedCount == 2)
        fake.replies = [FaceReviewPage(requestID: "req", action: "resolve", status: "ok", operationID: "op", personID: 1,
                                       total: 1, offset: 0, acceptedFaceIDs: [1], skippedFaceIDs: [2])]
        await model.acceptAll()
        guard case .resolveFaceReview(_, nil, 1, "acceptAll", nil) = fake.sent.last else { Issue.record("not acceptAll"); return }
        #expect(model.items.map(\.faceID) == [2, 3])
        #expect(model.notice == "Accepted 1 face · 1 skipped, already in their photo")
        #expect(model.undoOperationID == "op")
    }

    @Test func mergeQueueResolvesWithBothPeopleAndUndoes() async {
        let fake = FakeEngine()
        let model = FaceQueueModel(kind: .merges) { fake.request($0) }
        fake.replies = [FaceReviewPage(requestID: "req", action: "list", status: "ok", total: 1, offset: 0, queue: "merges", merges: [merge()])]
        await model.reload()
        guard case .listFaceReview(_, nil, 0, 100, "merges") = fake.sent.last else { Issue.record("not a merges list"); return }
        #expect(model.merges.count == 1 && model.total == 1)
        fake.replies = [FaceReviewPage(requestID: "req", action: "resolve", status: "ok", operationID: "op", total: 0, offset: 0, queue: "merges"),
                        FaceReviewPage(requestID: "req", action: "list", status: "ok", total: 0, offset: 0, queue: "merges", merges: [])]
        await model.resolve(merge(), "merge")
        guard case .resolveFaceReview(_, nil, 2, "merge", 3) = fake.sent[1] else { Issue.record("not a merge"); return }
        #expect(model.undoOperationID == "op" && model.notice == "Merged." && model.merges.isEmpty)
        fake.replies = [FaceReviewPage(requestID: "req", action: "undo", status: "refused", message: "Some of these faces were moved since, so nothing changed.",
                                       total: 0, offset: 0, queue: "merges"),
                        FaceReviewPage(requestID: "req", action: "list", status: "ok", total: 0, offset: 0, queue: "merges", merges: [])]
        await model.undo()
        #expect(model.message == "Some of these faces were moved since, so nothing changed.")
    }

    @Test func splitAndRefreshWording() {
        #expect(FaceQueueText.splitLabel(split()) == "Split Kim into two people")
        #expect(FaceQueueText.mergeLabel(merge()) == "Merge Unnamed into Alex")
        #expect(FaceQueueText.stays(split()) == "Stays (4)")
        let running = FaceRefreshStatus(model: "m", processing: "p", outdated: 3, total: 10, done: 6, pending: 2, missing: 1, offline: 1,
                                        failed: 0, jobID: "face-refresh", state: "running", progress: 0.799)
        #expect(FaceRefreshText.line(running) == "Updating faces: 79% · 1 missing · 1 offline")
        let idle = FaceRefreshStatus(model: "m", processing: "p", outdated: 3, total: 0, done: 0, pending: 0, missing: 0, offline: 0, failed: 0)
        #expect(FaceRefreshText.line(idle) == "Faces need updating (3)")
        let clean = FaceRefreshStatus(model: "m", processing: "p", outdated: 0, total: 5, done: 5, pending: 0, missing: 0, offline: 0,
                                      failed: 0, jobID: "face-refresh", state: "completed", progress: 1)
        #expect(FaceRefreshText.line(clean) == nil)
    }

    @Test func refreshStartsTheJobAndShowsTheNoModelMessage() async {
        let fake = FakeEngine()
        var sent: [CatalogRequest] = []
        let model = FaceRefreshModel(request: { fake.request($0) }, catalog: { request in
            sent.append(request)
            return CatalogResponse(requestID: request.requestID, status: "error", message: "Install the face recognition model in Settings, then update faces again.")
        })
        await model.start()
        #expect(sent.first?.action == "enqueueFaceRefresh")
        #expect(model.line == "Install the face recognition model in Settings, then update faces again.")
    }
}
