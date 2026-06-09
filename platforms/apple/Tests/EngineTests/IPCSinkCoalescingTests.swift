// H3 regression: when the IPC buffer is full (parent draining slowly),
// progress-event coalescing must never clobber a buffered critical event.
// The original bug matched on the literal '"' byte, so the overwrite hit
// ANY newest entry — including a buffered scanComplete, leaving the UI
// stuck mid-scan forever.
import Testing
import Foundation
@testable import FileIDEngine
import FileIDShared

private actor ByteCollector {
    private var data = Data()
    func append(_ d: Data) { data.append(d) }
    func snapshot() -> Data { data }
}

@Suite("IPCSink full-buffer coalescing (H3)")
struct IPCSinkCoalescingTests {

    @Test("Buffered scanComplete survives progress-flood coalescing")
    func scanCompleteSurvivesFlood() async throws {
        let pipe = Pipe()
        let sink = IPCSink(wire: pipe.fileHandleForWriting)
        let collector = ByteCollector()
        let reader = Task.detached {
            while true {
                let chunk = pipe.fileHandleForReading.availableData
                if chunk.isEmpty { break }
                await collector.append(chunk)
            }
        }

        let progress = ScanProgress(
            sessionID: "h3", phase: .tagging, total: 100, discovered: 100,
            processed: 1, failed: 0, filesPerSecond: 1, etaSeconds: nil,
            residentMB: 0, availableMB: 0
        )
        // Flood well past maxBuffer (1024) so the buffer is saturated and
        // the coalescing path runs, then land the critical event, then
        // keep flooding — the H3 bug overwrote it right here.
        for _ in 0..<2000 { await sink.emit(.progress(progress)) }
        await sink.emit(.scanComplete(ScanComplete(
            sessionID: "h3", totalFiles: 100, processedFiles: 100,
            failedFiles: 0, totalSeconds: 1
        )))
        for _ in 0..<300 { await sink.emit(.progress(progress)) }
        await sink.close()

        let needle = Data("\"scanComplete\"".utf8)
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            if await collector.snapshot().range(of: needle) != nil { break }
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        try? pipe.fileHandleForWriting.close()
        _ = await reader.value

        let out = await collector.snapshot()
        #expect(out.range(of: needle) != nil,
                "scanComplete must reach the wire even under full-buffer coalescing")
    }
}
