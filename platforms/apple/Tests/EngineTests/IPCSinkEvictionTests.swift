// F-C3-029/030 regression: the IPCSink full-buffer eviction must pin EVERY
// terminal completion (scanComplete, deepAnalyzeComplete, faceClusteringComplete,
// restructureApplyResult, error) — not just scan completion — and must never
// drop one via the old criticality-blind removeFirst(). The original critical
// set omitted deepAnalyzeComplete + restructureApplyResult, so under buffer
// pressure those terminals could be evicted and the Deep Analyze / Restructure
// tabs would never leave their in-flight state.
import Testing
import Foundation
@testable import FileIDEngine
import FileIDShared

private actor ByteSink {
    private var data = Data()
    func append(_ d: Data) { data.append(d) }
    func snapshot() -> Data { data }
}

@Suite("IPCSink terminal-event pinning (F-C3-029/030)")
struct IPCSinkEvictionTests {

    private static func line(_ p: IPCEvent.Payload) throws -> Data {
        try IPCCoder.encodeLine(IPCEvent(payload: p))
    }

    private static let allTerminals: [(name: String, payload: IPCEvent.Payload)] = [
        ("scanComplete", .scanComplete(ScanComplete(
            sessionID: "s", totalFiles: 1, processedFiles: 1, failedFiles: 0, totalSeconds: 1))),
        ("deepAnalyzeComplete", .deepAnalyzeComplete(DeepAnalyzeComplete(
            processed: 1, failed: 0, totalSeconds: 1, modelKind: "qwen2.5-vl-7b", cancelled: false))),
        ("faceClusteringComplete", .faceClusteringComplete(FaceClusteringResult(
            personCount: 1, faceCount: 2, unmatchedFaces: 0, durationSeconds: 1))),
        ("restructureApplyResult", .restructureApplyResult(RestructureApplyResult(
            applied: 3, failed: 0, privilegeError: nil))),
        ("error", .error(EngineError(kind: "boom", message: "boom"))),
    ]

    @Test("every terminal completion is classified critical (pinned)")
    func everyTerminalIsCritical() throws {
        for t in Self.allTerminals {
            #expect(IPCSink.isCritical(t.payload),
                    "\(t.name) must be pinned so eviction can never drop it")
            // …and its serialized form must be recognized by the byte-needle
            // scan the eviction path uses to skip pinned entries.
            let encoded = try Self.line(t.payload)
            #expect(IPCSink.entryLooksCritical(encoded),
                    "serialized \(t.name) must be detected as critical-looking")
        }
    }

    @Test("progress is NOT pinned (it is the evictable / coalescible class)")
    func progressIsEvictable() throws {
        let progress = IPCEvent.Payload.progress(ScanProgress(
            sessionID: "p", phase: .tagging, total: 100, discovered: 100,
            processed: 1, failed: 0, filesPerSecond: 1, etaSeconds: nil,
            residentMB: 0, availableMB: 0))
        let encoded = try Self.line(progress)
        #expect(!IPCSink.isCritical(progress))
        #expect(!IPCSink.entryLooksCritical(encoded))
    }

    @Test("deepAnalyzeComplete + restructureApplyResult reach the wire under a progress flood")
    func newTerminalsSurviveFlood() async throws {
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
        // Saturate well past maxBuffer (1024) so the eviction path runs, land
        // the two newly-pinned terminals mid-flood, then keep flooding — the
        // pre-fix code (these two omitted from the critical set) could evict
        // them right here.
        for _ in 0..<2000 { await sink.emit(.progress(progress)) }
        await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
            processed: 9, failed: 0, totalSeconds: 2, modelKind: "qwen2.5-vl-7b", cancelled: false)))
        for _ in 0..<300 { await sink.emit(.progress(progress)) }
        await sink.emit(.restructureApplyResult(RestructureApplyResult(
            applied: 7, failed: 1, privilegeError: nil)))
        for _ in 0..<300 { await sink.emit(.progress(progress)) }
        await sink.close()

        let dac = Data("\"deepAnalyzeComplete\"".utf8)
        let rar = Data("\"restructureApplyResult\"".utf8)
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            let out = await collector.snapshot()
            if out.range(of: dac) != nil && out.range(of: rar) != nil { break }
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        try? pipe.fileHandleForWriting.close()
        _ = await reader.value

        let out = await collector.snapshot()
        #expect(out.range(of: dac) != nil,
                "deepAnalyzeComplete must reach the wire under full-buffer pressure")
        #expect(out.range(of: rar) != nil,
                "restructureApplyResult must reach the wire under full-buffer pressure")
    }

    // F-C3-040: the shutdown path calls drainAndClose() before Darwin._exit(0)
    // so a buffered terminal event isn't dropped on the hard exit.
    @Test("drainAndClose flushes a buffered terminal event and then closes the sink")
    func drainAndCloseFlushesThenCloses() async throws {
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

        await sink.emit(.faceClusteringComplete(FaceClusteringResult(
            personCount: 1, faceCount: 1, unmatchedFaces: 0, durationSeconds: 1)))
        // The shutdown-path guarantee: flush remaining buffer, then close.
        await sink.drainAndClose()
        // After close, further emits must be dropped (proves the close took).
        await sink.emit(.error(EngineError(kind: "after_close", message: "should not appear")))

        let want = Data("\"faceClusteringComplete\"".utf8)
        let deadline = Date().addingTimeInterval(5)
        while Date() < deadline {
            if await collector.snapshot().range(of: want) != nil { break }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        try? pipe.fileHandleForWriting.close()
        _ = await reader.value

        let out = await collector.snapshot()
        #expect(out.range(of: want) != nil,
                "drainAndClose must flush the buffered terminal event before exit")
        #expect(out.range(of: Data("\"after_close\"".utf8)) == nil,
                "emits after drainAndClose must be dropped — the sink is closed")
    }
}
