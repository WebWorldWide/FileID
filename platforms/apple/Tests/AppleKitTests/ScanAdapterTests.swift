import CoreGraphics
import Foundation
import ImageIO
import Testing
import UniformTypeIdentifiers
@testable import FileIDAppleKit
@testable import FileIDImaging

/// Stage 5 scan adapters: every export, on synthetic files (no models needed).
@Suite("FileIDAppleKit scan adapters", .serialized)
struct ScanAdapterTests {
    private static let repo = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()

    private func folder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDScan-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func writePNG(_ url: URL, width: Int, height: Int) throws {
        let ctx = try #require(CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                         space: CGColorSpaceCreateDeviceRGB(),
                                         bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        for x in 0..<width {
            ctx.setFillColor(CGColor(red: 1 - CGFloat(x) / CGFloat(width), green: 0.4, blue: 0.3, alpha: 1))
            ctx.fill(CGRect(x: x, y: 0, width: 1, height: height))
        }
        let image = try #require(ctx.makeImage())
        let destination = try #require(CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(destination, image, nil)
        #expect(CGImageDestinationFinalize(destination))
    }

    private func json(_ pointer: UnsafeMutablePointer<CChar>?) throws -> [String: Any] {
        let text = try #require(pointer.map { String(cString: $0) })
        fileid_apple_free(pointer)
        return try #require(try JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any])
    }

    @Test func decodesTheScanImageWithExifAndDHash() throws {
        let camera = Self.repo.appendingPathComponent("shared/test-corpus/conformance-media/camera.jpg")
        var image: UInt64 = 0
        let info = try json(camera.path.withCString { fileid_apple_image_decode($0, 0, 0, &image) })
        #expect(image != 0)
        #expect(info["width"] as? Int == 1536)
        #expect(info["height"] as? Int == 1024)
        #expect(info["dhash"] == nil)
        #expect(info["cameraModel"] as? String == "ILCE-7M3")
        #expect(info["lat"] as? Double == 52.52)
        #expect(info["lon"] as? Double == -13.41)
        #expect(info["capturedAt"] as? Double == 1_717_230_030.25)

        var pixels: UnsafeMutablePointer<UInt8>?
        var length = 0
        var width: UInt32 = 0
        var height: UInt32 = 0
        #expect(fileid_apple_image_rgb(image, &pixels, &length, &width, &height))
        #expect(length == Int(width) * Int(height) * 3)
        fileid_apple_free(pixels)
        // CoreGraphics resampling differs by a bit across macOS versions; parity with
        // the Swift engine's own dHash is pinned by the scan-mac-adapters conformance case.
        let hash = fileid_apple_image_dhash(image)
        #expect(hash != 0)
        #expect((hash ^ 1_212_696_624).nonzeroBitCount <= 4)
        let before = ScanImages.shared.count
        fileid_apple_image_release(image)
        #expect(ScanImages.shared.count == before - 1)
        #expect(!fileid_apple_image_rgb(image, &pixels, &length, &width, &height))
    }

    @Test func refusesTinyAndMissingFiles() throws {
        let dir = try folder()
        defer { try? FileManager.default.removeItem(at: dir) }
        let tiny = dir.appendingPathComponent("tiny.jpg")
        try Data("not an image".utf8).write(to: tiny)
        var image: UInt64 = 7
        #expect(tiny.path.withCString { fileid_apple_image_decode($0, 12, 0, &image) } == nil)
        #expect(image == 0)
        #expect(dir.appendingPathComponent("missing.png").path.withCString { fileid_apple_image_decode($0, 0, 0, &image) } == nil)
        #expect(fileid_apple_image_decode(nil, 0, 0, &image) == nil)
    }

    @Test func primaryPassAndTextReadOnAScreenshot() throws {
        let text = Self.repo.appendingPathComponent("shared/test-corpus/ocr/known-text.png")
        var image: UInt64 = 0
        _ = try json(text.path.withCString { fileid_apple_image_decode($0, 0, 0, &image) })
        defer { fileid_apple_image_release(image) }
        let pass = try json(fileid_apple_vision_primary(image))
        #expect(pass["complete"] as? Bool == true)
        #expect(pass["faceCount"] as? Int == 0)
        #expect((pass["classifyTags"] as? [String])?.contains("document") == true)
        let request = #"{"classifyTags":["document"],"tags":["document"],"hasCamera":false}"#
        let read = try json(request.withCString { fileid_apple_text_read(image, $0) })
        #expect(read["ran"] as? Bool == true)
        #expect((read["text"] as? String)?.contains("OCR 12345") == true)
        #expect((read["carefulOwed"] as? Bool ?? false) == !fileid_apple_ocr_ready(), "a careful read the cold reader could not run is reported as owed")
        let skipped = try json(#"{"classifyTags":[],"tags":[],"hasCamera":true}"#.withCString { fileid_apple_text_read(image, $0) })
        #expect(skipped["ran"] as? Bool == false)
        #expect(fileid_apple_vision_primary(0) == nil)
    }

    @Test func readsAnImageOnlyPDF() throws {
        let pdf = Self.repo.appendingPathComponent("shared/test-corpus/conformance-media/scanned.pdf")
        let read = try json(pdf.path.withCString { fileid_apple_pdf_ocr($0) })
        #expect(read["opened"] as? Bool == true)
        #expect((read["text"] as? String)?.contains("OCR 12345") == true)
        #expect((read["carefulOwed"] as? Bool ?? false) == !fileid_apple_ocr_ready())
        let missing = try json("/nonexistent/scan.pdf".withCString { fileid_apple_pdf_ocr($0) })
        #expect(missing["opened"] as? Bool == false)
    }

    @Test func extractsDocumentTextLikeDocText() throws {
        let dir = try folder()
        defer { try? FileManager.default.removeItem(at: dir) }
        let rtf = dir.appendingPathComponent("letter.rtf")
        try Data(#"{\rtf1\ansi Dear reader,\par adapters read RTF.}"#.utf8).write(to: rtf)
        var handled = false
        let text = rtf.path.withCString { fileid_apple_document_text($0, 4000, &handled) }
        #expect(handled)
        #expect(text.map { String(cString: $0) } == "Dear reader,\nadapters read RTF.")
        fileid_apple_free(text)
        let pptx = dir.appendingPathComponent("deck.pptx")
        try Data([0x50, 0x4b, 0x03, 0x04]).write(to: pptx)
        #expect(pptx.path.withCString { fileid_apple_document_text($0, 4000, &handled) } == nil)
        #expect(!handled)
        let fakeDocx = dir.appendingPathComponent("fake.docx")
        try Data("plain".utf8).write(to: fakeDocx)
        #expect(fakeDocx.path.withCString { fileid_apple_document_text($0, 4000, &handled) } == nil)
        #expect(handled)
        let cut = String(repeating: "é", count: 10)
        #expect(ScanDocuments.utf8Text(Data(cut.utf8).dropLast()) == String(repeating: "é", count: 9))
    }

    @Test func videoKeyframeOfAnUnreadableFileIsNil() throws {
        let dir = try folder()
        defer { try? FileManager.default.removeItem(at: dir) }
        let video = dir.appendingPathComponent("clip.mov")
        try Data(count: 4096).write(to: video)
        var image: UInt64 = 9
        #expect(video.path.withCString { fileid_apple_video_keyframe($0, 512, 3000, &image) } == nil)
        #expect(image == 0)
    }

    @Test func unloadWithNothingLoadedIsANoOpAndLoadsStillFailAfterIt() throws {
        let dir = try folder()
        defer { try? FileManager.default.removeItem(at: dir) }
        fileid_apple_scan_models_unload()
        fileid_apple_scan_models_unload()
        #expect(!dir.appendingPathComponent("clip.onnx").path.withCString { fileid_apple_clip_load($0) })
        #expect(!dir.path.withCString { fileid_apple_ramplus_load($0) })
        fileid_apple_scan_models_unload()
        #expect(!ScanCLIP.shared.unload())
        #expect(!ScanRamPlus.shared.unload())
    }

    @Test func modelsRefuseMissingWeightsAndUnloadedCalls() throws {
        let dir = try folder()
        defer { try? FileManager.default.removeItem(at: dir) }
        #expect(!dir.appendingPathComponent("clip.onnx").path.withCString { fileid_apple_clip_load($0) })
        #expect(!dir.path.withCString { fileid_apple_ramplus_load($0) })
        let png = dir.appendingPathComponent("plain.png")
        try writePNG(png, width: 64, height: 48)
        var image: UInt64 = 0
        _ = try json(png.path.withCString { fileid_apple_image_decode($0, 0, 0, &image) })
        defer { fileid_apple_image_release(image) }
        var vector = [Float](repeating: 0, count: 512)
        #expect(!fileid_apple_clip_embed(image, &vector, vector.count))
        #expect("{}".withCString { fileid_apple_ramplus_tag(image, $0, 0) } == nil)
    }

    private final class ModelProbe: @unchecked Sendable {
        let lock = NSLock()
        private(set) var releasedOn: Thread?
        private(set) var ranOn: Thread?
        func ran() { lock.lock(); ranOn = Thread.current; lock.unlock() }
        func released() { lock.lock(); releasedOn = Thread.current; lock.unlock() }
    }

    private final class ProbedModel: @unchecked Sendable {
        let probe: ModelProbe
        init(_ probe: ModelProbe) { self.probe = probe }
        deinit { probe.released() }
    }

    private static func probedBatcher(_ probe: ModelProbe) -> GPURequestBatcher {
        let model = ProbedModel(probe)
        return GPURequestBatcher(label: "test.batcher", maxBatch: 4, gather: .milliseconds(2), usesMLX: false) { pixels, count in
            model.probe.ran()
            return (0..<count).map { [pixels[$0]] }
        }
    }

    /// MLX keeps its compile cache per thread, so a compiled model's weights are
    /// freed only if the model is dropped on the thread that ran it.
    @Test func requestBatcherAnswersEveryRequestAndDropsItsModelOnItsOwnThread() {
        let probe = ModelProbe()
        var answers: [Float?] = []
        do {
            let batcher = Self.probedBatcher(probe)
            let lock = NSLock()
            var collected = [Float?](repeating: nil, count: 12)
            DispatchQueue.concurrentPerform(iterations: 12) { i in
                let answer = batcher.submit([Float(i)])?.first
                lock.lock(); collected[i] = answer; lock.unlock()
            }
            answers = collected
        }
        #expect(answers == (0..<12).map { Float($0) })
        #expect(probe.releasedOn != nil)
        #expect(probe.releasedOn == probe.ranOn)
        #expect(probe.releasedOn != Thread.current)
    }

    @Test func ramPlusRefineMatchesTheEngine() {
        let refined = ScanRamPlus.refine([("baseball bat", 0.9), ("bat", 0.8), ("man", 0.75), ("woman", 0.7), ("goggles", 0.69), ("sunglasses", 0.65)])
        #expect(refined.map(\.tag) == ["baseball bat", "man", "sunglasses"])
    }

    @Test func ocrWarmupIsOptional() {
        #expect(!fileid_apple_ocr_ready() || AccurateOCRWarmup.shared.isReady)
    }

    @Test func aReadWaitingOnTheNeuralEnginesFirstCompileIsNotTimedOutLikeAClassifyCall() {
        #expect(accurateReadTimeoutSeconds >= 120, "the compile takes up to 70 s on a busy Mac")
        #expect(accurateReadTimeoutSeconds > visionPerformTimeoutSeconds)
        #expect(!runVisionWithTimeout(seconds: 0.05) { Thread.sleep(forTimeInterval: 0.3) })
        #expect(runVisionWithTimeout(seconds: 5) { Thread.sleep(forTimeInterval: 0.1) })
    }

    @Test func theAdapterReportsTheAccurateReaderPausedAfterThreeFailuresInARow() {
        let breaker = AccurateOCRBreaker.shared
        breaker.record(success: true)
        defer { breaker.record(success: true) }
        #expect(!fileid_apple_ocr_paused())
        for _ in 0..<AccurateOCRBreaker.failureLimit { breaker.record(success: false) }
        #expect(fileid_apple_ocr_paused())
        #expect(!fileid_apple_ocr_ready())
        breaker.record(success: true)
        #expect(!fileid_apple_ocr_paused())
    }
}
