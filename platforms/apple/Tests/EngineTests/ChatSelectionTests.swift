import Foundation
import Testing
import GRDB
import FileIDShared
@testable import FileIDEngine

@Suite struct ChatSelectionTests {
    struct Fixtures: Decodable {
        struct Criteria: Decodable { let text: String; let criteria: String; let words: [String] }
        struct Checks: Decodable { let criteria: String; let checks: [String] }
        struct Combine: Decodable { let first: [String]; let confirmations: [String]; let answer: String; let reason: String }
        struct VerdictCase: Decodable { let raw: String; let answer: String; let reason: String }
        struct Candidate: Decodable { let id: Int64; let kind: String; let evidence: String; let clip: Double?; let people: [Int64] }
        struct Known: Decodable {
            let id: Int64; let names: [String]
            init(from decoder: Decoder) throws {
                var container = try decoder.unkeyedContainer()
                id = try container.decode(Int64.self); names = try container.decode([String].self)
            }
        }
        struct Ranking: Decodable {
            let name: String; let text: String; let known: [Known]; let candidates: [Candidate]
            let order: [Int64]; let scores: [Double]; let reasons: [String]
        }
        struct Message: Decodable { let kind: String; let args: [Int]; let kinds: [String]; let why: String?; let message: String }
        let criteria: [Criteria]
        let checks: [Checks]
        let combine: [Combine]
        let verdicts: [VerdictCase]
        let ranking: [Ranking]
        let messages: [Message]
    }

    private func fixtures() throws -> Fixtures {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        return try JSONDecoder().decode(Fixtures.self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/chat-selection.json")))
    }

    @Test func sharedCriteriaCases() throws {
        for item in try fixtures().criteria {
            let criteria = ChatSelection.criteria(item.text)
            #expect(criteria == item.criteria, "\(item.text)")
            #expect(ChatSelection.contentWords(criteria) == item.words, "\(item.text)")
        }
    }

    @Test func sharedChecksCases() throws {
        for item in try fixtures().checks {
            #expect(ChatSelection.checks(item.criteria) == item.checks, "\(item.criteria)")
        }
    }

    @Test func sharedCombineCases() throws {
        for item in try fixtures().combine {
            let first = (ChatSelection.Verdict(rawValue: item.first[0]) ?? .unsure, item.first[1])
            let confirmations = item.confirmations.map { ChatSelection.Verdict(rawValue: $0) ?? .unsure }
            let (verdict, reason) = ChatSelection.combine(first, confirmations: confirmations)
            #expect(verdict.rawValue == item.answer && reason == item.reason, "\(item.first) \(item.confirmations)")
        }
    }

    @Test func confirmationsAskAboutTheWholeRequestThenEachPart() {
        let prompts = ChatSelection.confirmPrompts("woman in a pink shirt drinking from a water bottle", small: false)
        #expect(prompts.count == 3)
        #expect(prompts.first?.contains("\"woman in a pink shirt drinking from a water bottle\"") == true)
        #expect(prompts.dropFirst().first?.contains("\"woman in a pink shirt\"") == true)
        #expect(prompts.last?.contains("\"woman drinking from a water bottle\"") == true)
        #expect(prompts.allSatisfy { $0.contains("count") && !$0.contains("main subject") })
        let small = ChatSelection.confirmPrompts("close-up portrait", small: true)
        #expect(small.count == 1 && small[0].contains("main subject"))
        #expect(AIModelKind.qwen3VL2B.ramBudgetGB < ChatSelection.smallModelGB && AIModelKind.qwen3VL4B.ramBudgetGB >= ChatSelection.smallModelGB)
    }

    @Test func checkModelPrefersAnInstalledSmallModelThenTheBestThatFits() {
        let both: Set<AIModelKind> = [.qwen3VL2B, .qwen3VL4B]
        #expect(ChatSelection.checkModel(loaded: .qwen3VL2B, ramGB: 16, availableGB: 4, installed: both.contains) == .qwen3VL2B)
        #expect(ChatSelection.checkModel(loaded: nil, ramGB: 16, availableGB: 8, installed: both.contains) == .qwen3VL2B)
        #expect(ChatSelection.checkModel(loaded: .qwen3VL4B, ramGB: 16, availableGB: 0.5, installed: both.contains) == .qwen3VL2B)
        #expect(ChatSelection.checkModel(loaded: nil, ramGB: 16, availableGB: 8, installed: { $0 == .qwen3VL4B }) == .qwen3VL4B)
        #expect(ChatSelection.checkModel(loaded: .qwen3VL4B, ramGB: 16, availableGB: 0.5, installed: { $0 == .qwen3VL4B }) == .qwen3VL4B)
        #expect(ChatSelection.checkModel(loaded: nil, ramGB: 16, availableGB: 4, installed: { $0 == .qwen3VL4B }) == nil)
        #expect(ChatSelection.checkModel(loaded: nil, ramGB: 16, availableGB: 1, installed: both.contains) == nil)
        #expect(ChatSelection.checkModel(loaded: .qwen3VL2B, ramGB: 16, availableGB: 1, installed: { $0 == .qwen3VL2B }) == .qwen3VL2B)
        #expect(ChatSelection.checkModel(loaded: nil, ramGB: 16, availableGB: 8, installed: { _ in false }) == nil)
    }

    @Test func sharedVerdictCases() throws {
        for item in try fixtures().verdicts {
            let (verdict, reason) = ChatSelection.parseVerdict(item.raw)
            #expect(verdict.rawValue == item.answer && reason == item.reason, "\(item.raw)")
        }
    }

    @Test func sharedRankingCases() throws {
        for item in try fixtures().ranking {
            let candidates = item.candidates.map {
                ChatSelection.Candidate(id: $0.id, path: "/fixture/\($0.id).jpg", kind: $0.kind, evidence: $0.evidence, clip: $0.clip, people: $0.people)
            }
            let known = item.known.map { ChatSearchPlan.KnownPerson(id: $0.id, names: $0.names) }
            let ranked = ChatSelection.rank(candidates, criteria: ChatSelection.criteria(item.text), known: known)
            #expect(ranked.map(\.id) == item.order, "\(item.name)")
            #expect(ranked.map(\.score) == item.scores, "\(item.name)")
            #expect(ranked.map(ChatSelection.evidenceReason) == item.reasons, "\(item.name)")
        }
    }

    @Test func sharedMessageCases() throws {
        for item in try fixtures().messages {
            let a = item.args
            let message: String = switch item.kind {
            case "checked": ChatSelection.checkedMessage(yes: a[0], unsure: a[1], checked: a[2], total: a[3], noun: ChatSelection.noun(item.kinds, count: a[3]))
            case "evidence": ChatSelection.evidenceMessage(shown: a[0], total: a[1], noun: ChatSelection.noun(item.kinds, count: a[1]),
                                                           why: item.why == "off" ? .off : item.why == "notStarted" ? .notStarted : .noModel)
            case "progress": ChatSelection.progressMessage(checked: a[0], planned: a[1], total: a[2], noun: ChatSelection.noun(item.kinds, count: a[2]), yes: a[3])
            default: ChatSelection.stoppedMessage(checked: a[0], total: a[1], noun: ChatSelection.noun(item.kinds, count: a[1]))
            }
            #expect(message == item.message)
        }
    }

    @Test func unsureAndNoNeverCountAsMatches() {
        let ranked = { (id: Int64) in ChatSelection.Ranked(id: id, path: "/f/\(id).jpg", kind: "image", score: 0, matched: [], clip: false, people: false) }
        let hits = ChatSelection.ordered([(ranked(1), .unsure, "?"), (ranked(2), .no, ""), (ranked(3), .yes, "ok")])
        #expect(hits.map(\.fileID) == [3, 1])
        #expect(hits.map(\.match) == ["yes", "unsure"])
    }

    @Test func scopeNeverReadsOutsideTheSelection() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await db.pool.write { sql in
            for id in 1...6 {
                try sql.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed) VALUES(?,?,?,100,0,?,'jpg',?)",
                                arguments: [id, "/fixture/IMG_\(id).jpg", id, id == 5 ? "doc" : "image", id == 4 ? 1 : 0])
            }
            try sql.execute(sql: "INSERT INTO tags(file_id,tag,source,score) VALUES(1,'baseball','auto',0.9),(2,'baseball','auto',0.9),(3,'dog','auto',0.9),(4,'baseball','auto',0.9)")
        }
        let loaded = try await db.pool.read { sql in try ChatSelection.load(sql, scope: [3, 2, 4, 5, 99], root: nil, query: nil) }
        #expect(loaded.map(\.id) == [3, 2])
        #expect(ChatSelection.rank(loaded, criteria: "baseball", known: []).map(\.id) == [2, 3])
        #expect(try ChatSelection.validate([3, 2, 3]) == [3, 2])
        #expect(throws: (any Error).self) { try ChatSelection.validate([]) }
        #expect(throws: (any Error).self) { try ChatSelection.validate([0]) }
        #expect(throws: (any Error).self) { try ChatSelection.validate(Array(1...2001)) }

        let capture = WireCapture()
        let service = ChatService()
        await service.handle(ChatRequest(requestID: "s", conversationID: "c", action: "send", text: "baseball", useModel: false, scopeFileIDs: [3, 2]), database: db, sink: capture.sink)
        await capture.finish()
        try await Task.sleep(nanoseconds: 30_000_000)
        let events = try capture.bytes().split(separator: 10).map { try IPCCoder.decoder.decode(IPCEvent.self, from: Data($0)) }
        let reply = events.compactMap { event -> ChatResponse? in if case .chatResponse(let r) = event.payload { return r }; return nil }.last
        #expect(reply?.status == "completed")
        #expect(reply?.hits.map(\.fileID) == [2, 3])
        #expect(reply?.hits.allSatisfy { $0.match == "unsure" } == true)
        #expect(reply?.message.hasPrefix("Ranked 2 of 2 selected photos") == true)
        #expect(try await db.pool.read { sql in try Int.fetchOne(sql, sql: "SELECT COUNT(*) FROM catalog_chat") } == 2)
    }
}
