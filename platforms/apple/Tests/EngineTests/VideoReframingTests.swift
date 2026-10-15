import AVFoundation
import CoreVideo
import Foundation
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("People video reframing", .serialized)
struct VideoReframingTests {
    @Test func smoothCropsKeepAllObservedPeopleAndFitOnLostOrUnsafeGroups() throws {
        let size = CGSize(width: 1920, height: 1080)
        let observations = (0...8).map { index in
            VideoReframing.Observation(time: Double(index) / 2,
                                       people: [CGRect(x: 0.30 + Double(index) * 0.01, y: 0.25, width: 0.1, height: 0.5)])
        }
        let plan = try VideoReframing.plan(observations: observations, sourceSize: size, ratio: 9.0 / 16.0)
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
        let missing = try VideoReframing.plan(observations: lost, sourceSize: size, ratio: 9.0 / 16.0)
        #expect(missing.frames[3].crop == nil && missing.frames[4].crop == nil && missing.frames[5].crop == nil)
        var group = observations
        group[4].people.append(CGRect(x: 0.8, y: 0.25, width: 0.1, height: 0.5))
        let unsafe = try VideoReframing.plan(observations: group, sourceSize: size, ratio: 9.0 / 16.0)
        #expect(unsafe.frames[4].crop == nil)
        var safeGroup = observations
        safeGroup[4].people.append(CGRect(x: 0.45, y: 0.25, width: 0.06, height: 0.5))
        let safe = try VideoReframing.plan(observations: safeGroup, sourceSize: size, ratio: 9.0 / 16.0)
        #expect(safe.frames[4].crop != nil)
    }

    @Test func invalidEvidenceCannotProduceACropAndTimestampsAreChecked() throws {
        for box in [CGRect(x: -0.1, y: 0.1, width: 0.2, height: 0.5),
                    CGRect(x: 0.9, y: 0.1, width: 0.2, height: 0.5),
                    CGRect(x: 0.1, y: 0.1, width: 0, height: 0.5),
                    CGRect(x: Double.nan, y: 0.1, width: 0.2, height: 0.5)] {
            let plan = try VideoReframing.plan(observations: [.init(time: 0, people: [box])], sourceSize: CGSize(width: 1920, height: 1080), ratio: 9.0 / 16.0)
            #expect(plan.followedSamples == 0 && plan.frames[0].crop == nil)
        }
        #expect(throws: MediaTools.Failure.self) {
            try VideoReframing.plan(observations: [.init(time: 1, people: []), .init(time: 0.5, people: [])], sourceSize: CGSize(width: 1920, height: 1080), ratio: 1)
        }
        let gap = try VideoReframing.plan(observations: [.init(time: 0, people: [CGRect(x: 0.4, y: 0.25, width: 0.1, height: 0.5)]),
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
            let deadline = ContinuousClock.now.advanced(by: .seconds(10))
            while !input.isReadyForMoreMediaData {
                guard ContinuousClock.now < deadline else { throw MediaTools.Failure(text: "Fixture encoder timed out.") }
                try await Task.sleep(for: .milliseconds(5))
            }
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
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Moving.mov")
        try await movie(at: source, rotated: rotated)
        let original = try MediaTools.hash(source)
        let size = rotated ? CGSize(width: 108, height: 192) : CGSize(width: 192, height: 108)
        let ratio = rotated ? 16.0 / 9.0 : 9.0 / 16.0
        let observations = (0...6).map { index in
            let x = 0.35 + Double(index) / 6 * 0.3 - 14.0 / 192
            let box = rotated ? CGRect(x: 0.25, y: x, width: 0.5, height: 28.0 / 192)
                : CGRect(x: x, y: 0.25, width: 28.0 / 192, height: 0.5)
            return VideoReframing.Observation(time: Double(index) / 2, people: [box])
        }
        let plan = try VideoReframing.plan(observations: observations, sourceSize: size, ratio: ratio)
        #expect(plan.followedSamples == observations.count)
        let asset = AVURLAsset(url: source)
        let exporter = try #require(AVAssetExportSession(asset: asset, presetName: AVAssetExportPreset1280x720))
        exporter.videoComposition = try await VideoReframing.composition(asset: asset, dimension: 1280, ratio: ratio, plan: plan)
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
        #expect(try MediaTools.hash(source) == original)
    }

    @Test func autoWorkerWithoutPeopleFitsAndReportsFallback() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Shapes.mov")
        try await movie(at: source, rotated: false)
        let original = try MediaTools.hash(source)
        let recipe = ToolRecipe(kind: "video", format: "mp4", maxDimension: 1280, videoAspectRatio: "9:16", videoFraming: "auto")
        let output = root.appendingPathComponent("Auto.mp4")
        let result = try await VideoConversionWorker.request(source: source, output: output, recipe: recipe)
        #expect(result.framingSummary?.contains("0 followed samples") == true)
        #expect(result.framingSummary?.contains("full-picture samples") == true)
        #expect(abs(result.width / result.height - 9.0 / 16.0) < 0.02)
        #expect(try MediaTools.hash(source) == original)
        let plan = try VideoReframing.analyze(asset: AVURLAsset(url: source), duration: 3, sourceSize: CGSize(width: 192, height: 108), ratio: 9.0 / 16.0)
        #expect(plan.followedSamples == 0 && plan.fittedSamples == 7)
        #expect(throws: MediaTools.Failure.self) {
            try VideoReframing.analyze(asset: AVURLAsset(url: source), duration: 601, sourceSize: CGSize(width: 192, height: 108), ratio: 9.0 / 16.0)
        }
    }
}
