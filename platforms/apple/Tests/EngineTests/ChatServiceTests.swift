import Foundation
import Testing
import GRDB
import FileIDShared
@testable import FileIDEngine

@Suite struct ChatServiceTests {
    @Test func searchHistoryAndClearRemainLocalAndPersistent() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await db.pool.write { sql in
            try sql.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(1,'/offline/birthday.mov',1,100,0,'video','mov','Birthday gift opening')")
        }
        let capture = WireCapture()
        let service = ChatService()
        await service.handle(ChatRequest(requestID: "send", conversationID: "c", action: "send", text: "Please find my birthday videos", useModel: false), database: db, sink: capture.sink)
        #expect(try await db.pool.read { sql in try Int.fetchOne(sql, sql: "SELECT COUNT(*) FROM catalog_chat") } == 2)
        try db.pool.close()
        let reopened = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        await service.handle(ChatRequest(requestID: "history", conversationID: "c", action: "history"), database: reopened, sink: capture.sink)
        await service.handle(ChatRequest(requestID: "clear", conversationID: "c", action: "clear"), database: reopened, sink: capture.sink)
        #expect(try await reopened.pool.read { sql in try Int.fetchOne(sql, sql: "SELECT COUNT(*) FROM catalog_chat") } == 0)
        #expect(try await reopened.pool.read { sql in try Int.fetchOne(sql, sql: "SELECT COUNT(*) FROM files") } == 1)
        await capture.finish()
        try await Task.sleep(nanoseconds: 30_000_000)
        let events = try capture.bytes().split(separator: 10).map { try IPCCoder.decoder.decode(IPCEvent.self, from: Data($0)) }
        let replies = events.compactMap { event -> ChatResponse? in if case .chatResponse(let r) = event.payload { return r }; return nil }
        #expect(replies.contains { $0.requestID == "send" && $0.status == "completed" && $0.hits.first?.fileID == 1 })
        #expect(replies.contains { $0.requestID == "history" && $0.messages.count == 2 })
        #expect(replies.contains { $0.requestID == "clear" && $0.messages.isEmpty })
    }

    @Test func emptyAndOversizedRequestsCannotWriteHistory() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let capture = WireCapture(); let service = ChatService()
        for text in [" ", String(repeating: "x", count: 2001)] {
            await service.handle(ChatRequest(requestID: UUID().uuidString, conversationID: "c", action: "send", text: text), database: db, sink: capture.sink)
        }
        #expect(try await db.pool.read { sql in try Int.fetchOne(sql, sql: "SELECT COUNT(*) FROM catalog_chat") } == 0)
        await capture.finish()
    }

    @Test func requestsRoundTripWithoutNullFields() throws {
        let request = IPCCommand(payload: .chatRequest(request: ChatRequest(requestID: "r", conversationID: "c", action: "history")))
        let data = try IPCCoder.encoder.encode(request)
        #expect(!String(decoding: data, as: UTF8.self).contains("null"))
        let decoded = try IPCCoder.decoder.decode(IPCCommand.self, from: data)
        if case .chatRequest(let chat) = decoded.payload { #expect(chat.action == "history") }
        else { Issue.record("Wrong conversation payload") }
        #expect(ChatService.query("Please show me my birthday videos") == "birthday")
    }
}
