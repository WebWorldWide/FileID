import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Related-take proposals presentation")
struct TakeProposalsTests {
    private func job(_ state: String, updated: Double = 1) -> CatalogJob {
        CatalogJob(id: "take-grouping", kind: "takeGrouping", state: state, progress: 0.5, createdAt: 0, updatedAt: updated)
    }

    @Test func relationChipsAndEvidenceText() {
        #expect(TakeProposals.relationLabel("repeatedAttempts") == "Repeated tries")
        #expect(TakeProposals.relationLabel("alternateViews") == "Other angles")
        #expect(TakeProposals.relationLabel("uncertain") == "Not sure")
        #expect(TakeProposals.relationLabel(nil) == nil && TakeProposals.relationLabel("other") == nil)
        #expect(TakeProposals.relationTone("repeatedAttempts") == .gold && TakeProposals.relationTone("alternateViews") == .info
            && TakeProposals.relationTone("uncertain") == .ai)
        for relation in ["repeatedAttempts", "alternateViews", "uncertain"] { #expect(TakeProposals.relationHelp(relation) != nil) }
        let codes = ["captureSession", "visualSimilarity", "sharedSubject", "sameCamera", "simultaneousCameras", "mixedCameras", "cameraDetailsMissing"]
        for code in codes { #expect(TakeProposals.evidenceSentence(code) != nil) }
        #expect(TakeProposals.evidenceText(["captureSession", "visualSimilarity", "future", "sameCamera"])
            == "Taken close together. They look alike. One camera took them all.")
        #expect(TakeProposals.evidenceText(nil).isEmpty)
    }

    @Test func jobFindingAndReload() {
        #expect(TakeProposals.job([CatalogJob(id: "t", kind: "timelineSample", state: "running", progress: 0, createdAt: 0, updatedAt: 0), job("paused")])?.state == "paused")
        #expect(TakeProposals.canFind(nil) && TakeProposals.canFind(job("paused")) && TakeProposals.canFind(job("completed")))
        #expect(!TakeProposals.canFind(job("queued")) && !TakeProposals.canFind(job("running")))
        #expect(TakeProposals.finished(previous: job("running"), current: job("completed")))
        #expect(TakeProposals.finished(previous: nil, current: job("completed")))
        #expect(TakeProposals.finished(previous: job("completed", updated: 1), current: job("completed", updated: 2)))
        #expect(!TakeProposals.finished(previous: job("completed"), current: job("completed")))
        #expect(!TakeProposals.finished(previous: job("queued"), current: job("running")))
        #expect(TakeProposals.enqueueRequest(id: "e").action == "enqueueTakeGrouping")
        let read = TakeProposals.proposalsRequest(id: "p")
        #expect(read.action == "takeGroupProposals" && read.limit == 100)
    }

    @Test func labelsAndSavedMembers() {
        let group = CatalogTakeAutoGroup(groupKey: "takes-1", kind: "photos", status: "skipped", capturedAt: 0, similarity: 0.9,
                                         shots: [CatalogTakeShot(primaryFileID: 4, fileIDs: [4, 5], path: "/p/a.JPG"),
                                                 CatalogTakeShot(primaryFileID: 6, fileIDs: [6], path: "/p/b.JPG")],
                                         relation: "alternateViews", evidence: ["captureSession", "simultaneousCameras"])
        #expect(TakeProposals.accessibilityLabel(group, date: "Sat 4 Jul")
            == "2 photos, Other angles, Sat 4 Jul, skipped. Taken close together. Different cameras took them at the same moment.")
        let hits = TakeProposals.saveHits(group)
        #expect(hits.map(\.fileID) == [4, 6] && hits.allSatisfy { $0.kind == "image" })
    }
}
