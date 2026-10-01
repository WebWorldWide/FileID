import Foundation
import Darwin
import ImageIO
import AVFoundation
import FileIDShared

struct VideoFrameMetadata: Codable, Sendable, Equatable {
    let seconds: Double
    let duration: Double
    let size: Int64
    let modifiedAt: Double
    var revision: String { "\(size):\(modifiedAt.bitPattern)" }
}

enum VideoFrameWorker {
    struct Sample: Sendable { let url: URL; let metadata: VideoFrameMetadata }

    static func sweepAbandonedFrames(directory: URL = FileManager.default.temporaryDirectory, now: Date = Date()) {
        guard (try? ReadOnlyLocations.requireWritable(directory)) != nil,
            let entries = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.contentModificationDateKey, .isRegularFileKey]) else { return }
        for entry in entries where entry.lastPathComponent.hasPrefix("FileIDTimeline-") && entry.pathExtension == "png" {
            guard UUID(uuidString: String(entry.deletingPathExtension().lastPathComponent.dropFirst("FileIDTimeline-".count))) != nil,
                (try? ReadOnlyLocations.requireWritable(entry)) != nil,
                let values = try? entry.resourceValues(forKeys: [.contentModificationDateKey, .isRegularFileKey]),
                values.isRegularFile == true, let modified = values.contentModificationDate,
                now.timeIntervalSince(modified) > 300 else { continue }
            try? FileManager.default.removeItem(at: entry)
        }
    }

    static func run(arguments: [String]) async -> Int32 {
        let parentPID = getppid()
        let parentMonitor = Task.detached {
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 1_000_000_000) } catch { return }
                let parent = getppid()
                if parent == 1 || parent != parentPID { kill(getpid(), SIGKILL); return }
            }
        }
        defer { parentMonitor.cancel() }
        do {
            guard arguments.count == 3, let seconds = Double(arguments[1]), seconds.isFinite, seconds >= 0 else { return 2 }
            let source = URL(fileURLWithPath: arguments[0])
            let destination = URL(fileURLWithPath: arguments[2])
            try ReadOnlyLocations.requireWritable(destination)
            guard !FileManager.default.fileExists(atPath: destination.path) else { return 2 }
            let before = try attributes(source)
            guard let frame = await DeepAnalyze.extractTimedVideoFrame(url: source, maxPixelSize: 768, requestedSeconds: seconds),
                let duration = await DeepAnalyze.loadVideoDurationSeconds(AVURLAsset(url: source), timeoutSeconds: 8), duration.isFinite, duration > 0 else { return 3 }
            let after = try attributes(source)
            guard before == after else { return 4 }
            guard let output = CGImageDestinationCreateWithURL(destination as CFURL, "public.png" as CFString, 1, nil) else { return 3 }
            CGImageDestinationAddImage(output, frame.image, nil)
            guard CGImageDestinationFinalize(output) else { return 3 }
            let metadata = VideoFrameMetadata(seconds: frame.seconds, duration: duration, size: after.size, modifiedAt: after.modified)
            try FileHandle.standardOutput.write(contentsOf: JSONEncoder().encode(metadata))
            return 0
        } catch { return 3 }
    }

    static func sample(source: URL, seconds: Double) async throws -> Sample {
        sweepAbandonedFrames()
        let destination = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDTimeline-" + UUID().uuidString + ".png")
        try ReadOnlyLocations.requireWritable(destination)
        let output = Pipe()
        let process = Process()
        process.executableURL = URL(fileURLWithPath: CommandLine.arguments[0])
        process.arguments = ["--sample-video", source.path, String(seconds), destination.path]
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        do {
            let status = try await CancellableProcess.wait(process, timeoutSeconds: 12)
            guard status == 0, let data = try output.fileHandleForReading.read(upToCount: 4096) else { throw WorkerFailure() }
            let metadata = try JSONDecoder().decode(VideoFrameMetadata.self, from: data)
            guard metadata.seconds.isFinite, metadata.seconds >= 0, metadata.duration.isFinite, metadata.duration > 0 else { throw WorkerFailure() }
            return Sample(url: destination, metadata: metadata)
        } catch {
            try? FileManager.default.removeItem(at: destination)
            throw error
        }
    }

    private struct Attributes: Equatable { let size: Int64; let modified: Double }
    private static func attributes(_ source: URL) throws -> Attributes {
        let values = try source.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey])
        guard let size = values.fileSize, let modified = values.contentModificationDate else { throw WorkerFailure() }
        return Attributes(size: Int64(size), modified: modified.timeIntervalSince1970)
    }
    struct WorkerFailure: LocalizedError { var errorDescription: String? { "The video worker could not decode a stable source frame." } }
}

enum CancellableProcess {
    struct TimedOut: LocalizedError, Sendable {
        var errorDescription: String? { "The video decoder exceeded its time limit. The worker was stopped; try the file again or check whether the source drive is responding." }
    }
    static func wait(_ process: Process, timeoutSeconds: UInt64) async throws -> Int32 {
        let state = WaitState(process: process)
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                state.start(continuation, timeoutSeconds: timeoutSeconds)
            }
        } onCancel: { state.cancel() }
    }

    private final class WaitState: @unchecked Sendable {
        private let process: Process
        private let lock = NSLock()
        private var continuation: CheckedContinuation<Int32, Error>?
        private var timeout: Task<Void, Never>?
        private var cancelled = false
        private var timedOut = false
        private var finished = false
        init(process: Process) { self.process = process }
        func start(_ continuation: CheckedContinuation<Int32, Error>, timeoutSeconds: UInt64) {
            lock.lock()
            if cancelled { lock.unlock(); continuation.resume(throwing: CancellationError()); return }
            self.continuation = continuation
            process.terminationHandler = { [weak self] process in self?.finish(.success(process.terminationStatus)) }
            do { try process.run() } catch { lock.unlock(); finish(.failure(error)); return }
            timeout = Task { [weak self] in
                do { try await Task.sleep(nanoseconds: timeoutSeconds * 1_000_000_000) } catch { return }
                self?.cancel(timedOut: true)
            }
            lock.unlock()
        }
        func cancel(timedOut: Bool = false) {
            lock.lock()
            cancelled = true
            self.timedOut = self.timedOut || timedOut
            if process.isRunning { kill(process.processIdentifier, SIGKILL) }
            lock.unlock()
        }
        func finish(_ result: Result<Int32, Error>) {
            lock.lock()
            guard !finished else { lock.unlock(); return }
            finished = true
            let continuation = continuation
            self.continuation = nil
            timeout?.cancel()
            timeout = nil
            let cancelled = cancelled
            let timedOut = timedOut
            process.terminationHandler = nil
            lock.unlock()
            continuation?.resume(with: timedOut ? .failure(TimedOut()) : cancelled ? .failure(CancellationError()) : result)
        }
    }
}
