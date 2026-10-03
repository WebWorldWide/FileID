import Foundation
import Testing
import GRDB
import FileIDShared
@testable import FileIDEngine

@Suite struct ChatSearchPlanTests {
    @Test func confirmedPersonNamesBecomeIdentityFilters() {
        let grandma = ChatSearchPlan.KnownPerson(id: 7, names: ["Grandma"])
        let first = ChatSearchPlan.resolve(
            "Show videos of Grandma opening presents",
            knownPeople: [grandma]
        )
        #expect(first.query == "opening presents")
        #expect(first.kinds == ["video"])
        #expect(first.personIDs == [7])
        #expect(first.personNames == ["Grandma"])

        let refined = ChatSearchPlan.resolve("only videos", previous: first, knownPeople: [grandma])
        #expect(refined.query == "opening presents")
        #expect(refined.kinds == ["video"])
        #expect(refined.personIDs == [7])
        #expect(refined.personNames == ["Grandma"])

        let unconfirmed = ChatSearchPlan.resolve("Grandma opening presents")
        #expect(unconfirmed.query == "Grandma opening presents")
        #expect(unconfirmed.personIDs.isEmpty)

        let alex = ChatSearchPlan.resolve(
            "Find videos where Alex gets a hit",
            knownPeople: [
                ChatSearchPlan.KnownPerson(id: 8, names: ["Alex Johnson", "Alex"]),
                ChatSearchPlan.KnownPerson(id: 9, names: ["Alex Smith", "Alex"])
            ]
        )
        #expect(alex.query == "gets hit")
        #expect(alex.personIDs == [8, 9])
    }

    @Test func sharedPlansPreserveSubjectsAndExplicitFilters() throws {
        struct Fixture: Decodable { let messages: [String]; let query: String; let kinds: [String] }
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let fixtures = try JSONDecoder().decode([Fixture].self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/chat-search.json")))
        for fixture in fixtures {
            let plan = fixture.messages.reduce(nil as ChatSearchPlan?) { ChatSearchPlan.resolve($1, previous: $0) }
            #expect(plan == ChatSearchPlan(query: fixture.query, kinds: fixture.kinds))
        }
    }

    @Test func filtersApplyBeforeLimitsAndToEvidence() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await db.pool.write { sql in
            for id in 1...150 {
                try sql.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(?,?,?,100,0,?,'mov','Birthday gift opening')", arguments: [id,"/offline/birthday-\(id).mov",id,id == 1 ? "video" : "image"])
            }
            try sql.execute(sql: "INSERT INTO catalog_passages(id,file_id,text,source_revision,model_version,confidence,stale) VALUES('v',1,'home run','test','test',1,0),('p',2,'home run','test','test',1,0)")
            #expect(try CatalogStore.search(sql, query: "birthday", kinds: ["video"]).map(\.fileID) == [1])
            #expect(try CatalogStore.search(sql, query: "", kinds: ["video"]).map(\.fileID) == [1])
            #expect(try CatalogStore.search(sql, query: "home run", kinds: ["video"]).map(\.evidenceID) == ["v"])
        }
        let capture = WireCapture(); let service = ChatService()
        for text in ["find birthday", "only videos"] {
            await service.handle(ChatRequest(requestID: UUID().uuidString, conversationID: "c", action: "send", text: text, useModel: false), database: db, sink: capture.sink)
        }
        await capture.finish()
        try await Task.sleep(nanoseconds: 30_000_000)
        let replies = try capture.bytes().split(separator: 10).map { try IPCCoder.decoder.decode(IPCEvent.self, from: Data($0)) }.compactMap { event -> ChatResponse? in
            if case .chatResponse(let response) = event.payload { return response }; return nil
        }
        #expect(replies.last?.hits.map(\.fileID) == [1])
        #expect(replies.last?.message.contains("birthday") == true)
        #expect(replies.last?.message.contains("(video)") == true)
    }

    @Test func personFilterRequiresAConfirmedObservation() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await db.pool.write { sql in
            for id in 1...2 {
                try sql.execute(
                    sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(?,?,?,100,0,'video','mov','Opening presents')",
                    arguments: [id, "/offline/presents-\(id).mov", id]
                )
            }
            try sql.execute(sql: "INSERT INTO persons(id,name,file_count,created_at,is_unknown) VALUES(7,'Grandma',1,0,0)")
            try sql.execute(sql: "INSERT INTO catalog_observations(id,file_id,person_id,start_seconds,end_seconds,source_revision,model_version,confidence,stale) VALUES('grandma-in-video',1,7,12,13,'1:1','test',1,0)")

            let hits = try CatalogStore.search(sql, query: "opening presents", kinds: ["video"], personIDs: [7])
            #expect(hits.map(\.fileID) == [1])
        }
        let capture = WireCapture()
        await ChatService().handle(
            ChatRequest(requestID: "grandma-chat", conversationID: "grandma-chat", action: "send", text: "Show videos of Grandma opening presents", useModel: false),
            database: db,
            sink: capture.sink
        )
        await capture.finish()
        let events = try capture.bytes().split(separator: 10).map {
            try IPCCoder.decoder.decode(IPCEvent.self, from: Data($0))
        }
        let response = events.compactMap { event -> ChatResponse? in
            if case .chatResponse(let value) = event.payload { return value }
            return nil
        }.last
        #expect(response?.hits.map(\.fileID) == [1])
        #expect(response?.message.contains("people: Grandma") == true)
    }
}
