import AVFoundation
import CoreVideo
import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Native video stabilization", .serialized)
struct VideoStabilizationTests {
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
            let alignment = try #require(try VideoStabilization.alignment(reference: reference, current: current, sourceSize: CGSize(width: 320, height: 240)))
            let center = CGPoint(x: 160 + dx, y: 120 + dy).applying(alignment)
            #expect(hypot(center.x - 160, center.y - 120) < 2.5)
            #expect(abs(atan2(alignment.b, alignment.a) + angle) < 0.01)
        }
    }

    @Test func smoothingCorrectsKnownJitterAndNeverPublishesExposedBorders() throws {
        let size = CGSize(width: 320, height: 240)
        let observations = (0..<60).map { frame in
            VideoStabilization.Observation(time: Double(frame) / 30,
                                           alignment: frame == 0 ? nil : CGAffineTransform(translationX: frame.isMultiple(of: 2) ? -10 : 10, y: 0))
        }
        let plan = try VideoStabilization.plan(observations: observations, sourceSize: size)
        #expect(plan.correctedSamples > 40 && plan.zoom <= 1.12)
        for frame in plan.frames {
            let inverse = frame.transform.inverted()
            for corner in [CGPoint(x: 0, y: 0), CGPoint(x: size.width, y: 0), CGPoint(x: 0, y: size.height), CGPoint(x: size.width, y: size.height)] {
                let point = corner.applying(inverse)
                #expect(point.x >= -0.01 && point.y >= -0.01 && point.x <= size.width + 0.01 && point.y <= size.height + 0.01)
            }
        }
        let unavailable = try VideoStabilization.plan(observations: (0..<10).map { .init(time: Double($0) / 30, alignment: nil) }, sourceSize: size)
        #expect(unavailable.correctedSamples == 0 && unavailable.uncorrectedSamples == 9 && unavailable.zoom == 1)
        let unsafe = try VideoStabilization.plan(observations: [.init(time: 0, alignment: nil), .init(time: 1.0 / 30, alignment: CGAffineTransform(translationX: 250, y: 0))], sourceSize: size)
        #expect(unsafe.uncorrectedSamples == 2 && unsafe.frames.allSatisfy { $0.transform == .identity })
        #expect(throws: MediaTools.Failure.self) {
            try VideoStabilization.plan(observations: [.init(time: 1, alignment: nil), .init(time: 1, alignment: nil)], sourceSize: size)
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
            let deadline = ContinuousClock.now.advanced(by: .seconds(10))
            while !input.isReadyForMoreMediaData {
                guard ContinuousClock.now < deadline else { throw MediaTools.Failure(text: "Fixture encoder timed out.") }
                try await Task.sleep(for: .milliseconds(5))
            }
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
                if bytes[offset] > 180 && bytes[offset + 1] < 65 && bytes[offset + 2] < 65 {
                    redX += Double(column); redY += Double(row); redCount += 1
                }
                if bytes[offset + 1] > 180 && bytes[offset] < 65 && bytes[offset + 2] < 65 {
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

    @Test func actualWorkerReducesGeneratedTranslationAndRotationJitter() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Shaky.mov")
        try await movie(at: source)
        let original = try MediaTools.hash(source)
        let recipe = ToolRecipe(kind: "video", format: "mp4", maxDimension: 1280, videoStabilization: true)
        let preview = try await VideoConversionWorker.request(source: source, recipe: recipe)
        #expect(preview.stabilizationSummary?.contains("during export") == true)
        let output = root.appendingPathComponent("Steady.mp4")
        let result = try await VideoConversionWorker.request(source: source, output: output, recipe: recipe)
        #expect(result.stabilizationSummary?.contains("corrected samples") == true)
        #expect(abs(result.duration - 2) < 0.1 && abs(result.width / result.height - 4.0 / 3.0) < 0.01)
        let before = try jitter(at: source)
        let after = try jitter(at: output)
        print("STABILIZATION_QA: translation \(before.0) → \(after.0); rotation \(before.1) → \(after.1)")
        #expect(after.0 < before.0 * 0.65, "Translation jitter decreases: \(before.0) to \(after.0).")
        #expect(after.1 < before.1 * 0.75, "Rotation jitter decreases: \(before.1) to \(after.1).")
        #expect(try MediaTools.hash(source) == original)
    }

    @Test func stabilizedSocialExportPersistsRecipeRelationshipsAndUndo() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Shaky.mov")
        try await movie(at: source)
        let original = try MediaTools.hash(source)
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,?,1,100,0,'video','mov')", arguments: [source.path])
        }
        let tools = MediaTools()
        let recipe = ToolRecipe(kind: "video", format: "mp4", maxDimension: 1280, videoAspectRatio: "9:16", videoFraming: "auto", videoStabilization: true)
        let preview = await tools.handle(ToolRequest(requestID: "preview", action: "preview", fileIDs: [1], destination: root.path, recipe: recipe), database: database)
        #expect(preview.status == "ok", Comment(rawValue: preview.message))
        let id = try #require(preview.operationID)
        let exported = await tools.handle(ToolRequest(requestID: "execute", action: "execute", destination: root.path, operationID: id), database: database)
        #expect(exported.status == "ok", Comment(rawValue: exported.message))
        let output = URL(fileURLWithPath: try #require(exported.outputs.first).outputPath)
        #expect(output.lastPathComponent.contains("9x16 People - Stabilized"))
        #expect(exported.outputs.first?.message.contains("Stabilization:") == true)
        let stored = try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT recipe_json FROM catalog_assets WHERE original_id=1") }
        let decoded = try JSONDecoder().decode(ToolRecipe.self, from: Data(try #require(stored).utf8))
        #expect(decoded == recipe)
        let result = try await VideoConversionWorker.request(source: output, recipe: ToolRecipe(kind: "video", format: "mp4", maxDimension: 1280))
        #expect(abs(result.width / result.height - 9.0 / 16.0) < 0.01)
        let undone = await tools.handle(ToolRequest(requestID: "undo", action: "undo", destination: root.path, operationID: id), database: database)
        #expect(undone.status == "ok" && !FileManager.default.fileExists(atPath: output.path))
        #expect(try MediaTools.hash(source) == original)
    }
}
