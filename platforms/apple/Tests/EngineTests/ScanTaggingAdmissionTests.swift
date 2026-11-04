import Foundation
import Testing
@testable import FileIDEngine

@Suite("Scan tagging resource admission")
struct ScanTaggingAdmissionTests {
    @Test("scan tagging reports time blocked by interactive CPU work")
    func reportsInteractiveWait() async throws {
        let scheduler = ResourceScheduler(
            memory: { (16_384, 16_384) },
            cpuCapacity: 1,
            ioCapacity: 1
        )
        let interactive = try await scheduler.reserve(
            ResourceScheduler.Demand(cpuUnits: 1),
            priority: .interactive
        )
        let started = AsyncStream.makeStream(of: Void.self)
        let scan = Task {
            started.continuation.yield(())
            return try await ScanTaggingAdmission.withBackgroundCPU(
                scheduler: scheduler,
                isCancelled: { false }
            ) { waitMs in
                waitMs
            }
        }

        for await _ in started.stream { break }
        try await Task.sleep(for: .milliseconds(25))
        await scheduler.release(interactive)

        let waitMs = try await scan.value
        #expect(waitMs >= 50)
        #expect(await scheduler.snapshot().reservedCPUUnits == 0)
    }

    @Test("two scan files share one CPU unit, and a third waits")
    func scanFilesReserveHalfACore() async throws {
        let scheduler = ResourceScheduler(memory: { (16_384, 16_384) }, cpuCapacity: 1, ioCapacity: 1)
        let first = try await scheduler.reserve(.acceleratedHalfCore, priority: .background)
        let second = try await scheduler.reserve(.acceleratedHalfCore, priority: .background)
        #expect(await scheduler.snapshot().reservedCPUUnits == 1)
        #expect(await scheduler.snapshot().activeReservations == 2)

        let third = Task { try await scheduler.reserve(.acceleratedHalfCore, priority: .background) }
        try await waitForWaiters(1, scheduler)
        await scheduler.release(first)
        let thirdLease = try await third.value

        let whole = Task { try await scheduler.reserve(ResourceScheduler.Demand(cpuUnits: 1), priority: .interactive) }
        try await waitForWaiters(1, scheduler)
        await scheduler.release(second)
        try await Task.sleep(for: .milliseconds(20))
        #expect(await scheduler.snapshot().waitingCount == 1)
        await scheduler.release(thirdLease)
        let wholeLease = try await whole.value
        #expect(await scheduler.snapshot().reservedCPUUnits == 1)
        await scheduler.release(wholeLease)
        #expect(await scheduler.snapshot().reservedCPUUnits == 0)
    }

    private func waitForWaiters(_ count: Int, _ scheduler: ResourceScheduler) async throws {
        for _ in 0..<100 {
            if await scheduler.snapshot().waitingCount == count { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await scheduler.snapshot().waitingCount == count)
    }
}
