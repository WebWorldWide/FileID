import Foundation
import Testing
import GRDB
import FileIDShared
@testable import FileIDEngine

@Suite("Best-take groups and correction history")
struct CatalogTakesTests {
    @Test func outcomePrecedesQualityAndUnknownTakesAbstain() {
        let event = CatalogEvent(id: "baseball", title: "Batting practice", goal: "Alex gets a hit", fileIDs: [1, 2])
        let miss = CatalogTake(eventID: event.id, fileID: 1, path: "/internal/miss.mov", outcomeScore: 0,
                               qualityScore: 1, confidence: 1)
        let hit = CatalogTake(eventID: event.id, fileID: 2, path: "/internal/hit.mov", outcomeScore: 1,
                              qualityScore: 0.2, confidence: 1)
        #expect(CatalogTakes.recommend(event: event, takes: [miss, hit]).fileIDs == [2])
        #expect(CatalogTakes.recommend(event: event, takes: [miss, CatalogTake(eventID: event.id, fileID: 2, path: hit.path)]).status == "insufficient")
        var tied = hit
        tied.outcomeScore = 0.98
        tied.qualityScore = nil
        var other = miss
        other.outcomeScore = 1
        other.qualityScore = nil
        #expect(CatalogTakes.recommend(event: event, takes: [other, tied]).status == "tie")
        other.qualityScore = 0.9
        tied.qualityScore = 0.4
        #expect(CatalogTakes.recommend(event: event, takes: [other, tied]).fileIDs == [1])
        tied.preferred = true
        #expect(CatalogTakes.recommend(event: event, takes: [other, tied]).status == "preferred")
    }

    @Test func groupFeedbackUndoAndSourceChange() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,'/internal/miss.mov',1,100,10,0,'video','mov')")
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(2,'/internal/hit.mov',2,100,10,0,'video','mov')")
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(3,'/internal/other.mov',3,100,10,0,'video','mov')")
        }
        func send(_ action: String, event: CatalogEvent? = nil, eventID: String? = nil,
                  feedback: CatalogTakeFeedback? = nil, fileID: Int64? = nil) async -> CatalogResponse {
            await CatalogStore.handle(CatalogRequest(requestID: UUID().uuidString, action: action, fileID: fileID,
                                                     event: event, eventID: eventID, takeFeedback: feedback), database: database)
        }
        let event = CatalogEvent(id: "baseball", title: "Batting practice", goal: "Get a hit", fileIDs: [2, 1, 2])
        let created = await send("saveEvent", event: event)
        #expect(created.status == "ok")
        #expect(created.events?.first?.fileIDs == [1, 2])
        #expect(created.recommendation?.status == "insufficient")
        #expect((await send("setTakeFeedback", feedback: CatalogTakeFeedback(eventID: event.id, fileID: 3, outcomeScore: 1))).status == "error")
        #expect((await send("setTakeFeedback", feedback: CatalogTakeFeedback(eventID: event.id, fileID: 1, outcomeScore: 0))).status == "ok")
        let ranked = await send("setTakeFeedback", feedback: CatalogTakeFeedback(eventID: event.id, fileID: 2, outcomeScore: 1))
        #expect(ranked.recommendation?.status == "ranked")
        #expect(ranked.recommendation?.fileIDs == [2])
        let preferred = await send("setTakeFeedback", feedback: CatalogTakeFeedback(eventID: event.id, fileID: 1, outcomeScore: 0, preferred: true))
        #expect(preferred.recommendation?.status == "preferred")
        #expect(preferred.recommendation?.fileIDs == [1])
        let undone = await send("undoTakeFeedback", eventID: event.id, fileID: 1)
        #expect(undone.recommendation?.status == "ranked")
        #expect(undone.recommendation?.fileIDs == [2])
        #expect((await send("deleteEvent", eventID: event.id)).status == "ok")
        #expect((await send("takeGroup", eventID: event.id)).status == "error")
        #expect((await send("undoEventEdit", eventID: event.id)).status == "ok")
        #expect((await send("takeGroup", eventID: event.id)).recommendation?.fileIDs == [2])
        try await database.pool.write { db in
            try db.execute(sql: "UPDATE catalog_take_scores SET quality_score=0.9 WHERE event_id=? AND file_id=2", arguments: [event.id])
            try db.execute(sql: "UPDATE files SET size_bytes=200 WHERE id=2")
        }
        let changed = await send("takeGroup", eventID: event.id)
        #expect(changed.recommendation?.status == "insufficient")
        let reviewedAgain = await send("setTakeFeedback", feedback: CatalogTakeFeedback(eventID: event.id, fileID: 2, outcomeScore: 1))
        #expect(reviewedAgain.takes?.first(where: { $0.fileID == 2 })?.qualityScore == nil)
        #expect(changed.takes?.first(where: { $0.fileID == 2 })?.stale == true)
        #expect(changed.events?.first?.userEdited == true)
    }
}
