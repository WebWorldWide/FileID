import XCTest
import CoreGraphics
@testable import FileID

final class MediaProcessorMathTests: XCTestCase {

    // Build a small CGImage filled with a known gradient so the dHash output
    // is predictable. 64x64 RGBA, gradient ramps left→right.
    private func makeGradientImage(width: Int = 64, height: Int = 64) -> CGImage {
        let colorSpace = CGColorSpaceCreateDeviceRGB()
        let bytesPerRow = width * 4
        var pixels = [UInt8](repeating: 0, count: width * height * 4)
        for y in 0..<height {
            for x in 0..<width {
                let i = (y * width + x) * 4
                let v = UInt8(min(255, x * 4))
                pixels[i + 0] = v       // B
                pixels[i + 1] = v       // G
                pixels[i + 2] = v       // R
                pixels[i + 3] = 255     // A
            }
        }
        let ctx = CGContext(
            data: &pixels,
            width: width, height: height,
            bitsPerComponent: 8,
            bytesPerRow: bytesPerRow,
            space: colorSpace,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        return ctx.makeImage()!
    }

    func testDHashIsDeterministicForSameImage() {
        let image = makeGradientImage()
        let hash1 = MediaProcessor.computeDHashStatic(image)
        let hash2 = MediaProcessor.computeDHashStatic(image)
        XCTAssertEqual(hash1, hash2, "dHash must be deterministic — duplicate detection depends on it.")
    }

    func testDHashDiffersForDifferentImages() {
        let a = makeGradientImage(width: 64, height: 64)
        // A flat black image will have all-equal pixels — dHash bits are
        // p[i] > p[i+1], so all comparisons are equal-not-greater = 0.
        let cs = CGColorSpaceCreateDeviceRGB()
        var blackPixels = [UInt8](repeating: 0, count: 64 * 64 * 4)
        for i in stride(from: 3, to: blackPixels.count, by: 4) { blackPixels[i] = 255 }
        let blackCtx = CGContext(
            data: &blackPixels,
            width: 64, height: 64,
            bitsPerComponent: 8,
            bytesPerRow: 256,
            space: cs,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        let b = blackCtx.makeImage()!
        XCTAssertNotEqual(MediaProcessor.computeDHashStatic(a),
                          MediaProcessor.computeDHashStatic(b))
    }

    func testLightweightAestheticBoundedZeroToOne() {
        let image = makeGradientImage()
        for sizeMB in [0.0, 0.5, 1.0, 5.0, 50.0, 500.0] {
            let s = MediaProcessor.lightweightAestheticStatic(cgImage: image, fileSizeMB: sizeMB)
            XCTAssertGreaterThanOrEqual(s, 0.0)
            XCTAssertLessThanOrEqual(s, 1.0)
        }
    }

    func testLightweightAestheticIsMonotonicInSize() {
        let image = makeGradientImage()
        let small = MediaProcessor.lightweightAestheticStatic(cgImage: image, fileSizeMB: 0.1)
        let big   = MediaProcessor.lightweightAestheticStatic(cgImage: image, fileSizeMB: 10.0)
        XCTAssertLessThanOrEqual(small, big,
                                 "Larger files should not score lower at fixed resolution.")
    }
}
