import Foundation
import Testing
@testable import FileIDEngine

@Suite struct ModelResidencyTests {
    @Test func sharedMemoryAdmissionReservesSystemHeadroom() throws {
        struct Fixture: Decodable { let totalMB: UInt64; let availableMB: UInt64; let requestedMB: UInt64; let accepted: Bool }
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let fixtures = try JSONDecoder().decode([Fixture].self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/model-memory-admission.json")))
        for fixture in fixtures {
            #expect((ModelMemoryAdmission.rejection(totalMB: fixture.totalMB, availableMB: fixture.availableMB, requestedMB: fixture.requestedMB) == nil) == fixture.accepted)
        }
    }

    @Test func modelSwitchWaitsForInferenceAndCancellationDoesNotStrandQueue() async throws {
        let gate = ExclusiveResourceGate()
        let active = try await gate.acquire()
        let switcher = Task { try await gate.acquire() }
        try await waitForQueue(gate, count: 1)
        let cancelled = Task { try await gate.acquire() }
        try await waitForQueue(gate, count: 2)
        cancelled.cancel()
        do { _ = try await cancelled.value; Issue.record("Cancelled admission succeeded") }
        catch { #expect(error is CancellationError) }
        #expect(await gate.snapshot().occupied)
        #expect(await gate.snapshot().waiting == 1)
        await gate.release(UUID())
        #expect(await gate.snapshot().waiting == 1)
        await gate.release(active)
        let writer = try await switcher.value
        #expect(await gate.snapshot().occupied)
        let reader = Task { try await gate.acquire() }
        try await waitForQueue(gate, count: 1)
        await gate.release(writer)
        let next = try await reader.value
        await gate.release(next)
        #expect(await gate.snapshot().occupied == false)
        #expect(await gate.snapshot().waiting == 0)
    }

    @Test func alreadyCancelledAdmissionsLeaveCapacityAvailable() async throws {
        let gate = ExclusiveResourceGate()
        let task = Task {
            withUnsafeCurrentTask { $0?.cancel() }
            return try await gate.acquire()
        }
        do { _ = try await task.value; Issue.record("Cancelled task acquired model resources") }
        catch { #expect(error is CancellationError) }
        let token = try await gate.acquire()
        await gate.release(token)
        #expect(await gate.snapshot().occupied == false)
    }

    @Test func cancellationDuringHandoffDoesNotLeakModelCapacity() async throws {
        let gate = ExclusiveResourceGate()
        for _ in 0..<32 {
            let owner = try await gate.acquire()
            let queued = Task { try await gate.acquire() }
            try await waitForQueue(gate, count: 1)
            queued.cancel()
            await gate.release(owner)
            do { _ = try await queued.value; Issue.record("Cancelled handoff succeeded") }
            catch { #expect(error is CancellationError) }
            let next = try await gate.acquire()
            await gate.release(next)
            #expect(await gate.snapshot().occupied == false)
            #expect(await gate.snapshot().waiting == 0)
        }
    }

    private func waitForQueue(_ gate: ExclusiveResourceGate, count: Int) async throws {
        for _ in 0..<5000 {
            if await gate.snapshot().waiting == count { return }
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        throw NSError(domain: "FileID.ModelResidencyTests", code: 1)
    }
}
