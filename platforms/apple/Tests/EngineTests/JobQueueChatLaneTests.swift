import Testing
@testable import FileIDEngine

@Suite("Job queue chat lane")
struct JobQueueChatLaneTests {
    @Test func queuedChatSummaryDoesNotBlockDescribeCommands() async {
        let queue = JobQueue()
        let gate = AsyncGate()
        await queue.enqueue(.init(id: "chat-1", category: .deepAnalyze, title: "Chat", etaSeconds: nil, priority: .interactive, blocksExclusiveCommands: false) { await gate.wait() })
        #expect(await queue.hasActive(category: .deepAnalyze) == false)
        await queue.enqueue(.init(id: "describe", category: .deepAnalyze, title: "Describe", etaSeconds: nil) { await gate.wait() })
        #expect(await queue.hasActive(category: .deepAnalyze) == true)
        await gate.open()
    }
}

actor AsyncGate {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    func wait() async {
        if opened { return }
        await withCheckedContinuation { waiters.append($0) }
    }
    func open() {
        opened = true
        waiters.forEach { $0.resume() }
        waiters.removeAll()
    }
}
