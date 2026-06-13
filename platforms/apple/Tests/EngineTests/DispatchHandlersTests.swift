// F-C3-032 + F-C3-021-wiring regression for the engine command dispatcher.
//
// 032: startScan rejected for a db-unavailable engine must emit a scan-TERMINAL
// event (scanComplete), not only an error — otherwise the app's auto-pilot is
// stranded on "Scanning…" forever (it advances only on a scan-terminal event).
//
// 021-wiring: planRestructure / applyRestructure must call the engine butler
// (Restructure.proposeAll / Restructure.apply) and emit restructurePlan /
// restructureApplyResult — not the old not_implemented_yet error.
import Testing
import Foundation
@testable import FileIDEngine
import FileIDShared

private actor WireBytes {
    private var data = Data()
    func append(_ d: Data) { data.append(d) }
    func snapshot() -> Data { data }
}

@Suite("Engine dispatch handlers (F-C3-032/021)", .serialized)
struct DispatchHandlersTests {

    private func pipeSink() -> (IPCSink, WireBytes, Pipe, Task<Void, Never>) {
        let pipe = Pipe()
        let sink = IPCSink(wire: pipe.fileHandleForWriting)
        let collector = WireBytes()
        let reader = Task.detached {
            while true {
                let chunk = pipe.fileHandleForReading.availableData
                if chunk.isEmpty { break }
                await collector.append(chunk)
            }
        }
        return (sink, collector, pipe, reader)
    }

    private func waitFor(_ needles: [Data], in collector: WireBytes,
                         timeout: TimeInterval = 10) async -> Data {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            let out = await collector.snapshot()
            if needles.allSatisfy({ out.range(of: $0) != nil }) { return out }
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        return await collector.snapshot()
    }

    @Test("startScan with no database emits a terminal scanComplete, not just an error")
    func startScanDbUnavailableEmitsTerminal() async throws {
        let (sink, collector, pipe, reader) = pipeSink()
        let cmd = IPCCommand(payload: .startScan(
            rootPath: "/tmp/does-not-matter", rootDisplay: nil, rescan: false))

        await FileIDEngineMain.dispatch(cmd, coordinator: ScanCoordinator(),
                                        sink: sink, database: nil)

        let errNeedle = Data("\"db_unavailable\"".utf8)
        let doneNeedle = Data("\"scanComplete\"".utf8)
        let out = await waitFor([errNeedle, doneNeedle], in: collector)
        await sink.close()
        try? pipe.fileHandleForWriting.close()
        _ = await reader.value

        #expect(out.range(of: errNeedle) != nil, "db_unavailable error must still be emitted")
        #expect(out.range(of: doneNeedle) != nil,
                "a scan-terminal event must follow so the app leaves the scanning state")
    }

    @Test("planRestructure / applyRestructure round-trip through the engine butler")
    func restructureRoundTrip() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDRestructure-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))

        let (sink, collector, pipe, reader) = pipeSink()

        // Empty library → an empty (but real) plan. Proves the dead IPC is wired
        // to proposeAll instead of returning not_implemented_yet.
        await FileIDEngineMain.dispatch(
            IPCCommand(payload: .planRestructure(libraryRoot: tmp.path)),
            coordinator: ScanCoordinator(), sink: sink, database: db)
        let planNeedle = Data("\"restructurePlan\"".utf8)
        let notImpl = Data("\"not_implemented_yet\"".utf8)
        var out = await waitFor([planNeedle], in: collector)
        #expect(out.range(of: planNeedle) != nil, "planRestructure must emit a restructurePlan event")
        #expect(out.range(of: notImpl) == nil, "planRestructure must no longer be not_implemented_yet")

        // applyRestructure with no moves → a real (zero) result.
        await FileIDEngineMain.dispatch(
            IPCCommand(payload: .applyRestructure(libraryRoot: tmp.path, moves: [], useSymlinks: false)),
            coordinator: ScanCoordinator(), sink: sink, database: db)
        let applyNeedle = Data("\"restructureApplyResult\"".utf8)
        out = await waitFor([applyNeedle], in: collector)
        await sink.close()
        try? pipe.fileHandleForWriting.close()
        _ = await reader.value

        #expect(out.range(of: applyNeedle) != nil,
                "applyRestructure must emit a restructureApplyResult event")
    }

    @Test("restructurePlan DTO maps proposals and rolls up category counts")
    func restructurePlanDTOMapping() throws {
        let proposals = [
            RestructureProposal(fileID: 1, oldPath: "/a/1.jpg",
                                newPath: "/lib/People/Mom/1.jpg", bucket: "People/Mom",
                                confidence: "auto", reason: "Named person: Mom"),
            RestructureProposal(fileID: 2, oldPath: "/a/2.jpg",
                                newPath: "/lib/People/Mom/2.jpg", bucket: "People/Mom",
                                confidence: "auto", reason: nil),
            RestructureProposal(fileID: 3, oldPath: "/a/3.pdf",
                                newPath: "/lib/Documents/3.pdf", bucket: "Documents",
                                confidence: "review", reason: "Document"),
        ]
        let plan = FileIDEngineMain.restructurePlan(from: proposals, libraryRoot: "/lib")

        #expect(plan.libraryRoot == "/lib")
        #expect(plan.moves.count == 3)
        let first = try #require(plan.moves.first)
        #expect(first.source == "/a/1.jpg")
        #expect(first.destination == "/lib/People/Mom/1.jpg")
        #expect(first.category == "People/Mom")
        #expect(first.confidence == "auto")
        #expect(first.tier == nil)
        // Counts: People/Mom=2 (most), Documents=1; descending by count.
        #expect(plan.categoryCounts.first?.category == "People/Mom")
        #expect(plan.categoryCounts.first?.count == 2)
        #expect(plan.categoryCounts.reduce(0) { $0 + $1.count } == 3)
        #expect(plan.folderClassifications == nil)
    }
}
