import AVFoundation
import CoreGraphics
import CoreImage
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// Video frames for timeline analysis. The Rust engine calls these from its own
/// re-executed worker processes (`--sample-video`, `--sample-video-pool`), so a
/// decoder crash on a damaged file ends that worker, not the engine.
enum MediaFrames {
    struct Frame: Sendable {
        let rgb: [UInt8]
        let seconds: Double
    }

    private struct GeneratorRef: @unchecked Sendable {
        let value: AVAssetImageGenerator
    }

    private struct Generated: @unchecked Sendable {
        let image: CGImage
        let seconds: Double
    }

    private static func generate(_ ref: GeneratorRef, seconds: Double) async -> Generated? {
        await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                ref.value.generateCGImageAsynchronously(for: CMTime(seconds: seconds, preferredTimescale: 600)) { image, time, _ in
                    continuation.resume(returning: image.flatMap { time.seconds.isFinite ? Generated(image: $0, seconds: time.seconds) : nil })
                }
            }
        } onCancel: { ref.value.cancelAllCGImageGeneration() }
    }

    /// One upright frame at `seconds`, drawn into exactly `width` x `height` RGB.
    static func frame(source: URL, seconds: Double, width: Int, height: Int) async throws -> Frame {
        guard seconds.isFinite, seconds >= 0, (2...8192).contains(width), (2...8192).contains(height) else {
            throw MediaFailure("Invalid frame request.")
        }
        let asset = AVURLAsset(url: source, options: [AVURLAssetPreferPreciseDurationAndTimingKey: false])
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.requestedTimeToleranceBefore = .zero
        generator.requestedTimeToleranceAfter = .zero
        generator.maximumSize = CGSize(width: width, height: height)
        guard let generated = await generate(GeneratorRef(value: generator), seconds: seconds) else {
            throw MediaFailure("The video worker could not decode a frame at \(String(format: "%.3f", seconds)) seconds")
        }
        return Frame(rgb: try rgb(generated.image, width: width, height: height), seconds: generated.seconds)
    }

    static func rgb(_ image: CGImage, width: Int, height: Int) throws -> [UInt8] {
        var rgba = [UInt8](repeating: 0, count: width * height * 4)
        let drawn = rgba.withUnsafeMutableBytes { bytes -> Bool in
            guard let context = CGContext(data: bytes.baseAddress, width: width, height: height, bitsPerComponent: 8,
                                          bytesPerRow: width * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                          bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return false }
            context.interpolationQuality = .high
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        guard drawn else { throw MediaFailure("The video worker could not convert a frame.") }
        var rgb = [UInt8](repeating: 0, count: width * height * 3)
        for pixel in 0..<(width * height) {
            rgb[pixel * 3] = rgba[pixel * 4]
            rgb[pixel * 3 + 1] = rgba[pixel * 4 + 1]
            rgb[pixel * 3 + 2] = rgba[pixel * 4 + 2]
        }
        return rgb
    }

    // MARK: Frame pool (the Swift engine's `VideoFramePool`)

    struct PoolFrame: Sendable {
        let seconds: Double
        let file: String
        let signature: [UInt8]
    }

    enum PoolOutcome: Int32 {
        case ok = 0
        case invalid = 2
        case failed = 3
        case sourceChanged = 4
    }

    /// Decodes every `interval` frame no larger than `maxPixel` into `directory` as
    /// JPEGs plus `manifest.json` ({duration, size, modifiedAt, frames: [{seconds,
    /// file, signature}]}), the contract of the Swift engine's `--sample-video-pool`.
    static func pool(source: URL, interval: Double, maxPixel: Int, directory: URL, cancel: CancelFlag) async -> PoolOutcome {
        guard interval.isFinite, interval >= 0.1, interval <= 10, (32...1024).contains(maxPixel) else { return .invalid }
        do {
            let before = try MediaInspect.attributes(source)
            let asset = AVURLAsset(url: source, options: [AVURLAssetPreferPreciseDurationAndTimingKey: false])
            let duration = try await asset.load(.duration).seconds
            guard duration.isFinite, duration > 0 else { return .failed }
            var frames = (try? await readSequentially(asset: asset, interval: interval, maxPixel: maxPixel, output: directory, cancel: cancel)) ?? []
            if frames.isEmpty {
                removeFrames(in: directory)
                frames = try await seekEach(asset: asset, duration: duration, interval: interval, maxPixel: maxPixel, output: directory, cancel: cancel)
            }
            guard !frames.isEmpty, !cancel.isSet else { return .failed }
            guard try MediaInspect.attributes(source) == before else { return .sourceChanged }
            let manifest: [String: Any] = [
                "duration": duration, "size": before.size, "modifiedAt": before.modified,
                "frames": frames.map { ["seconds": $0.seconds, "file": $0.file, "signature": $0.signature.map(Int.init)] as [String: Any] },
            ]
            let data = try JSONSerialization.data(withJSONObject: manifest, options: [.sortedKeys])
            try data.write(to: directory.appendingPathComponent("manifest.json"), options: .withoutOverwriting)
            return .ok
        } catch {
            return .failed
        }
    }

    private static func removeFrames(in directory: URL) {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: directory.path)) ?? []
        for name in names where name.hasPrefix("frame-") && name.hasSuffix(".jpg") {
            try? FileManager.default.removeItem(at: directory.appendingPathComponent(name))
        }
    }

    static func nextSampleTime(after seconds: Double, interval: Double) -> Double {
        (floor((seconds + 0.001) / interval) + 1) * interval
    }

    static func poolTimes(duration: Double, interval: Double) -> [Double] {
        guard duration.isFinite, duration > 0, interval > 0 else { return [] }
        return stride(from: 0.0, to: duration, by: interval).map { $0 }
    }

    /// One hardware decode front to back with VideoToolbox scaling, keeping the
    /// first frame at or after each step: long-GOP 4K files cost one decode per
    /// frame instead of one GOP decode per seek.
    private static func readSequentially(asset: AVURLAsset, interval: Double, maxPixel: Int, output: URL, cancel: CancelFlag) async throws -> [PoolFrame] {
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
        var frames: [PoolFrame] = []
        var next = 0.0
        var index = 0
        while let sample = trackOutput.copyNextSampleBuffer() {
            if cancel.isSet { return [] }
            let seconds = CMSampleBufferGetPresentationTimeStamp(sample).seconds
            guard seconds.isFinite, seconds + 0.001 >= next, let buffer = CMSampleBufferGetImageBuffer(sample) else { continue }
            var image = CIImage(cvPixelBuffer: buffer).transformed(by: oriented)
            image = image.transformed(by: CGAffineTransform(translationX: -image.extent.minX, y: -image.extent.minY))
            guard let cg = context.createCGImage(image, from: image.extent) else { continue }
            let name = String(format: "frame-%05d.jpg", index)
            try writeJPEG(cg, to: output.appendingPathComponent(name))
            frames.append(PoolFrame(seconds: seconds, file: name, signature: try signature(cg)))
            index += 1
            next = nextSampleTime(after: seconds, interval: interval)
        }
        return reader.status == .completed ? frames : []
    }

    private static func seekEach(asset: AVURLAsset, duration: Double, interval: Double, maxPixel: Int, output: URL, cancel: CancelFlag) async throws -> [PoolFrame] {
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: maxPixel, height: maxPixel)
        let tolerance = CMTime(seconds: interval / 4, preferredTimescale: 600)
        generator.requestedTimeToleranceBefore = tolerance
        generator.requestedTimeToleranceAfter = tolerance
        let ref = GeneratorRef(value: generator)
        var frames: [PoolFrame] = []
        for (index, seconds) in poolTimes(duration: duration, interval: interval).enumerated() {
            guard !cancel.isSet else { return [] }
            guard let generated = await generate(ref, seconds: seconds) else { continue }
            if let last = frames.last, abs(last.seconds - generated.seconds) < interval / 2 { continue }
            let name = String(format: "frame-%05d.jpg", index)
            try writeJPEG(generated.image, to: output.appendingPathComponent(name))
            frames.append(PoolFrame(seconds: generated.seconds, file: name, signature: try signature(generated.image)))
        }
        return frames
    }

    private static func writeJPEG(_ image: CGImage, to url: URL) throws {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, UTType.jpeg.identifier as CFString, 1, nil) else {
            throw MediaFailure("The video worker could not encode a frame.")
        }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: 0.88] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { throw MediaFailure("The video worker could not encode a frame.") }
        try (data as Data).write(to: url, options: .withoutOverwriting)
    }

    /// 32x18 grayscale, the activity signature both engines compare.
    static func signature(_ image: CGImage) throws -> [UInt8] {
        let width = 32, height = 18
        var pixels = [UInt8](repeating: 0, count: width * height)
        let drawn = pixels.withUnsafeMutableBytes { bytes -> Bool in
            guard let context = CGContext(data: bytes.baseAddress, width: width, height: height, bitsPerComponent: 8,
                                          bytesPerRow: width, space: CGColorSpaceCreateDeviceGray(),
                                          bitmapInfo: CGImageAlphaInfo.none.rawValue) else { return false }
            context.interpolationQuality = .low
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        guard drawn else { throw MediaFailure("The video worker could not sign a frame.") }
        return pixels
    }
}
