// Re-audit R-15 regression: the F-C3-029/030 terminal-pinning fix pinned
// restructureApplyResult but omitted restructurePlan — the success-path terminal
// reply for the planRestructure flow. Its error twin (plan_restructure_failed)
// rides .error and was already pinned, so a FAILED plan always reached the app
// while a SUCCESSFUL plan could be evicted under buffer pressure, stranding the
// Restructure tab in "Computing plan…". restructurePlan must be in both the
// isCritical switch and the criticalNeedles byte-scan.
import Testing
import Foundation
@testable import FileIDEngine
import FileIDShared

private actor ByteSink {
    private var data = Data()
    func append(_ d: Data) { data.append(d) }
    func snapshot() -> Data { data }
}

@Suite("IPCSink restructurePlan pinning (R-15)")
struct IPCSinkRestructurePlanPinTests {

    private static func plan() -> IPCEvent.Payload {
        .restructurePlan(RestructurePlan(libraryRoot: "/lib", moves: [], categoryCounts: []))
    }

    @Test("restructurePlan is classified critical and detected in serialized form")
    func restructurePlanIsCritical() throws {
        let payload = Self.plan()
        #expect(IPCSink.isCritical(payload),
                "restructurePlan must be pinned — it is the planRestructure success terminal")
        let encoded = try IPCCoder.encodeLine(IPCEvent(payload: payload))
        #expect(IPCSink.entryLooksCritical(encoded),
                "serialized restructurePlan must be recognized by the eviction byte-scan")
    }

    @Test("restructurePlan reaches the wire under a full-buffer progress flood")
    func restructurePlanSurvivesFlood() async throws {
        let pipe = Pipe()
        let sink = IPCSink(wire: pipe.fileHandleForWriting)
        let collector = ByteSink()
        let reader = Task.detached {
            while true {
                let chunk = pipe.fileHandleForReading.availableData
                if chunk.isEmpty { break }
                await collector.append(chunk)
            }
        }

        let progress = ScanProgress(
            sessionID: "flood", phase: .tagging, total: 100, discovered: 100,
            processed: 1, failed: 0, filesPerSecond: 1, etaSeconds: nil,
            residentMB: 0, availableMB: 0)
        // Saturate past maxBuffer (1024) so the eviction path runs, land the plan
        // mid-flood, keep flooding — the pre-fix code (restructurePlan omitted
        // from the critical set) could evict it right here.
        for _ in 0..<2000 { await sink.emit(.progress(progress)) }
        await sink.emit(Self.plan())
        for _ in 0..<500 { await sink.emit(.progress(progress)) }
        await sink.close()

        let needle = Data("\"restructurePlan\"".utf8)
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            if await collector.snapshot().range(of: needle) != nil { break }
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        try? pipe.fileHandleForWriting.close()
        _ = await reader.value

        let out = await collector.snapshot()
        #expect(out.range(of: needle) != nil,
                "restructurePlan must reach the wire under full-buffer pressure")
    }
}
