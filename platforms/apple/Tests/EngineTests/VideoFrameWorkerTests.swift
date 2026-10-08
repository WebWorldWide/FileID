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
}
