import Foundation
import Testing
@testable import FileIDEngine

@Suite("Decode-ahead queue")
struct DecodeAheadQueueTests {
    private final class Dropped: @unchecked Sendable {
        private let lock = NSLock()
        private var items: [Int] = []
        func add(_ item: Int) { lock.lock(); items.append(item); lock.unlock() }
        var all: [Int] { lock.lock(); defer { lock.unlock() }; return items }
    }

    private final class Flag: @unchecked Sendable {
        private let lock = NSLock()
        private var value = false
        func set() { lock.lock(); value = true; lock.unlock() }
        var isSet: Bool { lock.lock(); defer { lock.unlock() }; return value }
    }

    private static func settle() async {
        try? await Task.sleep(nanoseconds: 40_000_000)
    }

    @Test("items leave in the order they were filled, not the order they were reserved")
    func fillOrder() async throws {
        let queue = DecodeAheadQueue<Int>(capacity: 4, budgetBytes: 1000)
        let a = try #require(await queue.reserve(bytes: 10))
        let b = try #require(await queue.reserve(bytes: 10))
        let c = try #require(await queue.reserve(bytes: 10))
        queue.fill(b, 2, bytes: 10)
        queue.fill(c, 3, bytes: 10)
        queue.fill(a, 1, bytes: 10)
        queue.finish()
        #expect(await queue.next() == 2)
        #expect(await queue.next() == 3)
        #expect(await queue.next() == 1)
        #expect(await queue.next() == nil)
        #expect(queue.held.count == 0 && queue.held.bytes == 0)
    }

    @Test("a full queue holds the next reservation back until an item is taken")
    func countBackPressure() async throws {
        let queue = DecodeAheadQueue<Int>(capacity: 2, budgetBytes: 1000)
        let a = try #require(await queue.reserve(bytes: 1))
        let b = try #require(await queue.reserve(bytes: 1))
        queue.fill(a, 1, bytes: 1)
        queue.fill(b, 2, bytes: 1)
        let granted = Flag()
        let third = Task { () -> Bool in
            let ticket = await queue.reserve(bytes: 1)
            granted.set()
            return ticket != nil
        }
        await Self.settle()
        #expect(!granted.isSet, "two items are held: no room for a third")
        #expect(queue.held.count == 2)
        #expect(await queue.next() == 1)
        #expect(await third.value)
        #expect(queue.held.count == 2)
        #expect(queue.stats.reserveWaits == 1)
        #expect(queue.stats.peakHeld == 2)
    }

    @Test("the byte budget holds reservations back, and a smaller real frame frees room")
    func byteBackPressure() async throws {
        let queue = DecodeAheadQueue<Int>(capacity: 8, budgetBytes: 100)
        let a = try #require(await queue.reserve(bytes: 60))
        let granted = Flag()
        let second = Task { () -> DecodeAheadQueue<Int>.Ticket? in
            let ticket = await queue.reserve(bytes: 60)
            granted.set()
            return ticket
        }
        await Self.settle()
        #expect(!granted.isSet, "60 + 60 is over the 100-byte budget")
        queue.fill(a, 1, bytes: 30)
        let b = try #require(await second.value)
        #expect(queue.held.bytes == 90)
        queue.fill(b, 2, bytes: 60)
        #expect(queue.stats.peakBytes <= 100)
        #expect(await queue.next() == 1)
        #expect(queue.held.bytes == 60)
    }

    @Test("one item larger than the whole budget is still admitted when nothing is held")
    func oversizedItemAlone() async throws {
        let queue = DecodeAheadQueue<Int>(capacity: 4, budgetBytes: 10)
        let ticket = try #require(await queue.reserve(bytes: 500))
        queue.fill(ticket, 7, bytes: 500)
        #expect(await queue.next() == 7)
    }

    @Test("an idle consumer makes room for one more decode")
    func idleConsumerAddsRoom() async throws {
        let queue = DecodeAheadQueue<Int>(capacity: 1, budgetBytes: 100)
        let decoding = try #require(await queue.reserve(bytes: 80))
        let granted = Flag()
        let second = Task { () -> DecodeAheadQueue<Int>.Ticket? in
            let ticket = await queue.reserve(bytes: 80)
            granted.set()
            return ticket
        }
        await Self.settle()
        #expect(!granted.isSet)
        let consumer = Task { await queue.next() }
        let extra = try #require(await second.value)
        queue.fill(extra, 9, bytes: 80)
        #expect(await consumer.value == 9, "the waiting consumer gets the first item filled")
        queue.fill(decoding, 8, bytes: 80)
        #expect(await queue.next() == 8)
        let stats = queue.stats
        #expect(stats.misses == 1 && stats.hits == 1)
    }

    @Test("finish lets consumers drain, and waits for a decode still under way")
    func finishDrains() async throws {
        let queue = DecodeAheadQueue<Int>(capacity: 2, budgetBytes: 100)
        let pending = try #require(await queue.reserve(bytes: 1))
        queue.finish()
        #expect(await queue.reserve(bytes: 1) == nil, "nothing is reserved after finish")
        let consumer = Task { await queue.next() }
        await Self.settle()
        queue.fill(pending, 5, bytes: 1)
        #expect(await consumer.value == 5)
        #expect(await queue.next() == nil)
    }

    @Test("an abandoned reservation ends the queue for waiting consumers")
    func abandonReleases() async throws {
        let queue = DecodeAheadQueue<Int>(capacity: 1, budgetBytes: 100)
        let ticket = try #require(await queue.reserve(bytes: 50))
        queue.finish()
        let consumer = Task { await queue.next() }
        await Self.settle()
        queue.abandon(ticket)
        #expect(await consumer.value == nil)
        #expect(queue.held.count == 0 && queue.held.bytes == 0)
    }

    @Test("cancel drops held items, wakes everyone waiting and discards late fills")
    func cancelDropsEverything() async throws {
        let dropped = Dropped()
        let queue = DecodeAheadQueue<Int>(capacity: 2, budgetBytes: 100) { dropped.add($0) }
        let a = try #require(await queue.reserve(bytes: 10))
        let late = try #require(await queue.reserve(bytes: 10))
        queue.fill(a, 1, bytes: 10)
        let blockedProducer = Task { await queue.reserve(bytes: 10) }
        await Self.settle()
        queue.cancel()
        #expect(await blockedProducer.value == nil)
        #expect(await queue.next() == nil)
        #expect(await queue.reserve(bytes: 1) == nil)
        queue.fill(late, 2, bytes: 10)
        #expect(dropped.all == [1, 2], "the held frame and the one decoded after the cancel are both released")
        #expect(queue.held.count == 0 && queue.held.bytes == 0)

        let empty = DecodeAheadQueue<Int>(capacity: 1, budgetBytes: 100)
        let idle = Task { await empty.next() }
        await Self.settle()
        empty.cancel()
        #expect(await idle.value == nil, "a consumer parked on an empty queue is released")
    }

    @Test("a cancelled task stops waiting promptly and leaves the queue usable")
    func taskCancellation() async throws {
        let queue = DecodeAheadQueue<Int>(capacity: 1, budgetBytes: 100)
        let held = try #require(await queue.reserve(bytes: 1))
        queue.fill(held, 1, bytes: 1)
        let producer = Task { await queue.reserve(bytes: 1) }
        await Self.settle()
        producer.cancel()
        #expect(await producer.value == nil)
        #expect(await queue.next() == 1)

        let consumer = Task { await queue.next() }
        await Self.settle()
        consumer.cancel()
        #expect(await consumer.value == nil)

        let again = try #require(await queue.reserve(bytes: 1))
        queue.fill(again, 2, bytes: 1)
        #expect(await queue.next() == 2)

        let already = Task { () -> Bool in
            withUnsafeCurrentTask { $0?.cancel() }
            let ticket = await queue.reserve(bytes: 1)
            let item = await queue.next()
            return ticket == nil && item == nil
        }
        #expect(await already.value)
        #expect(queue.held.count == 0)
    }

    @Test("hits, misses and average depth say whether consumers ever waited")
    func statistics() async throws {
        let queue = DecodeAheadQueue<Int>(capacity: 4, budgetBytes: 1000)
        for value in 1...3 {
            let ticket = try #require(await queue.reserve(bytes: 1))
            queue.fill(ticket, value, bytes: 1)
        }
        for value in 1...3 { #expect(await queue.next() == value) }
        let waiting = Task { await queue.next() }
        await Self.settle()
        let ticket = try #require(await queue.reserve(bytes: 1))
        queue.fill(ticket, 4, bytes: 1)
        #expect(await waiting.value == 4)
        let stats = queue.stats
        #expect(stats.hits == 3 && stats.misses == 1 && stats.asks == 4)
        #expect(stats.depthSum == 3 + 2 + 1 + 0)
        #expect(stats.averageDepth == 1.5)
        #expect(stats.takeWaitSeconds > 0)
    }

    @Test("FILEID_DECODE_AHEAD picks the depth; unset follows the default constant")
    func depthSwitch() {
        #expect(ScanDecodeAhead.depth(workerCap: 14, environment: ["FILEID_DECODE_AHEAD": "0"]) == 0)
        #expect(ScanDecodeAhead.depth(workerCap: 14, environment: ["FILEID_DECODE_AHEAD": "-3"]) == 0)
        #expect(ScanDecodeAhead.depth(workerCap: 14, environment: ["FILEID_DECODE_AHEAD": " 6 "]) == 6)
        #expect(ScanDecodeAhead.depth(workerCap: 14, environment: ["FILEID_DECODE_AHEAD": "500"]) == ScanDecodeAhead.maxDepth)
        let unset = ScanDecodeAhead.depth(workerCap: 14, environment: [:])
        #expect(unset == (ScanDecodeAhead.enabledByDefault ? 7 : 0))
        #expect(ScanDecodeAhead.depth(workerCap: 14, environment: ["FILEID_DECODE_AHEAD": "on"]) == unset)
        #expect(ScanDecodeAhead.automaticDepth(workerCap: 14) == 7)
        #expect(ScanDecodeAhead.automaticDepth(workerCap: 32) == 8)
        #expect(ScanDecodeAhead.automaticDepth(workerCap: 1) == 1)
        #expect(ScanDecodeAhead.budgetBytes(environment: [:]) == ScanDecodeAhead.defaultBudgetMB << 20)
        #expect(ScanDecodeAhead.budgetBytes(environment: ["FILEID_DECODE_AHEAD_MB": "200"]) == 200 << 20)
        #expect(ScanDecodeAhead.budgetBytes(environment: ["FILEID_DECODE_AHEAD_MB": "1"]) == 16 << 20)
    }
}
