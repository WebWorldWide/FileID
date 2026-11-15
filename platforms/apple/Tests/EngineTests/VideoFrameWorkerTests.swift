import Foundation
import Testing
@testable import FileIDEngine

@Suite("Isolated video worker")
struct VideoFrameWorkerTests {
    @Test func sequenceRejectsUnboundedAndDuplicateDestinationsBeforeReadingSource() async throws {
        let duplicate = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDTimeline-\(UUID().uuidString).png")
        let paths = String(decoding: try JSONEncoder().encode([duplicate.path, duplicate.path]), as: UTF8.self)
        #expect(await VideoFrameWorker.runSequence(arguments: ["/missing/source.mov", "[0,1]", paths]) == 2)
        #expect(!FileManager.default.fileExists(atPath: duplicate.path))
        #expect(await VideoFrameWorker.runSequence(arguments: ["/missing/source.mov", "[0]", "[]"]) == 2)
        #expect(await VideoFrameWorker.runSequence(arguments: ["/missing/source.mov", "[1,0]", paths]) == 2)
        #expect(await VideoFrameWorker.runSequence(arguments: ["/missing/source.mov", String(repeating: "0", count: 1025), "[]"]) == 2)
    }

    @Test func sweepsOnlyOldWorkerFrames() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let now = Date()
        let abandoned = directory.appendingPathComponent("FileIDTimeline-\(UUID().uuidString).png")
        let active = directory.appendingPathComponent("FileIDTimeline-\(UUID().uuidString).png")
        let unrelated = directory.appendingPathComponent("FileIDTimeline-user-photo.png")
        for url in [abandoned,active,unrelated] { try Data([1]).write(to: url) }
        for url in [abandoned,unrelated] { try FileManager.default.setAttributes([.modificationDate: now.addingTimeInterval(-600)], ofItemAtPath: url.path) }
        VideoFrameWorker.sweepAbandonedFrames(directory: directory, now: now)
        #expect(!FileManager.default.fileExists(atPath: abandoned.path))
        #expect(FileManager.default.fileExists(atPath: active.path))
        #expect(FileManager.default.fileExists(atPath: unrelated.path))
    }

    @Test func sweepsTheFrameFolderOncePerGate() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let old = Date().addingTimeInterval(-600)
        func plant() throws -> URL {
            let url = directory.appendingPathComponent("FileIDTimeline-\(UUID().uuidString).png")
            try Data([1]).write(to: url)
            try FileManager.default.setAttributes([.modificationDate: old], ofItemAtPath: url.path)
            return url
        }
        let gate = VideoFrameWorker.SweepGate()
        let first = try plant()
        VideoFrameWorker.sweepAbandonedFramesOnce(gate: gate, directory: directory)
        #expect(!FileManager.default.fileExists(atPath: first.path))
        let second = try plant()
        VideoFrameWorker.sweepAbandonedFramesOnce(gate: gate, directory: directory)
        #expect(FileManager.default.fileExists(atPath: second.path))
        VideoFrameWorker.sweepAbandonedFramesOnce(gate: VideoFrameWorker.SweepGate(), directory: directory)
        #expect(!FileManager.default.fileExists(atPath: second.path))
    }

    @Test func sweepGateIsClaimedOnceAcrossThreads() async {
        let gate = VideoFrameWorker.SweepGate()
        let claims = await withTaskGroup(of: Bool.self) { group in
            for _ in 0..<64 { group.addTask { gate.claim() } }
            return await group.reduce(0) { $0 + ($1 ? 1 : 0) }
        }
        #expect(claims == 1)
    }

    @Test func timeoutTerminatesWorker() async throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sleep")
        process.arguments = ["30"]
        let start = Date()
        do {
            _ = try await CancellableProcess.wait(process, timeoutSeconds: 1)
            Issue.record("Worker should have timed out")
        } catch is CancellableProcess.TimedOut {}
        #expect(!process.isRunning)
        #expect(Date().timeIntervalSince(start) < 5)
    }
    @Test func workerRefusesProtectedOutputsBeforeReadingSource() async {
        let result = await VideoFrameWorker.run(arguments: ["/missing/source.mov", "0", "/Volumes/Adlon/frame.png"])
        #expect(result != 0)
    }

    @Test func cancellationWaitsForWorkerExit() async throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sleep")
        process.arguments = ["30"]
        let task = Task { try await CancellableProcess.wait(process, timeoutSeconds: 30) }
        try await Task.sleep(nanoseconds: 100_000_000)
        task.cancel()
        do {
            _ = try await task.value
            Issue.record("A cancelled worker should not return success")
        } catch is CancellationError {}
        #expect(!process.isRunning)
    }

    /// A fixture that failed the cross-engine timeline cases: stat said 1794731299 s and 518318480 ns.
    /// Foundation's Date gives ...256674; the Rust engine and Python store ...256673.
    @Test func sourceRevisionToleratesTheOtherEnginesRoundingOfTheSameModifiedTime() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDRevision-\(UUID().uuidString).bin")
        try Data("clip".utf8).write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        let stamp = timespec(tv_sec: 1_794_731_299, tv_nsec: 518_318_480)
        var times = [stamp, stamp]
        #expect(utimensat(AT_FDCWD, file.path, &times, 0) == 0)
        let onDisk = try VideoFrameWorker.currentRevision(file)
        let swiftRevision = "4:4745314456662256674"
        let rustRevision = "4:4745314456662256673"
        #expect(onDisk == swiftRevision)
        #expect(onDisk != rustRevision)
        #expect(VideoFrameMetadata.sameSource(onDisk, rustRevision))
        #expect(VideoFrameMetadata.sameSource(rustRevision, onDisk))
        #expect(VideoFrameMetadata.sameSource(onDisk, onDisk))
    }

    @Test func sourceRevisionStillRejectsARealChange() {
        let revision = "4:4745314456662256673"
        let moved = Double(bitPattern: 4_745_314_456_662_256_673) + 0.001
        let later = String(moved.bitPattern)
        #expect(!VideoFrameMetadata.sameSource(revision, "5:4745314456662256673"))
        #expect(!VideoFrameMetadata.sameSource(revision, "4:" + later))
        #expect(!VideoFrameMetadata.sameSource(revision, "4:unknown"))
        #expect(!VideoFrameMetadata.sameSource("4:unknown", revision))
        #expect(VideoFrameMetadata.sameSource("4:unknown", "4:unknown"))
        #expect(!VideoFrameMetadata.sameSource(revision, "garbage"))
    }
}
