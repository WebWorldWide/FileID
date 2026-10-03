import Testing
@testable import FileIDEngine

@Suite("Shared resource reservations")
struct ResourceSchedulerTests {
    @Test("competing work waits until the shared memory reservation is released")
    func reservationsShareCapacity() async throws {
        let scheduler = ResourceScheduler(memory: { (16_384, 16_384) })
        let first = try await scheduler.reserveMemory(8_000, priority: .background)
        let waiting = Task { try await scheduler.reserveMemory(8_000, priority: .interactive) }

        for _ in 0..<100 {
            if await scheduler.snapshot().waitingCount == 1 { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await scheduler.snapshot().waitingCount == 1)
        #expect(await scheduler.snapshot().reservedMemoryMB == 8_000)

        await scheduler.release(first)
        let second = try await waiting.value
        #expect(await scheduler.snapshot().reservedMemoryMB == 8_000)
        await scheduler.release(second)
        #expect(await scheduler.snapshot().reservedMemoryMB == 0)
    }

    @Test("CPU and storage reservations arbitrate independently of memory")
    func computeAndIOReservationsShareCapacity() async throws {
        let scheduler = ResourceScheduler(memory: { (16_384, 16_384) }, cpuCapacity: 1, ioCapacity: 1)
        let first = try await scheduler.reserve(
            ResourceScheduler.Demand(cpuUnits: 1, ioUnits: 1), priority: .background
        )
        let waiting = Task {
            try await scheduler.reserve(ResourceScheduler.Demand(cpuUnits: 1, ioUnits: 1), priority: .interactive)
        }

        for _ in 0..<100 {
            if await scheduler.snapshot().waitingCount == 1 { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await scheduler.snapshot().reservedCPUUnits == 1)
        #expect(await scheduler.snapshot().reservedIOUnits == 1)
        #expect(await scheduler.snapshot().waitingCount == 1)

        await scheduler.release(first)
        let second = try await waiting.value
        #expect(await scheduler.snapshot().reservedCPUUnits == 1)
        #expect(await scheduler.snapshot().reservedIOUnits == 1)
        await scheduler.release(second)
        #expect(await scheduler.snapshot().reservedCPUUnits == 0)
        #expect(await scheduler.snapshot().reservedIOUnits == 0)
    }

    @Test("a resident model keeps memory after model-loading capacity is released")
    func residentModelRetainsOnlyMemory() async throws {
        let scheduler = ResourceScheduler(memory: { (16_384, 16_384) }, cpuCapacity: 2, ioCapacity: 1)
        let model = try await scheduler.reserve(
            ResourceScheduler.Demand(memoryMB: 8_000, cpuUnits: 1, ioUnits: 1), priority: .interactive
        )

        await scheduler.retainMemory(model)
        let state = await scheduler.snapshot()
        #expect(state.reservedMemoryMB == 8_000)
        #expect(state.reservedCPUUnits == 0)
        #expect(state.reservedIOUnits == 0)

        let index = try await scheduler.reserve(
            ResourceScheduler.Demand(memoryMB: 2_000, cpuUnits: 1, ioUnits: 1), priority: .background
        )
        #expect(await scheduler.snapshot().reservedMemoryMB == 10_000)
        await scheduler.release(index)
        await scheduler.release(model)
    }

    @Test("interactive work overtakes queued background work without leaving it stuck")
    func interactivePriorityDefersBlockedBackgroundWork() async throws {
        let scheduler = ResourceScheduler(memory: { (16_384, 16_384) }, cpuCapacity: 1, ioCapacity: 1)
        let active = try await scheduler.reserve(
            ResourceScheduler.Demand(cpuUnits: 1), priority: .background
        )
        let background = Task {
            try await scheduler.reserve(ResourceScheduler.Demand(cpuUnits: 1), priority: .background)
        }
        for _ in 0..<100 {
            if await scheduler.snapshot().waitingCount == 1 { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        let interactive = Task {
            try await scheduler.reserve(ResourceScheduler.Demand(cpuUnits: 1), priority: .interactive)
        }
        for _ in 0..<100 {
            if await scheduler.snapshot().waitingCount == 2 { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await scheduler.snapshot().waitingCount == 2)

        await scheduler.release(active)
        let interactiveLease = try await interactive.value
        await #expect(throws: ResourceScheduler.AdmissionError.insufficientResources) {
            try await background.value
        }
        #expect(await scheduler.snapshot().reservedCPUUnits == 1)
        await scheduler.release(interactiveLease)
    }

    @Test("cancelling a queued reservation removes only that waiter")
    func cancellationRemovesWaiter() async throws {
        let scheduler = ResourceScheduler(memory: { (16_384, 16_384) })
        let first = try await scheduler.reserveMemory(8_000, priority: .background)
        let waiting = Task { try await scheduler.reserveMemory(8_000, priority: .interactive) }

        for _ in 0..<100 {
            if await scheduler.snapshot().waitingCount == 1 { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await scheduler.snapshot().waitingCount == 1)

        waiting.cancel()
        await #expect(throws: CancellationError.self) { try await waiting.value }
        #expect(await scheduler.snapshot().waitingCount == 0)
        #expect(await scheduler.snapshot().reservedMemoryMB == 8_000)

        await scheduler.release(first)
        let next = try await scheduler.reserveMemory(8_000, priority: .background)
        #expect(await scheduler.snapshot().reservedMemoryMB == 8_000)
        await scheduler.release(next)
    }

    @Test("background indexing defers when resident interactive work owns the available budget")
    func backgroundWorkDefersForResidentModel() async throws {
        let scheduler = ResourceScheduler(memory: { (16_384, 16_384) })
        let model = try await scheduler.reserveMemory(11_000, priority: .interactive)

        await #expect(throws: ResourceScheduler.AdmissionError.insufficientAvailableMemory) {
            try await scheduler.reserveMemory(2_000, priority: .background)
        }
        #expect(await scheduler.snapshot().reservedMemoryMB == 11_000)
        #expect(await scheduler.snapshot().waitingCount == 0)

        await scheduler.release(model)
        let index = try await scheduler.reserveMemory(2_000, priority: .background)
        await scheduler.release(index)
    }
}
