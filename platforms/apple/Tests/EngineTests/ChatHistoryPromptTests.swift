import FileIDShared
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
