import FileIDShared
import Foundation
import Testing
@testable import FileIDEngine

@Suite("Chat history prompt")
struct ChatHistoryPromptTests {
    @Test func keepsOnlyTheLastTurnsAndTrimsThem() {
        let turns = (1...6).map { (role: $0 % 2 == 1 ? "user" : "assistant", text: "turn \($0) " + String(repeating: "x", count: 400)) }
        let block = ChatService.historyBlock(turns, maxTurns: 4, maxChars: 20)
        #expect(!block.contains("turn 2"))
        #expect(block.contains("turn 3"))
        #expect(block.contains("turn 6"))
        #expect(block.split(separator: "\n").allSatisfy { $0.count < 40 })
    }

    @Test func emptyHistoryAddsNothing() {
        #expect(ChatService.historyBlock([]) == "")
        #expect(ChatService.evidencePrompt(request: "hi", hits: []).hasPrefix("Request: hi"))
    }

    @Test func evidenceNamesTheCaptureDateAndRecognisedPeople() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDChatPeople-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try Database(at: root.appendingPathComponent("catalog.sqlite"))
        let noon = 1_688_472_000.0
        let day = try await db.pool.read { sql in try String.fetchOne(sql, sql: "SELECT date(?,'unixepoch','localtime')", arguments: [noon]) ?? "" }
        let details = try await db.pool.write { sql -> [Int64: [String]] in
            try sql.execute(sql: "INSERT INTO persons(id,name,file_count,created_at,is_unknown) VALUES (1,'Alex',0,0,0),(2,'Blake',0,0,0),(3,'Hidden',0,0,1),(4,NULL,0,0,0)")
            for id in 1...3 {
                try sql.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,created_at) VALUES (?,?,?,1,0,'image','jpg',?)",
                                arguments: [id, "/p/\(id).jpg", id, id == 1 ? noon : nil])
            }
            for (file, person) in [(1, 1), (1, 2), (1, 3), (2, 4), (3, 2)] as [(Int64, Int64)] {
                try sql.execute(sql: "INSERT INTO face_prints(file_id,person_id,print_data,bbox) VALUES (?,?,?,'0,0,1,1')", arguments: [file, person, Data(count: 8)])
            }
            try sql.execute(sql: "UPDATE face_prints SET excluded=1 WHERE file_id=3")
            return try ChatService.evidenceDetails(sql, [1, 2, 3])
        }
        #expect(details == [1: ["taken \(day)", "people: Alex, Blake"]])
        let hits = [CatalogHit(fileID: 1, path: "/p/1.jpg", kind: "image", text: "two people"), CatalogHit(fileID: 2, path: "/p/2.jpg", kind: "image", text: "a field")]
        let prompt = ChatService.evidencePrompt(request: "photos of Alex", hits: hits, details: details)
        #expect(prompt.contains("[1] 1.jpg (image, taken \(day), people: Alex, Blake): two people"))
        #expect(prompt.contains("[2] 2.jpg (image): a field"))
    }

    @Test func historyPrecedesTheRequest() {
        let history = ChatService.historyBlock([(role: "user", text: "photos of Sam")])
        let prompt = ChatService.evidencePrompt(request: "only 2024", hits: [], history: history)
        #expect(prompt.hasPrefix("Earlier in this conversation:\nUser: photos of Sam\nRequest: only 2024"))
    }
}

@Suite("Chat model wake")
struct ChatModelWakeTests {
    private func request(_ kind: String?) -> ChatRequest {
        ChatRequest(requestID: "r", conversationID: "c", action: "send", text: "hi", useModel: true, modelKind: kind)
    }

    @Test func onlyAKnownInstalledModelIsEverWoken() {
        #expect(ChatService.wakeableKind(request(nil)) == nil, "no model chosen")
        #expect(ChatService.wakeableKind(request("hf:attacker/arbitrary")) == nil, "unknown ids are never loaded")
        #expect(ChatService.wakeableKind(request("")) == nil)
    }

    @Test func anIdleEngineMayWakeAModel() async {
        #expect(await ChatService.canWake())
    }
}
