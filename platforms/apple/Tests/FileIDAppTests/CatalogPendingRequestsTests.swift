import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Catalog workbench requests")
struct CatalogPendingRequestsTests {
    private func response(_ id: String, status: String = "ok") -> CatalogResponse {
        CatalogResponse(requestID: id, status: status, message: nil, hits: [], chapters: [], jobs: [])
    }

    @Test func aDetailRequestSentWhileASearchIsPendingDoesNotDropTheSearch() {
        var pending = CatalogPendingRequests()
        pending.beginSearch("s1")
        pending.beginOther("d1", action: "detail")
        #expect(pending.isCurrentSearch("s1"))
        #expect(pending.searching)
        let settled = pending.settle(responses: ["s1": response("s1"), "d1": response("d1")])
        #expect(settled.map(\.action) == ["search", "detail"])
        #expect(!pending.searching)
    }

    @Test func aResponseIsHandedOverOnceAndOnlyWhenItArrives() {
        var pending = CatalogPendingRequests()
        pending.beginSearch("s1")
        #expect(pending.settle(responses: [:]).isEmpty)
        #expect(pending.settle(responses: ["s1": response("s1")]).count == 1)
        #expect(pending.settle(responses: ["s1": response("s1")]).isEmpty)
    }

    @Test func aFailedSearchDropsTheOldResultsAndASuccessfulOneDoesNot() {
        #expect(CatalogPendingRequests.dropsListedResults(action: "search", status: "error"))
        #expect(!CatalogPendingRequests.dropsListedResults(action: "search", status: "ok"))
        #expect(!CatalogPendingRequests.dropsListedResults(action: "detail", status: "error"))
    }
}
