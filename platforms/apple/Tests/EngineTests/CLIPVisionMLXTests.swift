import Testing
import Foundation
import CoreGraphics
@testable import FileIDEngine

@Suite("CLIP on the GPU", .serialized)
struct CLIPVisionMLXTests {
    private static var installed: Bool {
        ProcessInfo.processInfo.environment["FILEID_GPU_TESTS"] == "1"
            && FileManager.default.fileExists(atPath: MobileCLIPService.defaultImageModelURL.path)
    }

    private static func card(_ seed: Int) -> CGImage {
        let ctx = CGContext(data: nil, width: 300, height: 220, bitsPerComponent: 8, bytesPerRow: 0,
                            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        for i in 0..<12 {
            let v = Double((seed * 37 + i * 53) % 255) / 255
            ctx.setFillColor(red: v, green: 1 - v, blue: Double((i * 29 + seed) % 7) / 7, alpha: 1)
            ctx.fill(CGRect(x: (i * 47 + seed * 13) % 260, y: (i * 31 + seed * 7) % 180, width: 40 + i * 3, height: 30 + seed % 20))
        }
        return ctx.makeImage()!
    }

    @Test(.enabled(if: installed)) func gpuEncoderMatchesOnnxRuntime() throws {
        let service = MobileCLIPService.shared
        #expect(service.loadGPUEncoder() != nil)
        var gpuSeconds = 0.0, cpuSeconds = 0.0
        for seed in 0..<6 {
            let image = Self.card(seed)
            var t = CFAbsoluteTimeGetCurrent()
            let reference = try #require(service.embedImageCPU(image))
            cpuSeconds += CFAbsoluteTimeGetCurrent() - t
            t = CFAbsoluteTimeGetCurrent()
            let gpu = try #require(service.embedImage(image))
            gpuSeconds += CFAbsoluteTimeGetCurrent() - t
            let cosine = zip(reference, gpu).reduce(Float(0)) { $0 + $1.0 * $1.1 }
            #expect(cosine > 0.999, "seed \(seed) cosine \(cosine)")
        }
        print("clip-parity cpu \(Int(cpuSeconds / 6 * 1000)) ms gpu \(Int(gpuSeconds / 6 * 1000)) ms per image")
    }

    @Test(.enabled(if: installed)) func gpuThroughput() throws {
        let service = MobileCLIPService.shared
        let encoder = try #require(service.loadGPUEncoder())
        let image = Self.card(3)
        _ = service.embedImage(image)
        var t = CFAbsoluteTimeGetCurrent()
        for _ in 0..<20 { _ = service.embedImage(image) }
        let single = (CFAbsoluteTimeGetCurrent() - t) / 20
        let pixels = [Float](repeating: 0.1, count: 3 * 224 * 224 * 16)
        _ = encoder.embed(pixels: pixels, count: 16)
        t = CFAbsoluteTimeGetCurrent()
        for _ in 0..<5 { _ = encoder.embed(pixels: pixels, count: 16) }
        let batched = (CFAbsoluteTimeGetCurrent() - t) / 80
        t = CFAbsoluteTimeGetCurrent()
        for _ in 0..<5 { _ = service.embedImageCPU(image) }
        let cpu = (CFAbsoluteTimeGetCurrent() - t) / 5
        print(String(format: "clip-speed cpu %.1f ms, gpu single %.1f ms, gpu batch16 %.1f ms per image", cpu * 1000, single * 1000, batched * 1000))
    }
}
