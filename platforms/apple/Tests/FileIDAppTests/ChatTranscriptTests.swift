import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Chat transcript")
struct ChatTranscriptTests {
    private func message(_ id: String, _ role: String = "assistant") -> ChatMessage {
        ChatMessage(id: id, role: role, text: "text", createdAt: 0)
    }

    private func action(_ id: String) -> ChatAction {
        ChatAction(id: id, kind: "rescan", title: "Rescan the library", detail: "Checks for new files.", requiresConfirmation: false, source: "parser")
    }

    @Test func aCardArrivingAfterTheAnswerChangesTheTail() {
        let answer = [message("u1", "user"), message("a1")]
        let before = ChatTranscriptTail(messages: answer, actions: [], busy: false)
        #expect(ChatTranscriptTail(messages: answer, actions: [action("card")], busy: false) != before)
        #expect(ChatTranscriptTail(messages: answer, actions: [], busy: true) != before)
        #expect(ChatTranscriptTail(messages: answer + [message("a2")], actions: [], busy: false) != before)
        #expect(ChatTranscriptTail(messages: answer, actions: [], busy: false) == before)
    }

    @Test func aCardDoesNotRepeatTheAnswerAboveIt() {
        #expect(ChatActionCard.repeatsAnswer(detail: "Opens Duplicates.", answer: "Opens Duplicates. "))
        #expect(!ChatActionCard.repeatsAnswer(detail: "Opens Duplicates.", answer: "Suggest a layout for 2012."))
        #expect(!ChatActionCard.repeatsAnswer(detail: "Opens Duplicates.", answer: nil))
    }
}
