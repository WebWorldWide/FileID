import AVFoundation
import CoreVideo
import Foundation
import Testing
@testable import FileIDAppleKit
@testable import FileIDVLMCore

// The follow-people and stabilization plans the Rust engine's video export runs
// through FileIDAppleKit. Ported from the Swift engine's VideoReframingTests and
// VideoStabilizationTests, which leave with that engine.

private func scratch() throws -> URL {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDAppleKit-plan-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    return root
}

private func sha(_ url: URL) throws -> Data { try Data(contentsOf: url) }

/// Every metadata item in every format of `asset` that names a place.
private func locationItems(_ asset: AVURLAsset) async throws -> [String] {
    var found: [String] = []
    for format in try await asset.load(.availableMetadataFormats) {
        for item in try await asset.loadMetadata(for: format) {
            let name = item.identifier?.rawValue ?? ""
            if name.contains("6709") || name.lowercased().contains("location") || name.contains("xyz") { found.append(name) }
        }
    }
    return found
}

private func waitUntilReady(_ input: AVAssetWriterInput) async throws {
    let deadline = ContinuousClock.now.advanced(by: .seconds(10))
    while !input.isReadyForMoreMediaData {
        guard ContinuousClock.now < deadline else { throw MediaFailure("Fixture encoder timed out.") }
        try await Task.sleep(for: .milliseconds(5))
    }
}

@Suite("FileIDAppleKit people framing plan", .serialized)
struct MediaReframingPlanTests {
    @Test func smoothCropsKeepAllObservedPeopleAndFitOnLostOrUnsafeGroups() throws {
        let size = CGSize(width: 1920, height: 1080)
        let observations = (0...8).map { index in
            MediaReframing.Observation(time: Double(index) / 2,
                                       people: [CGRect(x: 0.30 + Double(index) * 0.01, y: 0.25, width: 0.1, height: 0.5)])
        }
        let plan = try MediaReframing.plan(observations: observations, sourceSize: size, ratio: 9.0 / 16.0)
        #expect(plan.followedSamples == observations.count)
        for (observation, key) in zip(observations, plan.frames) {
            let crop = try #require(key.crop)
            for box in observation.people {
                #expect(crop.contains(CGRect(x: box.minX * size.width, y: box.minY * size.height,
                                            width: box.width * size.width, height: box.height * size.height)))
            }
            #expect(abs(crop.width / crop.height - 9.0 / 16.0) < 0.001)
        }
        for (left, right) in zip(plan.frames, plan.frames.dropFirst()) {
            #expect(abs(try #require(right.crop).minX - #require(left.crop).minX) < 40)
        }
        var lost = observations
        lost[4].people = []
        let missing = try MediaReframing.plan(observations: lost, sourceSize: size, ratio: 9.0 / 16.0)
        #expect(missing.frames[3].crop == nil && missing.frames[4].crop == nil && missing.frames[5].crop == nil)
        var group = observations
        group[4].people.append(CGRect(x: 0.8, y: 0.25, width: 0.1, height: 0.5))
        let unsafe = try MediaReframing.plan(observations: group, sourceSize: size, ratio: 9.0 / 16.0)
        #expect(unsafe.frames[4].crop == nil)
        var safeGroup = observations
        safeGroup[4].people.append(CGRect(x: 0.45, y: 0.25, width: 0.06, height: 0.5))
        let safe = try MediaReframing.plan(observations: safeGroup, sourceSize: size, ratio: 9.0 / 16.0)
        #expect(safe.frames[4].crop != nil)
    }

    @Test func invalidEvidenceCannotProduceACropAndTimestampsAreChecked() throws {
        for box in [CGRect(x: -0.1, y: 0.1, width: 0.2, height: 0.5),
                    CGRect(x: 0.9, y: 0.1, width: 0.2, height: 0.5),
                    CGRect(x: 0.1, y: 0.1, width: 0, height: 0.5),
                    CGRect(x: Double.nan, y: 0.1, width: 0.2, height: 0.5)] {
            let plan = try MediaReframing.plan(observations: [.init(time: 0, people: [box])], sourceSize: CGSize(width: 1920, height: 1080), ratio: 9.0 / 16.0)
            #expect(plan.followedSamples == 0 && plan.frames[0].crop == nil)
        }
        #expect(throws: MediaFailure.self) {
            try MediaReframing.plan(observations: [.init(time: 1, people: []), .init(time: 0.5, people: [])], sourceSize: CGSize(width: 1920, height: 1080), ratio: 1)
        }
        let gap = try MediaReframing.plan(observations: [.init(time: 0, people: [CGRect(x: 0.4, y: 0.25, width: 0.1, height: 0.5)]),
                                                       .init(time: 5, people: [CGRect(x: 0.4, y: 0.25, width: 0.1, height: 0.5)])],
                                         sourceSize: CGSize(width: 1920, height: 1080), ratio: 9.0 / 16.0)
        #expect(gap.frames.allSatisfy { $0.crop == nil })
    }

    private func movie(at url: URL, rotated: Bool) async throws {
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: 192, AVVideoHeightKey: 108])
        if rotated { input.transform = CGAffineTransform(a: 0, b: 1, c: -1, d: 0, tx: 108, ty: 0) }
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey as String: 192, kCVPixelBufferHeightKey as String: 108])
        writer.add(input)
        #expect(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        for frame in 0..<18 {
            try await waitUntilReady(input)
            var buffer: CVPixelBuffer?
            #expect(CVPixelBufferCreate(kCFAllocatorDefault, 192, 108, kCVPixelFormatType_32BGRA, nil, &buffer) == kCVReturnSuccess)
            let pixels = try #require(buffer)
            CVPixelBufferLockBaseAddress(pixels, [])
            let base = try #require(CVPixelBufferGetBaseAddress(pixels))
            let center = 192 * (0.35 + Double(frame) / 18 * 0.3)
            for row in 0..<108 {
                let line = base.advanced(by: row * CVPixelBufferGetBytesPerRow(pixels)).assumingMemoryBound(to: UInt8.self)
                for column in 0..<192 {
                    let offset = column * 4
                    let subject = abs(Double(column) - center) < 14 && (27..<81).contains(row)
                    line[offset] = subject ? 0 : 200
                    line[offset + 1] = 0
                    line[offset + 2] = subject ? 240 : 0
                    line[offset + 3] = 255
                }
            }
            CVPixelBufferUnlockBaseAddress(pixels, [])
            #expect(adaptor.append(pixels, withPresentationTime: CMTime(value: Int64(frame), timescale: 6)))
        }
        input.markAsFinished()
        await writer.finishWriting()
        #expect(writer.status == .completed)
    }

    private func centerPixel(_ image: CGImage) throws -> [UInt8] {
        var bytes = [UInt8](repeating: 0, count: image.width * image.height * 4)
        let drawn = bytes.withUnsafeMutableBytes { buffer -> Bool in
            guard let context = CGContext(data: buffer.baseAddress, width: image.width, height: image.height,
                                          bitsPerComponent: 8, bytesPerRow: image.width * 4,
                                          space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            return true
        }
        #expect(drawn)
        let offset = ((image.height / 2) * image.width + image.width / 2) * 4
        return Array(bytes[offset..<(offset + 4)])
    }

    @Test(arguments: [false, true]) func generatedMovingSubjectsFollowCropRampsAndPreferredOrientation(rotated: Bool) async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Moving.mov")
        try await movie(at: source, rotated: rotated)
        let original = try sha(source)
        let size = rotated ? CGSize(width: 108, height: 192) : CGSize(width: 192, height: 108)
        let ratio = rotated ? 16.0 / 9.0 : 9.0 / 16.0
        let observations = (0...6).map { index in
            let x = 0.35 + Double(index) / 6 * 0.3 - 14.0 / 192
            let box = rotated ? CGRect(x: 0.25, y: x, width: 0.5, height: 28.0 / 192)
                : CGRect(x: x, y: 0.25, width: 28.0 / 192, height: 0.5)
            return MediaReframing.Observation(time: Double(index) / 2, people: [box])
        }
        let plan = try MediaReframing.plan(observations: observations, sourceSize: size, ratio: ratio)
        #expect(plan.followedSamples == observations.count)
        let asset = AVURLAsset(url: source)
        let exporter = try #require(AVAssetExportSession(asset: asset, presetName: AVAssetExportPreset1280x720))
        exporter.videoComposition = try await MediaReframing.composition(asset: asset, dimension: 1280, ratio: ratio, plan: plan)
        let output = root.appendingPathComponent("Followed.mp4")
        try await exporter.export(to: output, as: .mp4)
        let generator = AVAssetImageGenerator(asset: AVURLAsset(url: output))
        generator.appliesPreferredTrackTransform = true
        for time in [0.0, 0.5, 1.0, 1.5, 2.0, 2.5] {
            let image = try generator.copyCGImage(at: CMTime(seconds: time, preferredTimescale: 600), actualTime: nil)
            #expect(abs(Double(image.width) / Double(image.height) - ratio) < 0.02)
            let center = try centerPixel(image)
            #expect(center[0] > 150 && center[2] < 60, "Moving red subject remains at the output center.")
        }
        #expect(try sha(source) == original)
    }

    @Test func autoFramingWithoutPeopleFitsReportsFallbackAndRefusesLongVideos() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Shapes.mov")
        try await movie(at: source, rotated: false)
        let original = try sha(source)
        let recipe = try MediaRecipe(["kind": "video", "format": "mp4", "maxDimension": 1280, "videoAspectRatio": "9:16", "videoFraming": "auto"])
        let output = root.appendingPathComponent(".FileIDExport-\(UUID().uuidString).part")
        let result = try await MediaExport.video(source: source, output: output, recipe: recipe, cancel: .never)
        #expect(result.framingSummary?.contains("0 followed samples") == true)
        #expect(result.framingSummary?.contains("full-picture samples") == true)
        #expect(abs(result.probe.width / result.probe.height - 9.0 / 16.0) < 0.02)
        #expect(try sha(source) == original)
        let plan = try MediaReframing.analyze(asset: AVURLAsset(url: source), duration: 3, sourceSize: CGSize(width: 192, height: 108), ratio: 9.0 / 16.0, cancel: .never)
        #expect(plan.followedSamples == 0 && plan.fittedSamples == 7)
        #expect(throws: MediaFailure.self) {
            try MediaReframing.analyze(asset: AVURLAsset(url: source), duration: 601, sourceSize: CGSize(width: 192, height: 108), ratio: 9.0 / 16.0, cancel: .never)
        }
    }
}

@Suite("FileIDAppleKit stabilization plan", .serialized)
struct MediaStabilizationPlanTests {
    private func draw(_ context: CGContext, width: Int, height: Int, dx: Double, dy: Double, angle: Double) {
        context.setFillColor(CGColor(gray: 0.18, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        context.translateBy(x: Double(width) / 2 + dx, y: Double(height) / 2 + dy)
        context.rotate(by: angle)
        context.translateBy(x: -Double(width) / 2, y: -Double(height) / 2)
        var random: UInt64 = 42
        for _ in 0..<180 {
            random = random &* 6_364_136_223_846_793_005 &+ 1
            let x = Double((random >> 16) % UInt64(width + 30)) - 15
            random = random &* 6_364_136_223_846_793_005 &+ 1
            let y = Double((random >> 16) % UInt64(height + 30)) - 15
            let value = Double((random >> 32) % 120 + 20) / 255
            context.setFillColor(CGColor(red: value, green: value * 0.8, blue: value * 0.6, alpha: 1))
            context.fill(CGRect(x: x, y: y, width: 8, height: 8))
            context.setStrokeColor(CGColor(gray: 0.85, alpha: 1))
            context.stroke(CGRect(x: x, y: y, width: 8, height: 8))
        }
        context.setFillColor(CGColor(red: 1, green: 0.02, blue: 0.02, alpha: 1))
        context.fillEllipse(in: CGRect(x: Double(width) / 2 - 9, y: Double(height) / 2 - 9, width: 18, height: 18))
        context.setFillColor(CGColor(red: 0.02, green: 1, blue: 0.02, alpha: 1))
        context.fillEllipse(in: CGRect(x: Double(width) / 2 + 50, y: Double(height) / 2 - 6, width: 12, height: 12))
    }

    private func image(dx: Double, dy: Double, angle: Double) throws -> CGImage {
        let context = try #require(CGContext(data: nil, width: 320, height: 240, bitsPerComponent: 8, bytesPerRow: 320 * 4,
                                             space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        draw(context, width: 320, height: 240, dx: dx, dy: dy, angle: angle)
        return try #require(context.makeImage())
    }

    @Test func nativeRegistrationEstimatesRealTranslationAndRotation() throws {
        let reference = try image(dx: 0, dy: 0, angle: 0)
        for (dx, dy, angle) in [(8.0, -5.0, 0.0), (0.0, 0.0, 0.02), (5.0, -3.0, 0.015)] {
            let current = try image(dx: dx, dy: dy, angle: angle)
            let alignment = try #require(try MediaStabilization.alignment(reference: reference, current: current, sourceSize: CGSize(width: 320, height: 240)))
            let center = CGPoint(x: 160 + dx, y: 120 + dy).applying(alignment)
            #expect(hypot(center.x - 160, center.y - 120) < 2.5)
            #expect(abs(atan2(alignment.b, alignment.a) + angle) < 0.01)
        }
    }

    @Test func smoothingCorrectsKnownJitterAndNeverPublishesExposedBorders() throws {
        let size = CGSize(width: 320, height: 240)
        let observations = (0..<60).map { frame in
            MediaStabilization.Observation(time: Double(frame) / 30,
                                           alignment: frame == 0 ? nil : CGAffineTransform(translationX: frame.isMultiple(of: 2) ? -10 : 10, y: 0))
        }
        let plan = try MediaStabilization.plan(observations: observations, sourceSize: size)
        #expect(plan.correctedSamples > 40 && plan.zoom <= 1.12)
        for frame in plan.frames {
            let inverse = frame.transform.inverted()
            for corner in [CGPoint(x: 0, y: 0), CGPoint(x: size.width, y: 0), CGPoint(x: 0, y: size.height), CGPoint(x: size.width, y: size.height)] {
                let point = corner.applying(inverse)
                #expect(point.x >= -0.01 && point.y >= -0.01 && point.x <= size.width + 0.01 && point.y <= size.height + 0.01)
            }
        }
        let unavailable = try MediaStabilization.plan(observations: (0..<10).map { .init(time: Double($0) / 30, alignment: nil) }, sourceSize: size)
        #expect(unavailable.correctedSamples == 0 && unavailable.uncorrectedSamples == 9 && unavailable.zoom == 1)
        let unsafe = try MediaStabilization.plan(observations: [.init(time: 0, alignment: nil), .init(time: 1.0 / 30, alignment: CGAffineTransform(translationX: 250, y: 0))], sourceSize: size)
        #expect(unsafe.uncorrectedSamples == 2 && unsafe.frames.allSatisfy { $0.transform == .identity })
        #expect(throws: MediaFailure.self) {
            try MediaStabilization.plan(observations: [.init(time: 1, alignment: nil), .init(time: 1, alignment: nil)], sourceSize: size)
        }
    }

    private func movie(at url: URL) async throws {
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: 320, AVVideoHeightKey: 240,
                                                                         AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: 2_000_000]])
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey as String: 320, kCVPixelBufferHeightKey as String: 240])
        writer.add(input)
        #expect(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        for frame in 0..<60 {
            try await waitUntilReady(input)
            var buffer: CVPixelBuffer?
            #expect(CVPixelBufferCreate(kCFAllocatorDefault, 320, 240, kCVPixelFormatType_32BGRA, nil, &buffer) == kCVReturnSuccess)
            let pixels = try #require(buffer)
            CVPixelBufferLockBaseAddress(pixels, [])
            let context = try #require(CGContext(data: CVPixelBufferGetBaseAddress(pixels), width: 320, height: 240, bitsPerComponent: 8,
                                                 bytesPerRow: CVPixelBufferGetBytesPerRow(pixels), space: CGColorSpaceCreateDeviceRGB(),
                                                 bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue))
            draw(context, width: 320, height: 240, dx: frame.isMultiple(of: 2) ? 5 : -5,
                 dy: frame.isMultiple(of: 3) ? 3 : -3, angle: frame.isMultiple(of: 2) ? 0.012 : -0.012)
            CVPixelBufferUnlockBaseAddress(pixels, [])
            #expect(adaptor.append(pixels, withPresentationTime: CMTime(value: Int64(frame), timescale: 30)))
        }
        input.markAsFinished()
        await writer.finishWriting()
        #expect(writer.status == .completed)
    }

    private func markerPositions(_ image: CGImage) throws -> (CGPoint, CGPoint) {
        var bytes = [UInt8](repeating: 0, count: image.width * image.height * 4)
        let drawn = bytes.withUnsafeMutableBytes { buffer -> Bool in
            guard let context = CGContext(data: buffer.baseAddress, width: image.width, height: image.height, bitsPerComponent: 8,
                                          bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(),
                                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            return true
        }
        #expect(drawn)
        var redX = 0.0, redY = 0.0, redCount = 0.0
        var greenX = 0.0, greenY = 0.0, greenCount = 0.0
        for row in 0..<image.height {
            for column in 0..<image.width {
                let offset = (row * image.width + column) * 4
                // Dominance rather than absolute floors: software H.264 encoders
                // (hosted CI VMs) lift the minor channels of saturated markers.
                let r = Int(bytes[offset]), g = Int(bytes[offset + 1]), b = Int(bytes[offset + 2])
                if r > 160 && r - g > 100 && r - b > 100 {
                    redX += Double(column); redY += Double(row); redCount += 1
                }
                if g > 160 && g - r > 90 && g - b > 90 {
                    greenX += Double(column); greenY += Double(row); greenCount += 1
                }
            }
        }
        #expect(redCount > 0 && greenCount > 0)
        return (CGPoint(x: redX / redCount / Double(image.width), y: redY / redCount / Double(image.height)),
                CGPoint(x: greenX / greenCount / Double(image.width), y: greenY / greenCount / Double(image.height)))
    }

    private func jitter(at url: URL) throws -> (Double, Double) {
        let generator = AVAssetImageGenerator(asset: AVURLAsset(url: url))
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: 320, height: 320)
        generator.requestedTimeToleranceBefore = .zero
        generator.requestedTimeToleranceAfter = .zero
        var x: [Double] = [], y: [Double] = [], angle: [Double] = []
        for frame in 6..<54 {
            let image = try generator.copyCGImage(at: CMTime(value: Int64(frame), timescale: 30), actualTime: nil)
            let positions = try markerPositions(image)
            x.append(positions.0.x); y.append(positions.0.y)
            angle.append(atan2(positions.1.y - positions.0.y, positions.1.x - positions.0.x))
        }
        func variance(_ values: [Double]) -> Double {
            let mean = values.reduce(0, +) / Double(values.count)
            return values.reduce(0) { $0 + ($1 - mean) * ($1 - mean) } / Double(values.count)
        }
        return (variance(x) + variance(y), variance(angle))
    }

    @Test func stabilizedExportReducesGeneratedTranslationAndRotationJitter() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Shaky.mov")
        try await movie(at: source)
        let original = try sha(source)
        let recipe = try MediaRecipe(["kind": "video", "format": "mp4", "maxDimension": 1280, "videoStabilization": true])
        let preview = try await MediaExport.video(source: source, output: nil, recipe: recipe, cancel: .never)
        #expect(preview.stabilizationSummary?.contains("during export") == true)
        let output = root.appendingPathComponent(".FileIDExport-\(UUID().uuidString).mp4")
        let result = try await MediaExport.video(source: source, output: output, recipe: recipe, cancel: .never)
        #expect(result.stabilizationSummary?.contains("corrected samples") == true)
        #expect(abs(result.probe.duration - 2) < 0.1 && abs(result.probe.width / result.probe.height - 4.0 / 3.0) < 0.01)
        let before = try jitter(at: source)
        let after = try jitter(at: output)
        #expect(after.0 < before.0 * 0.65, "Translation jitter decreases: \(before.0) to \(after.0).")
        #expect(after.1 < before.1 * 0.75, "Rotation jitter decreases: \(before.1) to \(after.1).")
        #expect(try sha(source) == original)
    }
}

@Suite("Describe Files video keyframe")
struct DescribeKeyframeTests {
    @Test func videoKeyframeTargetIsOneQuarterOfDuration() {
        #expect(abs(DeepAnalyze.representativeVideoTime(durationSeconds: 120).seconds - 30) < 0.001)
        #expect(abs(DeepAnalyze.representativeVideoTime(durationSeconds: 3).seconds - 0.75) < 0.001)
        #expect(abs(DeepAnalyze.representativeVideoTime(durationSeconds: nil).seconds - 1) < 0.001)
        #expect(abs(DeepAnalyze.representativeVideoTime(durationSeconds: .infinity).seconds - 1) < 0.001)
    }
}

// Ported from the Swift engine's VideoConversionTests and CommonMediaToolsTests:
// the same exports, run through the adapter the Rust engine calls.
@Suite("FileIDAppleKit video export results", .serialized)
struct MediaVideoExportTests {
    private func movie(at url: URL, marker: Bool = false) async throws {
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: 96, AVVideoHeightKey: 54])
        if !marker { input.transform = CGAffineTransform(a: 0, b: 1, c: -1, d: 0, tx: 54, ty: 0) }
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey as String: 96, kCVPixelBufferHeightKey as String: 54])
        writer.add(input)
        #expect(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        for frame in 0..<30 {
            try await waitUntilReady(input)
            var buffer: CVPixelBuffer?
            #expect(CVPixelBufferCreate(kCFAllocatorDefault, 96, 54, kCVPixelFormatType_32BGRA, nil, &buffer) == kCVReturnSuccess)
            let pixels = try #require(buffer)
            CVPixelBufferLockBaseAddress(pixels, [])
            let base = try #require(CVPixelBufferGetBaseAddress(pixels))
            for row in 0..<54 {
                let line = base.advanced(by: row * CVPixelBufferGetBytesPerRow(pixels)).assumingMemoryBound(to: UInt8.self)
                for column in 0..<96 {
                    let value: UInt8 = marker ? (column < 32 && row < 18 ? 255 : 0) : UInt8(frame * 5)
                    line[column * 4] = value; line[column * 4 + 1] = value; line[column * 4 + 2] = value; line[column * 4 + 3] = 255
                }
            }
            CVPixelBufferUnlockBaseAddress(pixels, [])
            #expect(adaptor.append(pixels, withPresentationTime: CMTime(value: Int64(frame), timescale: 30)))
        }
        input.markAsFinished()
        await writer.finishWriting()
        #expect(writer.status == .completed)
    }

    /// A one-second portrait clip with a 440 Hz AAC-able tone and a location tag.
    private func movieWithTone(in root: URL) async throws -> URL {
        let silent = root.appendingPathComponent("Silent.mov")
        try await movie(at: silent)
        var wave = Data()
        func number<T: FixedWidthInteger>(_ value: T) {
            var little = value.littleEndian
            withUnsafeBytes(of: &little) { wave.append(contentsOf: $0) }
        }
        wave.append(Data("RIFF".utf8)); number(UInt32(36 + 96_000))
        wave.append(Data("WAVEfmt ".utf8)); number(UInt32(16)); number(UInt16(1)); number(UInt16(1))
        number(UInt32(48_000)); number(UInt32(96_000)); number(UInt16(2)); number(UInt16(16))
        wave.append(Data("data".utf8)); number(UInt32(96_000))
        for sample in 0..<48_000 { number(Int16(sin(Double(sample) * 2 * .pi * 440 / 48_000) * 1000)) }
        let audioURL = root.appendingPathComponent("Tone.wav")
        try wave.write(to: audioURL)
        let movie = AVURLAsset(url: silent)
        let audio = AVURLAsset(url: audioURL)
        let videoSource = try #require(try await movie.loadTracks(withMediaType: .video).first)
        let audioSource = try #require(try await audio.loadTracks(withMediaType: .audio).first)
        let composition = AVMutableComposition()
        let videoTrack = try #require(composition.addMutableTrack(withMediaType: .video, preferredTrackID: kCMPersistentTrackID_Invalid))
        let audioTrack = try #require(composition.addMutableTrack(withMediaType: .audio, preferredTrackID: kCMPersistentTrackID_Invalid))
        let range = CMTimeRange(start: .zero, duration: CMTime(value: 1, timescale: 1))
        try videoTrack.insertTimeRange(range, of: videoSource, at: .zero)
        videoTrack.preferredTransform = try await videoSource.load(.preferredTransform)
        try audioTrack.insertTimeRange(range, of: audioSource, at: .zero)
        let output = root.appendingPathComponent("PortraitWithTone.mov")
        let exporter = try #require(AVAssetExportSession(asset: composition, presetName: AVAssetExportPresetHighestQuality))
        let location = AVMutableMetadataItem()
        location.identifier = .quickTimeMetadataLocationISO6709
        location.value = "+10.0000+020.0000/" as NSString
        location.dataType = kCMMetadataBaseDataType_UTF8 as String
        exporter.metadata = [location]
        try await exporter.export(to: output, as: .mov)
        return output
    }

    private func staged(_ root: URL) -> URL { root.appendingPathComponent(".FileIDExport-\(UUID().uuidString).part") }

    private func video(_ recipe: [String: Any], source: URL, output: URL?) async throws -> MediaProbe {
        try await MediaExport.video(source: source, output: output, recipe: try MediaRecipe(recipe), cancel: .never).probe
    }

    @Test func videoAndAudioTimingSurviveConversionAndLocationIsStripped() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try await movieWithTone(in: root)
        let original = try sha(source)
        #expect(try await !locationItems(AVURLAsset(url: source)).isEmpty)
        let recipe: [String: Any] = ["kind": "video", "format": "mp4", "maxDimension": 1920]
        let before = try await video(recipe, source: source, output: nil)
        let output = staged(root)
        let after = try await video(recipe, source: source, output: output)
        #expect(before.audioCount == 1 && after.audioCount == 1)
        #expect(after.audioCodec == "aac")
        let exported = AVURLAsset(url: output, options: [AVURLAssetOverrideMIMETypeKey: "video/mp4", AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue])
        #expect(try await locationItems(exported).isEmpty)
        #expect(abs(after.duration - before.duration) < 0.1)
        #expect(after.height > after.width)
        #expect(try sha(source) == original)
    }

    @Test func remuxCopiesStreamsAndStripsLocation() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try await movieWithTone(in: root)
        let original = try sha(source)
        let output = root.appendingPathComponent(".FileIDExport-\(UUID().uuidString).mov")
        let before = try await MediaExport.common(source: source, output: output,
                                                  recipe: try MediaRecipe(["kind": "remux", "format": "mov", "maxDimension": 1920]), cancel: .never)
        let after = try await MediaInspect.probe(output, strict: true)
        #expect(after.videoCodec == before.videoCodec && after.audioCodec == before.audioCodec)
        #expect(after.rotation == before.rotation && abs(after.duration - before.duration) < 0.1)
        #expect(try await locationItems(AVURLAsset(url: output)).isEmpty)
        #expect(try sha(source) == original)
    }

    @Test func framedExportsPreservePictureTimingAndAudioAndPadRatherThanStretch() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = try await movieWithTone(in: root)
        let original = try sha(source)
        for (frame, ratio, dimension) in [("9:16", 9.0 / 16.0, 1280), ("16:9", 16.0 / 9.0, 1280),
                                          ("1:1", 1.0, 1280), ("4:5", 4.0 / 5.0, 1280),
                                          ("9:16", 9.0 / 16.0, 1920)] {
            let output = staged(root)
            let result = try await video(["kind": "video", "format": "mp4", "maxDimension": dimension, "videoAspectRatio": frame], source: source, output: output)
            #expect(abs(result.width / result.height - ratio) < 0.02, "\(frame) at \(dimension)")
            #expect(max(result.width, result.height) <= Double(dimension) + 2)
            #expect(result.audioCount == 1 && result.audioCodec == "aac")
            #expect(abs(result.duration - 1) < 0.1)
            #expect(try sha(source) == original)
            if frame == "16:9" {
                let generator = AVAssetImageGenerator(asset: AVURLAsset(url: output, options: [AVURLAssetOverrideMIMETypeKey: "video/mp4"]))
                generator.appliesPreferredTrackTransform = true
                let image = try generator.copyCGImage(at: CMTime(value: 15, timescale: 30), actualTime: nil)
                let (width, height) = (image.width, image.height)
                var pixels = [UInt8](repeating: 0, count: width * height * 4)
                let drawn = pixels.withUnsafeMutableBytes { buffer -> Bool in
                    guard let context = CGContext(data: buffer.baseAddress, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                                                  space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
                    context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
                    return true
                }
                #expect(drawn)
                let middle = height / 2
                #expect(pixels[(middle * width + width / 2) * 4] > 25, "the picture is in the middle")
                #expect(pixels[(middle * width + 5) * 4] < 15, "the sides are padded, not stretched")
            }
        }
    }

    @Test func conventionalVideoEnlargementIsOptInAndPreservesTiming() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("source.mov")
        try await movie(at: source)
        let original = try sha(source)
        let recipe: [String: Any] = ["kind": "video", "format": "mp4", "maxDimension": 1280, "allowUpscale": true]
        let before = try await video(recipe, source: source, output: nil)
        let after = try await video(recipe, source: source, output: staged(root))
        #expect(max(after.width, after.height) == 1280)
        #expect(max(before.width, before.height) < max(after.width, after.height))
        #expect(abs(after.duration - before.duration) < 0.1)
        #expect(after.audioCount == before.audioCount)
        #expect(try sha(source) == original)
    }

    /// Luma of each pixel of the displayed (transform-applied) frame, row 0 at the top.
    private func displayedLuma(_ url: URL) throws -> (width: Int, height: Int, luma: [UInt8]) {
        let generator = AVAssetImageGenerator(asset: AVURLAsset(url: url))
        generator.appliesPreferredTrackTransform = true
        generator.requestedTimeToleranceBefore = .zero
        generator.requestedTimeToleranceAfter = .zero
        let image = try generator.copyCGImage(at: CMTime(value: 1, timescale: 2), actualTime: nil)
        let (width, height) = (image.width, image.height)
        var rgba = [UInt8](repeating: 0, count: width * height * 4)
        let drawn = rgba.withUnsafeMutableBytes { bytes -> Bool in
            guard let context = CGContext(data: bytes.baseAddress, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                                          space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        try #require(drawn)
        var luma = [UInt8](repeating: 0, count: width * height)
        for pixel in 0..<(width * height) {
            luma[pixel] = UInt8((Int(rgba[pixel * 4]) + Int(rgba[pixel * 4 + 1]) + Int(rgba[pixel * 4 + 2])) / 3)
        }
        return (width, height, luma)
    }

    @Test func rotatingNinetyRightMovesTheTopLeftMarkerToTheTopRight() async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Marker.mov")
        try await movie(at: source, marker: true)
        let sample = { (frame: (width: Int, height: Int, luma: [UInt8]), x: Int, y: Int) in frame.luma[y * frame.width + x] }
        let before = try displayedLuma(source)
        #expect(before.width == 96 && before.height == 54)
        #expect(sample(before, 12, 6) > 180 && sample(before, 84, 6) < 70 && sample(before, 12, 48) < 70)
        let output = root.appendingPathComponent(".FileIDExport-\(UUID().uuidString).mov")
        _ = try await MediaExport.common(source: source, output: output,
                                         recipe: try MediaRecipe(["kind": "rotate", "format": "mov", "maxDimension": 1920, "rotationDegrees": 90]), cancel: .never)
        let after = try displayedLuma(output)
        #expect(after.width == 54 && after.height == 96)
        // Turned 90 degrees clockwise, the top-left block (x < 32, y < 18) lands at x >= 36, y < 32.
        #expect(sample(after, 45, 10) > 180, "marker should be top right")
        #expect(sample(after, 8, 10) < 70, "top left should be dark")
        #expect(sample(after, 45, 85) < 70, "bottom right should be dark")
        #expect(sample(after, 8, 85) < 70, "bottom left should be dark")
    }

    @Test(arguments: [90, 180, 270]) func rotationAddsToAPortraitSourcesRotationWithoutReencoding(degrees: Int) async throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Portrait.mov")
        try await movie(at: source)
        let before = try await MediaInspect.probe(source, strict: true)
        #expect(before.rotation == 90 && before.height > before.width)
        let output = root.appendingPathComponent(".FileIDExport-\(UUID().uuidString).mov")
        _ = try await MediaExport.common(source: source, output: output,
                                         recipe: try MediaRecipe(["kind": "rotate", "format": "mov", "maxDimension": 1920, "rotationDegrees": degrees]), cancel: .never)
        let after = try await MediaInspect.probe(output, strict: true)
        #expect(after.rotation == (before.rotation + degrees) % 360)
        #expect(after.videoCodec == before.videoCodec)
        #expect((after.width > after.height) == (degrees != 180), "a quarter turn of a portrait clip is landscape")
    }
}
