import Foundation
import Darwin
import ImageIO
import AVFoundation
import CoreGraphics
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
    private struct SignalFrame: @unchecked Sendable { let image: CGImage; let seconds: Double }
    private struct SignalGenerator: @unchecked Sendable { let value: AVAssetImageGenerator }

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
            try writeFrame(frame.image, destination: destination)
            let metadata = VideoFrameMetadata(seconds: frame.seconds, duration: duration, size: after.size, modifiedAt: after.modified)
            try FileHandle.standardOutput.write(contentsOf: JSONEncoder().encode(metadata))
            return 0
        } catch { return 3 }
    }

    static func runSignals(arguments: [String]) async -> Int32 {
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
            guard arguments.count == 4,
                  let start = Double(arguments[1]), start.isFinite, start >= 0,
                  let end = Double(arguments[2]), end.isFinite, end > start,
                  let interval = Double(arguments[3]), interval.isFinite, interval >= 0.25, interval <= 5 else { return 2 }

            let source = URL(fileURLWithPath: arguments[0])
            let before = try attributes(source)
            let asset = AVURLAsset(url: source)
            guard let duration = await DeepAnalyze.loadVideoDurationSeconds(asset, timeoutSeconds: 8),
                  duration.isFinite, duration > 0, start < duration else { return 3 }

            let generator = AVAssetImageGenerator(asset: asset)
            generator.appliesPreferredTrackTransform = true
            generator.maximumSize = CGSize(width: 32, height: 18)
            generator.requestedTimeToleranceBefore = CMTime(seconds: interval / 2, preferredTimescale: 600)
            generator.requestedTimeToleranceAfter = CMTime(seconds: interval / 2, preferredTimescale: 600)
            let generatorRef = SignalGenerator(value: generator)

            var signals: [TimelineSignalAnalysis.Signal] = []
            var previousPixels: [UInt8]?
            var target = start
            while target < min(end, duration), !Task.isCancelled {
                let frame = await generateSignalFrame(using: generatorRef, at: target)
                guard let frame else { return 3 }
                guard frame.seconds.isFinite else { return 3 }
                let pixels = try grayscaleSignature(frame.image)
                let score = previousPixels.map { difference($0, pixels) } ?? 0
                signals.append(.init(seconds: frame.seconds, changeScore: score))
                previousPixels = pixels
                target += interval
            }
            guard !Task.isCancelled else { return 3 }
            guard try attributes(source) == before else { return 4 }
            try FileHandle.standardOutput.write(contentsOf: JSONEncoder().encode(signals))
            return 0
        } catch {
            return 3
        }
    }

    static func runPhoto(arguments: [String]) async -> Int32 {
        let parentPID = getppid()
        let monitor = Task.detached {
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 1_000_000_000) } catch { return }
                if getppid() == 1 || getppid() != parentPID { kill(getpid(), SIGKILL); return }
            }
        }
        defer { monitor.cancel() }
        do {
            guard [4, 5].contains(arguments.count), ["png", "jpeg", "tiff"].contains(arguments[1]),
                  (arguments.count == 4 || ["true", "false"].contains(arguments[4])), let dimension = Int(arguments[2]), (1...8192).contains(dimension) else { return 2 }
            let destination = URL(fileURLWithPath: arguments[3])
            try ReadOnlyLocations.requireSourceMutation(destination)
            guard !FileManager.default.fileExists(atPath: destination.path) else { return 2 }
            try MediaTools.exportPhoto(source: URL(fileURLWithPath: arguments[0]), output: destination, recipe: ToolRecipe(kind: "photo", format: arguments[1], maxDimension: dimension, allowUpscale: arguments.count == 5 ? arguments[4] == "true" : nil))
            return 0
        } catch { return 3 }
    }

    static func exportPhoto(source: URL, output: URL, recipe: ToolRecipe) async throws {
        try ReadOnlyLocations.requireWritable(output)
        let process = Process()
        var executable = CommandLine.arguments[0]
        #if DEBUG
        executable = ProcessInfo.processInfo.environment["FILEID_TEST_ENGINE_PATH"] ?? executable
        #endif
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = ["--export-photo", source.path, recipe.format, String(recipe.maxDimension), output.path, recipe.allowUpscale == true ? "true" : "false"]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        guard try await CancellableProcess.wait(process, timeoutSeconds: 30) == 0 else { throw MediaTools.Failure(text: "The photo worker could not convert this input. Check its format, animation, and readability.") }
    }

    private static func writeFrame(_ image: CGImage, destination: URL) throws {
        let data = NSMutableData()
        guard let output = CGImageDestinationCreateWithData(data, "public.png" as CFString, 1, nil) else { throw WorkerFailure() }
        CGImageDestinationAddImage(output, image, nil)
        guard CGImageDestinationFinalize(output) else { throw WorkerFailure() }
        try (data as Data).write(to: destination, options: .withoutOverwriting)
    }

    static func runSequence(arguments: [String]) async -> Int32 {
        let parentPID = getppid()
        let parentMonitor = Task.detached {
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 1_000_000_000) } catch { return }
                let parent = getppid()
                if parent == 1 || parent != parentPID { kill(getpid(), SIGKILL); return }
            }
        }
        defer { parentMonitor.cancel() }
        var created: [URL] = []
        var succeeded = false
        defer { if !succeeded { for url in created { try? FileManager.default.removeItem(at: url) } } }
        do {
            guard arguments.count == 3, arguments[1].utf8.count <= 1024, arguments[2].utf8.count <= 32_768 else { return 2 }
            let times = try JSONDecoder().decode([Double].self, from: Data(arguments[1].utf8))
            let paths = try JSONDecoder().decode([String].self, from: Data(arguments[2].utf8))
            guard (2...8).contains(times.count), paths.count == times.count, Set(paths).count == paths.count,
                  times.allSatisfy({ $0.isFinite && $0 >= 0 && $0 <= 21_600 }),
                  zip(times, times.dropFirst()).allSatisfy({ $0 <= $1 }) else { return 2 }
            let destinations = paths.map { URL(fileURLWithPath: $0) }
            for destination in destinations {
                try ReadOnlyLocations.requireWritable(destination)
                guard !FileManager.default.fileExists(atPath: destination.path) else { return 2 }
            }
            let source = URL(fileURLWithPath: arguments[0])
            let before = try attributes(source)
            guard let duration = await DeepAnalyze.loadVideoDurationSeconds(AVURLAsset(url: source), timeoutSeconds: 8),
                  duration.isFinite, duration > 0, times.allSatisfy({ $0 < duration }) else { return 3 }
            var metadata: [VideoFrameMetadata] = []
            for (seconds, destination) in zip(times, destinations) {
                guard let frame = await DeepAnalyze.extractTimedVideoFrame(url: source, maxPixelSize: 256, requestedSeconds: seconds) else { return 3 }
                try writeFrame(frame.image, destination: destination)
                created.append(destination)
                metadata.append(VideoFrameMetadata(seconds: frame.seconds, duration: duration, size: before.size, modifiedAt: before.modified))
            }
            guard try attributes(source) == before else { return 4 }
            try FileHandle.standardOutput.write(contentsOf: JSONEncoder().encode(metadata))
            succeeded = true
            return 0
        } catch { return 3 }
    }

    static func sampleSequence(source: URL, times: [Double]) async throws -> [Sample] {
        guard (2...8).contains(times.count) else { throw WorkerFailure() }
        sweepAbandonedFrames()
        let destinations = times.map { _ in FileManager.default.temporaryDirectory.appendingPathComponent("FileIDTimeline-" + UUID().uuidString + ".png") }
        for destination in destinations { try ReadOnlyLocations.requireWritable(destination) }
        let output = Pipe()
        let process = Process()
        var executable = CommandLine.arguments[0]
#if DEBUG
        executable = ProcessInfo.processInfo.environment["FILEID_TEST_ENGINE_PATH"] ?? executable
#endif
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = ["--sample-video-sequence", source.path,
                             String(decoding: try JSONEncoder().encode(times), as: UTF8.self),
                             String(decoding: try JSONEncoder().encode(destinations.map(\.path)), as: UTF8.self)]
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        do {
            let status = try await CancellableProcess.wait(process, timeoutSeconds: 60)
            if status == 4 { throw SourceRevisionChanged() }
            guard status == 0, let data = try output.fileHandleForReading.read(upToCount: 16_384) else { throw WorkerFailure() }
            let metadata = try JSONDecoder().decode([VideoFrameMetadata].self, from: data)
            guard metadata.count == times.count, metadata.allSatisfy({ $0.seconds.isFinite && $0.seconds >= 0 && $0.duration.isFinite && $0.duration > 0 }) else { throw WorkerFailure() }
            return zip(destinations, metadata).map { Sample(url: $0, metadata: $1) }
        } catch {
            for destination in destinations { try? FileManager.default.removeItem(at: destination) }
            throw error
        }
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

    static func scanSignals(source: URL, start: Double, end: Double, interval: Double) async throws -> [TimelineSignalAnalysis.Signal] {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: CommandLine.arguments[0])
        process.arguments = ["--scan-video-signals", source.path, String(start), String(end), String(interval)]
        let output = Pipe()
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        let status = try await CancellableProcess.wait(process, timeoutSeconds: 90)
        if status == 4 { throw SourceRevisionChanged() }
        guard status == 0 else { throw WorkerFailure() }
        let data = output.fileHandleForReading.readDataToEndOfFile()
        return try JSONDecoder().decode([TimelineSignalAnalysis.Signal].self, from: data)
    }

    private static func generateSignalFrame(using generator: SignalGenerator, at seconds: Double) async -> SignalFrame? {
        await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                generator.value.generateCGImageAsynchronously(for: CMTime(seconds: seconds, preferredTimescale: 600)) { image, time, _ in
                    continuation.resume(returning: image.map { SignalFrame(image: $0, seconds: time.seconds) })
                }
            }
        } onCancel: {
            generator.value.cancelAllCGImageGeneration()
        }
    }

    private static func grayscaleSignature(_ image: CGImage) throws -> [UInt8] {
        let width = 32
        let height = 18
        var pixels = [UInt8](repeating: 0, count: width * height)
        let rendered = pixels.withUnsafeMutableBytes { bytes -> Bool in
            guard let context = CGContext(
                data: bytes.baseAddress,
                width: width,
                height: height,
                bitsPerComponent: 8,
                bytesPerRow: width,
                space: CGColorSpaceCreateDeviceGray(),
                bitmapInfo: CGImageAlphaInfo.none.rawValue
            ) else { return false }
            context.interpolationQuality = .low
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        guard rendered else { throw WorkerFailure() }
        return pixels
    }

    private static func difference(_ lhs: [UInt8], _ rhs: [UInt8]) -> Double {
        guard lhs.count == rhs.count, !lhs.isEmpty else { return 0 }
        let total = zip(lhs, rhs).reduce(0) { sum, pair in
            sum + abs(Int(pair.0) - Int(pair.1))
        }
        return Double(total) / Double(lhs.count * 255)
    }

    private struct Attributes: Equatable { let size: Int64; let modified: Double }
    private static func attributes(_ source: URL) throws -> Attributes {
        let values = try source.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey])
        guard let size = values.fileSize, let modified = values.contentModificationDate else { throw WorkerFailure() }
        return Attributes(size: Int64(size), modified: modified.timeIntervalSince1970)
    }
    struct WorkerFailure: LocalizedError { var errorDescription: String? { "The video worker could not decode a stable source frame." } }
    struct SourceRevisionChanged: LocalizedError { var errorDescription: String? { "The video changed while visual activity was being sampled." } }
}

enum CancellableProcess {
    struct TimedOut: LocalizedError, Sendable {
        var errorDescription: String? { "The decoder exceeded its time limit. The worker was stopped; try the file again or check whether the source drive is responding." }
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
        private var timeout: DispatchSourceTimer?
        private var cancelled = false
        private var timedOut = false
        private var finished = false
        init(process: Process) { self.process = process }
        func start(_ continuation: CheckedContinuation<Int32, Error>, timeoutSeconds: UInt64) {
            lock.lock()
            if cancelled { lock.unlock(); continuation.resume(throwing: CancellationError()); return }
            self.continuation = continuation
            process.terminationHandler = { [weak self] process in self?.finish(.success(process.terminationStatus)) }
            let timer = DispatchSource.makeTimerSource(queue: .global(qos: .userInitiated))
            // A cooperative executor can be busy when the worker deadline expires.
            timer.schedule(deadline: .now() + .seconds(Int(min(timeoutSeconds, 86_400))))
            timer.setEventHandler { [weak self] in self?.cancel(timedOut: true) }
            timeout = timer
            timer.resume()
            do { try process.run() } catch { lock.unlock(); finish(.failure(error)); return }
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
