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

@Suite("Job queue idle wait")
struct JobQueueIdleWaitTests {
    @Test func aFinishingJobReleasesItsSlotWithinTheGraceWindow() async {
        let queue = JobQueue()
        await queue.enqueue(.init(id: "scan", category: .scan, title: "Scan", etaSeconds: nil) { try? await Task.sleep(for: .milliseconds(150)) })
        #expect(await queue.waitUntilIdle(category: .scan, timeout: .seconds(2)))
    }

    @Test func aStillRunningJobIsReportedBusyAfterTheTimeout() async {
        let queue = JobQueue()
        let gate = AsyncGate()
        await queue.enqueue(.init(id: "scan", category: .scan, title: "Scan", etaSeconds: nil) { await gate.wait() })
        #expect(await queue.waitUntilIdle(category: .scan, timeout: .milliseconds(120)) == false)
        await gate.open()
        #expect(await queue.waitUntilIdle(category: .scan, timeout: .seconds(2)))
    }
}
