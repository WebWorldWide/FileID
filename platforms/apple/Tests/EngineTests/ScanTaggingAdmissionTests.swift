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
}
