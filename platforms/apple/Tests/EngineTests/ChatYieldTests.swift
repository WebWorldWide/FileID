import Foundation
import Testing
@testable import FileIDEngine

@Suite("Chat yield")
struct ChatYieldTests {
    actor Log {
        var entries: [String] = []
        func add(_ entry: String) { entries.append(entry) }
    }

    @Test func parkedSummariesRunOnceInOrderBetweenFiles() async {
        let yield = ChatYield()
        let log = Log()
        await yield.park(id: "a") { interleaved in await log.add("a:\(interleaved)") }
        await yield.park(id: "b") { interleaved in await log.add("b:\(interleaved)") }
        await yield.drain()
        await yield.drain()
        #expect(await log.entries == ["a:true", "b:true"])
        #expect(await yield.isEmpty)
    }

    @Test func theQueueFallbackSkipsASummaryTheBatchAlreadyRan() async {
        let yield = ChatYield()
        let log = Log()
        await yield.park(id: "a") { _ in await log.add("ran") }
        await yield.drain()
        #expect(await yield.remove(id: "a") == false)
        #expect(await log.entries == ["ran"])
    }

    @Test func aSummaryTheQueueReachesFirstIsNotRunAgainByTheBatch() async {
        let yield = ChatYield()
        let log = Log()
        await yield.park(id: "a") { _ in await log.add("ran") }
        #expect(await yield.remove(id: "a") == true)
        await yield.drain()
        #expect(await log.entries.isEmpty)
    }
}
