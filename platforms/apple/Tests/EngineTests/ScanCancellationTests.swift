// End-to-end: cancelling a scan mid-flight must still
// produce the terminal scanComplete event and leave the engine
// responsive (the original deadlock left an unbuffered channel producer
// uncancelled, wedging shutdown). Spawns the real engine binary, so
// this also exercises the fd-2 transport split: every wire line must
// be a parseable JSON event.
import Testing
import Foundation
@testable import FileIDEngine
import FileIDShared

@Suite("Scan cancellation (C1, process-level)", .serialized)
struct ScanCancellationTests {

    private static var engineBinary: URL {
        // CI points FILEID_TEST_ENGINE_PATH at the last release build, which is the
        // App Store sandbox variant: it will not scan a folder it holds no bookmark
        // for, so this test needs its own plain engine there.
        let environment = ProcessInfo.processInfo.environment
        if let override = environment["FILEID_TEST_SCAN_ENGINE_PATH"] ?? environment["FILEID_TEST_ENGINE_PATH"] {
            return URL(fileURLWithPath: override)
        }
        // …/platforms/apple/Tests/EngineTests/ScanCancellationTests.swift
        //   → …/platforms/apple/.build/debug/FileIDEngine
        return URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent(".build/debug/FileIDEngine")
    }

    // Lock-based (not an actor): the readability handler can feed SYNCHRONOUSLY
    // instead of spawning a `Task { await … }` per output chunk. Under the
    // 2000-file scan's event flood those unstructured tasks piled up and
    // contended with `await snapshot()` on the actor, starving the bounded
    // waitFor loops on a slow CI runner — the suite then hung past the 12-min
    // SIGALRM with a leaked engine child. Synchronous feed/snapshot removes the
    // task pile-up and the await entirely.
    private final class LineCollector: @unchecked Sendable {
        private let lock = NSLock()
        private var pending = Data()
        private var lines: [String] = []
        func feed(_ chunk: Data) {
            lock.lock(); defer { lock.unlock() }
            pending.append(chunk)
            while let nl = pending.firstIndex(of: 0x0A) {
                let line = pending.subdata(in: pending.startIndex..<nl)
                pending.removeSubrange(pending.startIndex...nl)
                if !line.isEmpty {
                    lines.append(String(decoding: line, as: UTF8.self))
                }
            }
        }
        func snapshot() -> [String] { lock.lock(); defer { lock.unlock() }; return lines }
    }

    // Waits on events, not on the clock: every step ends when its event arrives or when
    // the engine process has exited, and one overall deadline (below the 180 s watchdog)
    // is only the safety net against a wedged engine. A busy machine makes this slower,
    // never wrong.
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
        // Enough files that discovery is still producing when the cancel lands (the C1
        // deadlock needed a producer parked on the rendezvous channel), no more.
        for i in 0..<600 {
            FileManager.default.createFile(
                atPath: root.appendingPathComponent("f\(i).jpg").path,
                contents: Data([0xFF, 0xD8, 0xFF, 0xE0]))
        }

        let proc = Process()
        proc.executableURL = binary
        proc.environment = ProcessInfo.processInfo.environment.merging([
            "FILEID_DATABASE_PATH": root.appendingPathComponent("fileid.sqlite").path
        ]) { _, isolated in isolated }
        let stdin = Pipe(), wire = Pipe(), stdout = Pipe()
        proc.standardInput = stdin
        proc.standardError = wire
        proc.standardOutput = stdout
        let collector = LineCollector()
        wire.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            if data.isEmpty { handle.readabilityHandler = nil; return }
            collector.feed(data)
        }
        // DRAIN stdout: the engine's IPC is on fd 2 (wire); fd 1 is incidental
        // (library/MLX chatter). An UNREAD stdout Pipe fills its 64 KB buffer and
        // blocks the engine's next write — wedging it so it never processes
        // shutdown and never exits. Discard whatever lands here.
        stdout.fileHandleForReading.readabilityHandler = { handle in
            if handle.availableData.isEmpty { handle.readabilityHandler = nil }
        }
        try proc.run()
        // A leaked engine child holds the test harness's output pipe open and
        // turns "suite finished" into a hang that only the job's 12-min SIGALRM
        // breaks. An out-of-band watchdog SIGKILLs the engine after a hard ceiling
        // REGARDLESS of where the test body is blocked; the defer closes stdin and
        // force-kills on the normal path. GCD, NOT Task.detached: it has its own
        // threads, independent of the Swift concurrency pool.
        let enginePID = proc.processIdentifier
        let killItem = DispatchWorkItem { kill(enginePID, SIGKILL) }
        DispatchQueue.global().asyncAfter(deadline: .now() + 180, execute: killItem)
        defer {
            killItem.cancel()
            try? stdin.fileHandleForWriting.close()
            proc.terminate()
            kill(enginePID, SIGKILL)   // unconditional; harmless if already gone
            wire.fileHandleForReading.readabilityHandler = nil
            stdout.fileHandleForReading.readabilityHandler = nil
        }
        let overall = Date().addingTimeInterval(150)

        func send(_ json: String) throws {
            try stdin.fileHandleForWriting.write(contentsOf: Data((json + "\n").utf8))
        }
        // Ends on the first needle seen, the engine exiting, or the overall deadline.
        func waitFor(_ needles: String...) async -> Bool {
            while Date() < overall {
                let lines = collector.snapshot()
                if lines.contains(where: { line in needles.contains { line.contains($0) } }) { return true }
                if !proc.isRunning { return false }
                try? await Task.sleep(nanoseconds: 20_000_000)
            }
            return false
        }

        guard await waitFor("\"ready\"") else {
            Issue.record("engine must emit ready")
            return
        }

        let escapedRoot = root.path.replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        try send(#"{"id":"s","payload":{"startScan":{"rootPath":""# + escapedRoot
            + #"","rootDisplay":null,"rescan":false}}}"#)

        // Cancel once the scan is visibly under way: its first phase or progress event.
        guard await waitFor("\"phaseChanged\"", "\"progress\"") else {
            let tail = collector.snapshot().suffix(12).map { String($0.prefix(300)) }.joined(separator: "\n")
            Issue.record("the scan never reported a phase or progress; the engine's last lines:\n\(tail)")
            return
        }
        try send(#"{"id":"c","payload":{"cancelScan":{}}}"#)

        // The C1 deadlock manifested here: no terminal event, ever.
        #expect(await waitFor("\"scanComplete\""),
                "cancelled scan must still emit the terminal scanComplete")

        // The other C1 symptom was shutdown wedging behind the uncancelled
        // producer — a clean exit IS the responsiveness probe. Poll isRunning
        // instead of waitUntilExit(): a blocking wait inside a task group can
        // NEVER be cancelled.
        try send(#"{"id":"x","payload":{"shutdown":{}}}"#)
        while proc.isRunning && Date() < overall {
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        #expect(!proc.isRunning, "engine must exit cleanly on shutdown after a cancelled scan")

        // Every line on the wire must be a JSON object — no library
        // chatter may reach the IPC stream.
        for line in collector.snapshot() {
            #expect(line.hasPrefix("{"), "non-JSON line on the IPC wire: \(line.prefix(120))")
        }
    }

    @Test("a scan root with a NUL, or one that is not absolute, is refused instead of scanning the working directory")
    func scanRootMustBeAbsoluteWithoutNUL() {
        #expect(FileIDEngineMain.isUsableScanRoot("/Users/example/Pictures"))
        #expect(!FileIDEngineMain.isUsableScanRoot("\u{0}"))
        #expect(!FileIDEngineMain.isUsableScanRoot("/Users/example\u{0}/Pictures"))
        #expect(!FileIDEngineMain.isUsableScanRoot(""))
        #expect(!FileIDEngineMain.isUsableScanRoot("Pictures"))
    }

    @Test("scan and restructure cancellation stay isolated across later jobs")
    func operationSpecificCancellation() async {
        let coord = ScanCoordinator()
        await coord.requestRestructureCancel()
        let task = Task.detached {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000)
            }
        }
        let token = await coord.reserveRestructure()
        #expect(token != nil)
        if let token { await coord.attachRestructure(task, token: token) }
        await coord.requestCancel()
        #expect(!task.isCancelled, "idle or scan cancellation must not poison restructure")

        await coord.requestRestructureCancel()
        await task.value
        #expect(task.isCancelled, "cancelRestructure must cancel the registered apply task")
        if let token { await coord.finishRestructure(token: token) }

        let laterTask = Task.detached {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000)
            }
        }
        let laterToken = await coord.reserveRestructure()
        #expect(laterToken != nil)
        if let laterToken {
            await coord.attachRestructure(laterTask, token: laterToken)
        }
        #expect(!laterTask.isCancelled, "a later restructure must not inherit stale cancellation")
        await coord.requestRestructureCancel()
        await laterTask.value
        if let laterToken { await coord.finishRestructure(token: laterToken) }
    }

    @Test("Only one restructure operation can be reserved")
    func restructureReservationIsExclusive() async {
        let coord = ScanCoordinator()
        let first = await coord.reserveRestructure()
        #expect(first != nil)
        #expect(await coord.reserveRestructure() == nil)
        if let first { await coord.finishRestructure(token: first) }
        #expect(await coord.reserveRestructure() != nil)
    }

    @Test("periodic progress stops after a terminal scan phase")
    func periodicProgressStopsAtTerminalPhase() async {
        let coord = ScanCoordinator()
        _ = await coord.startSession(rootDisplayPath: "/tmp/library", epoch: 0)
        #expect(await coord.periodicSnapshot() != nil)

        await coord.setPhase(.completed)
        #expect(await coord.periodicSnapshot() == nil)
        #expect(await coord.snapshot()?.phase == .completed)
    }

}
