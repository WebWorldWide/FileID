// C1 regression, end-to-end: cancelling a scan mid-flight must still
// produce the terminal scanComplete event and leave the engine
// responsive (the original deadlock left an unbuffered channel producer
// uncancelled, wedging shutdown). Spawns the real engine binary, so
// this also exercises the U4 fd-2 transport split: every wire line must
// be a parseable JSON event.
import Testing
import Foundation
@testable import FileIDEngine
import FileIDShared

@Suite("Scan cancellation (C1, process-level)", .serialized)
struct ScanCancellationTests {

    private static var engineBinary: URL {
        // …/platforms/apple/Tests/EngineTests/ScanCancellationTests.swift
        //   → …/platforms/apple/.build/debug/FileIDEngine
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent(".build/debug/FileIDEngine")
    }

    private actor LineCollector {
        private var pending = Data()
        private(set) var lines: [String] = []
        func feed(_ chunk: Data) {
            pending.append(chunk)
            while let nl = pending.firstIndex(of: 0x0A) {
                let line = pending.subdata(in: pending.startIndex..<nl)
                pending.removeSubrange(pending.startIndex...nl)
                if !line.isEmpty {
                    lines.append(String(decoding: line, as: UTF8.self))
                }
            }
        }
        func snapshot() -> [String] { lines }
    }

    @Test("Cancel mid-scan still yields scanComplete; engine stays responsive")
    func cancelMidScanCompletes() async throws {
        let binary = Self.engineBinary
        guard FileManager.default.isExecutableFile(atPath: binary.path) else {
            // swift test builds executable targets the test target depends
            // on, so this only trips when running a partial build.
            Issue.record("engine binary missing at \(binary.path) — build first")
            return
        }

        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDCancelTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        for i in 0..<2000 {
            FileManager.default.createFile(
                atPath: root.appendingPathComponent("f\(i).jpg").path,
                contents: Data([0xFF, 0xD8, 0xFF, 0xE0]))
        }

        let proc = Process()
        proc.executableURL = binary
        let stdin = Pipe(), wire = Pipe(), stdout = Pipe()
        proc.standardInput = stdin
        proc.standardError = wire
        proc.standardOutput = stdout
        let collector = LineCollector()
        wire.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            if data.isEmpty { handle.readabilityHandler = nil; return }
            Task { await collector.feed(data) }
        }
        try proc.run()
        // A leaked engine child holds the test harness's output pipe open
        // and turns "suite finished" into an infinite hang (the job-level
        // symptom: swiftpm-testing + FileIDEngine reaped as orphans at the
        // 60-min CI timeout). Close stdin (the engine's EOF exit path),
        // then escalate to SIGKILL — never leave it running.
        defer {
            try? stdin.fileHandleForWriting.close()
            if proc.isRunning {
                proc.terminate()
                kill(proc.processIdentifier, SIGKILL)
            }
        }

        func send(_ json: String) throws {
            try stdin.fileHandleForWriting.write(contentsOf: Data((json + "\n").utf8))
        }
        func waitFor(_ needle: String, timeout: TimeInterval) async -> Bool {
            let deadline = Date().addingTimeInterval(timeout)
            while Date() < deadline {
                if await collector.snapshot().contains(where: { $0.contains(needle) }) {
                    return true
                }
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
            return false
        }

        #expect(await waitFor("\"ready\"", timeout: 15), "engine must emit ready")

        let escapedRoot = root.path.replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        try send(#"{"id":"s","payload":{"startScan":{"rootPath":""# + escapedRoot
            + #"","rootDisplay":null,"rescan":false}}}"#)

        // Cancel as soon as the scan is visibly under way.
        var underway = await waitFor("\"phaseChanged\"", timeout: 20)
        if !underway { underway = await waitFor("\"progress\"", timeout: 5) }
        #expect(underway, "scan must start emitting before we cancel")
        try send(#"{"id":"c","payload":{"cancelScan":{}}}"#)

        // The C1 deadlock manifested here: no terminal event, ever.
        #expect(await waitFor("\"scanComplete\"", timeout: 60),
                "cancelled scan must still emit the terminal scanComplete")

        // The other C1 symptom was shutdown wedging behind the uncancelled
        // producer — a clean exit IS the responsiveness probe. Poll
        // isRunning instead of waitUntilExit(): a blocking wait inside a
        // task group can NEVER be cancelled, so the old "timeout" wrapper
        // deadlocked the whole suite whenever exit took >15 s (task groups
        // drain all children before returning). 30 s budget for slow CI
        // runners — a cancelled 2000-file scan still checkpoints the WAL
        // on the way out.
        try send(#"{"id":"x","payload":{"shutdown":{}}}"#)
        let exitDeadline = Date().addingTimeInterval(30)
        while proc.isRunning && Date() < exitDeadline {
            try? await Task.sleep(nanoseconds: 200_000_000)
        }
        #expect(!proc.isRunning, "engine must exit cleanly on shutdown after a cancelled scan")

        // U4: every line on the wire must be a JSON object — no library
        // chatter may reach the IPC stream.
        for line in await collector.snapshot() {
            #expect(line.hasPrefix("{"), "non-JSON line on the IPC wire: \(line.prefix(120))")
        }
    }

    // F-C6-013 wiring: cancelScan → coordinator.requestCancel() must cancel a
    // registered restructure-apply task (Restructure.apply polls Task.isCancelled
    // per move). Before the wiring the apply ran in a discarded detached task no
    // signal could reach, so a long apply was unstoppable.
    @Test("requestCancel cancels a registered restructure-apply task")
    func requestCancelCancelsRestructureTask() async {
        let coord = ScanCoordinator()
        let task = Task.detached {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000)
            }
        }
        await coord.setActiveRestructure(task)
        await coord.requestCancel()
        // `await task.value` returns ONLY after the loop observed cancellation
        // and exited — proving requestCancel propagated to the registered task.
        await task.value
        #expect(task.isCancelled, "requestCancel must cancel the registered apply task")
    }

}
