import Testing
import Foundation
@testable import FileIDEngine

@Suite struct JobQueueTests {
    private actor Recorder {
        var ids: [String] = []
        func add(_ id: String) { ids.append(id) }
    }

    @Test func interactiveJobsOvertakePendingBackgroundWorkAndKeepFifo() async throws {
        let queue = JobQueue(); let recorder = Recorder()
        let (stream, continuation) = AsyncStream<Void>.makeStream()
        await queue.enqueue(.init(id: "first", category: .scan, title: "First", etaSeconds: nil) {
            await recorder.add("first")
            for await _ in stream { break }
        })
        for _ in 0..<100 {
            if await recorder.ids.count == 1 { break }
            try await Task.sleep(nanoseconds: 5_000_000)
        }
        #expect(await recorder.ids == ["first"])
        for (id, priority) in [("background1", JobQueue.Job.Priority.background), ("background2", .background), ("interactive1", .interactive), ("interactive2", .interactive)] {
            await queue.enqueue(.init(id: id, category: .deepAnalyze, title: id, etaSeconds: nil, priority: priority) { await recorder.add(id) })
        }
        #expect(await queue.snapshot().pending.map(\.id) == ["interactive1", "interactive2", "background1", "background2"])
        await queue.cancelPending(id: "background2")
        continuation.yield(); continuation.finish()
        for _ in 0..<100 {
            if await queue.snapshot().isIdle { break }
            try await Task.sleep(nanoseconds: 5_000_000)
        }
        #expect(await recorder.ids == ["first", "interactive1", "interactive2", "background1"])
        #expect(await queue.snapshot().isIdle)
        await queue.enqueue(.init(id: "restart", category: .scan, title: "Restart", etaSeconds: nil) { await recorder.add("restart") })
        for _ in 0..<100 {
            if await recorder.ids.last == "restart" { break }
            try await Task.sleep(nanoseconds: 5_000_000)
        }
        #expect(await recorder.ids.last == "restart")
    }
}
