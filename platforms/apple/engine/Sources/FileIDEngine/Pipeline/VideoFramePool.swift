import Foundation
import Darwin
import ImageIO
import AVFoundation
import CoreGraphics
import CoreImage
import UniformTypeIdentifiers
import FileIDShared

/// One helper process per video decodes every pool frame through a single
/// AVAssetImageGenerator (hardware decode, scaled to `maxPixelSize`), writes small
/// JPEGs and a manifest, and exits. Replaces one process per frame or per window.
enum VideoFramePool {
    struct Frame: Codable, Sendable, Equatable {
        let seconds: Double
        let file: String
        let signature: [UInt8]
    }

    struct Manifest: Codable, Sendable {
        let duration: Double
        let size: Int64
        let modifiedAt: Double
        let frames: [Frame]
        var revision: String { VideoFrameMetadata(seconds: 0, duration: duration, size: size, modifiedAt: modifiedAt).revision }
    }

    struct Pool: Sendable {
        let directory: URL
        let manifest: Manifest
        func url(_ frame: Frame) -> URL { directory.appendingPathComponent(frame.file) }
    }

    static let directoryPrefix = "FileIDTimelinePool-"
    static let maxPixelSize = 448

    static func enabled(environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        environment["FILEID_VIDEO_SCENES"] != "0"
    }

    static func sweepAbandoned(directory: URL = FileManager.default.temporaryDirectory, now: Date = Date()) {
        guard let entries = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.contentModificationDateKey]) else { return }
        for entry in entries where entry.lastPathComponent.hasPrefix(directoryPrefix) {
            guard UUID(uuidString: String(entry.lastPathComponent.dropFirst(directoryPrefix.count))) != nil,
                  let modified = (try? entry.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate,
                  now.timeIntervalSince(modified) > 6 * 3600 else { continue }
            try? FileManager.default.removeItem(at: entry)
        }
    }

    /// Worker entry: `<source> <interval> <maxPixelSize> <outputDirectory>`.
    static func run(arguments: [String]) async -> Int32 {
        let parentPID = getppid()
        let monitor = Task.detached {
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 1_000_000_000) } catch { return }
                if getppid() == 1 || getppid() != parentPID { kill(getpid(), SIGKILL); return }
            }
        }
        defer { monitor.cancel() }
        guard arguments.count == 4,
              let interval = Double(arguments[1]), interval.isFinite, interval >= 0.1, interval <= 10,
              let maxPixel = Int(arguments[2]), (32...1024).contains(maxPixel) else { return 2 }
        let source = URL(fileURLWithPath: arguments[0])
        let output = URL(fileURLWithPath: arguments[3], isDirectory: true)
        do {
            try ReadOnlyLocations.requireWritable(output)
            let before = try attributes(source)
            let asset = AVURLAsset(url: source, options: [AVURLAssetPreferPreciseDurationAndTimingKey: false])
            guard let duration = await DeepAnalyze.loadVideoDurationSeconds(asset, timeoutSeconds: 8),
                  duration.isFinite, duration > 0 else { return 3 }
            var frames = (try? await readSequentially(asset: asset, interval: interval, maxPixel: maxPixel, output: output)) ?? []
            if frames.isEmpty {
                frames = try await seekEach(asset: asset, duration: duration, interval: interval, maxPixel: maxPixel, output: output)
            }
            guard frames.count >= 1 else { return 3 }
            guard try attributes(source) == before else { return 4 }
            let manifest = Manifest(duration: duration, size: before.size, modifiedAt: before.modified, frames: frames)
            try JSONEncoder().encode(manifest).write(to: output.appendingPathComponent("manifest.json"), options: .withoutOverwriting)
            return 0
        } catch {
            return 3
        }
    }

    static func newDirectory() throws -> URL {
        sweepAbandoned()
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(directoryPrefix + UUID().uuidString, isDirectory: true)
        try ReadOnlyLocations.requireWritable(directory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        return directory
    }

    static func readManifest(in directory: URL) throws -> Manifest {
        let handle = try FileHandle(forReadingFrom: directory.appendingPathComponent("manifest.json"))
        defer { try? handle.close() }
        guard let data = try handle.readToEnd() else { throw VideoFrameWorker.WorkerFailure() }
        let manifest = try JSONDecoder().decode(Manifest.self, from: data)
        guard manifest.duration.isFinite, manifest.duration > 0, !manifest.frames.isEmpty,
              manifest.frames.allSatisfy({ $0.seconds.isFinite && $0.seconds >= 0 && !$0.file.contains("/") }) else {
            throw VideoFrameWorker.WorkerFailure()
        }
        return manifest
    }

    static func remove(_ pool: Pool?) {
        guard let pool else { return }
        try? FileManager.default.removeItem(at: pool.directory)
    }

    /// Decodes the track once, front to back, on the hardware decoder with VideoToolbox
    /// scaling, and keeps the first frame at or after each `interval` step. Long-GOP
    /// 4K files cost one decode per frame instead of one GOP decode per seek.
    private static func readSequentially(asset: AVURLAsset, interval: Double, maxPixel: Int, output: URL) async throws -> [Frame] {
        guard let track = try await asset.loadTracks(withMediaType: .video).first else { return [] }
        let natural = try await track.load(.naturalSize)
        let transform = try await track.load(.preferredTransform)
        guard natural.width > 0, natural.height > 0 else { return [] }
        let scale = min(1, Double(maxPixel) / Double(max(natural.width, natural.height)))
        let width = max(2, Int((Double(natural.width) * scale / 2).rounded()) * 2)
        let height = max(2, Int((Double(natural.height) * scale / 2).rounded()) * 2)
        let reader = try AVAssetReader(asset: asset)
        let trackOutput = AVAssetReaderTrackOutput(track: track, outputSettings: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: width,
            kCVPixelBufferHeightKey as String: height,
        ])
        trackOutput.alwaysCopiesSampleData = false
        guard reader.canAdd(trackOutput) else { return [] }
        reader.add(trackOutput)
        guard reader.startReading() else { return [] }
        defer { reader.cancelReading() }
        let context = CIContext(options: [.useSoftwareRenderer: false])
        let toScaled = CGAffineTransform(scaleX: CGFloat(width) / natural.width, y: CGFloat(height) / natural.height)
        let flip = CGAffineTransform(scaleX: 1, y: -1)
        let oriented = flip.concatenating(toScaled.inverted()).concatenating(transform).concatenating(toScaled).concatenating(flip)
        var frames: [Frame] = []
        var next = 0.0
        var index = 0
        while let sample = trackOutput.copyNextSampleBuffer() {
            if Task.isCancelled { return [] }
            let seconds = CMSampleBufferGetPresentationTimeStamp(sample).seconds
            guard seconds.isFinite, seconds + 0.001 >= next, let buffer = CMSampleBufferGetImageBuffer(sample) else { continue }
            var image = CIImage(cvPixelBuffer: buffer).transformed(by: oriented)
            image = image.transformed(by: CGAffineTransform(translationX: -image.extent.minX, y: -image.extent.minY))
            guard let cg = context.createCGImage(image, from: image.extent) else { continue }
            let name = String(format: "frame-%05d.jpg", index)
            try writeJPEG(cg, to: output.appendingPathComponent(name))
            frames.append(Frame(seconds: seconds, file: name, signature: try signature(cg)))
            index += 1
            next = (floor(seconds / interval) + 1) * interval
        }
        return reader.status == .completed ? frames : []
    }

    private static func seekEach(asset: AVURLAsset, duration: Double, interval: Double, maxPixel: Int, output: URL) async throws -> [Frame] {
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: maxPixel, height: maxPixel)
        let tolerance = CMTime(seconds: interval / 4, preferredTimescale: 600)
        generator.requestedTimeToleranceBefore = tolerance
        generator.requestedTimeToleranceAfter = tolerance
        let ref = GeneratorRef(generator)
        var frames: [Frame] = []
        for (index, seconds) in VideoSceneSelection.poolTimes(duration: duration, interval: interval).enumerated() {
            guard !Task.isCancelled else { return [] }
            guard let (image, actual) = await generate(ref, seconds: seconds) else { continue }
            if let last = frames.last, abs(last.seconds - actual) < 0.001 { continue }
            let name = String(format: "frame-%05d.jpg", index)
            try writeJPEG(image, to: output.appendingPathComponent(name))
            frames.append(Frame(seconds: actual, file: name, signature: try signature(image)))
        }
        return frames
    }

    private struct GeneratorRef: @unchecked Sendable {
        let value: AVAssetImageGenerator
        init(_ value: AVAssetImageGenerator) { self.value = value }
    }
    private struct Generated: @unchecked Sendable { let image: CGImage; let seconds: Double }

    private static func generate(_ ref: GeneratorRef, seconds: Double) async -> (CGImage, Double)? {
        let result: Generated? = await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                ref.value.generateCGImageAsynchronously(for: CMTime(seconds: seconds, preferredTimescale: 600)) { image, time, _ in
                    continuation.resume(returning: image.map { Generated(image: $0, seconds: time.seconds) })
                }
            }
        } onCancel: { ref.value.cancelAllCGImageGeneration() }
        guard let result, result.seconds.isFinite else { return nil }
        return (result.image, result.seconds)
    }

    private static func writeJPEG(_ image: CGImage, to url: URL) throws {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, UTType.jpeg.identifier as CFString, 1, nil) else {
            throw VideoFrameWorker.WorkerFailure()
        }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: 0.88] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { throw VideoFrameWorker.WorkerFailure() }
        try (data as Data).write(to: url, options: .withoutOverwriting)
    }

    static func signature(_ image: CGImage) throws -> [UInt8] {
        let width = 32, height = 18
        var pixels = [UInt8](repeating: 0, count: width * height)
        let ok = pixels.withUnsafeMutableBytes { bytes -> Bool in
            guard let context = CGContext(data: bytes.baseAddress, width: width, height: height, bitsPerComponent: 8,
                                          bytesPerRow: width, space: CGColorSpaceCreateDeviceGray(),
                                          bitmapInfo: CGImageAlphaInfo.none.rawValue) else { return false }
            context.interpolationQuality = .low
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        guard ok else { throw VideoFrameWorker.WorkerFailure() }
        return pixels
    }

    private struct Attributes: Equatable { let size: Int64; let modified: Double }
    private static func attributes(_ source: URL) throws -> Attributes {
        let values = try source.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey])
        guard let size = values.fileSize, let modified = values.contentModificationDate else { throw VideoFrameWorker.WorkerFailure() }
        return Attributes(size: Int64(size), modified: modified.timeIntervalSince1970)
    }
}
