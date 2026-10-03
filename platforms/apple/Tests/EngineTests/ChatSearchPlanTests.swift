import Foundation
import Testing
import GRDB
import FileIDShared
@testable import FileIDEngine

@Suite struct ChatSearchPlanTests {
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
}
