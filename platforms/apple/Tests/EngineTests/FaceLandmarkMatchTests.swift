import Testing
import Foundation
import CoreGraphics
import CryptoKit
@testable import FileIDEngine

@Suite struct FaceLandmarkMatchTests {
    @Test func sharedAlignmentFixtures() throws {
        struct Fixtures: Decodable {
            struct Case: Decodable { let source: [[Float]]; let expected: [Float]? }
            let template: [[Float]]
            let cases: [Case]
        }
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let fixture = try JSONDecoder().decode(Fixtures.self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/face-alignment.json")))
        #expect(fixture.template.count == FaceAlign.template.count)
        for (actual, expected) in zip(FaceAlign.template, fixture.template) {
            #expect(actual.0 == expected[0] && actual.1 == expected[1])
        }
        for item in fixture.cases {
            let fit = FaceAlign.fitSimilarity(src: item.source.map { ($0[0], $0[1]) }, dst: FaceAlign.template)
            if let expected = item.expected {
                let actual = try #require(fit)
                for (a, e) in zip([actual.0, actual.1, actual.2, actual.3], expected) { #expect(abs(a - e) < 0.001) }
            } else { #expect(fit == nil) }
        }
    }

    private struct SharedFixture: Decodable {
        struct Aligned: Decodable { let name: String; let width: Int; let height: Int; let landmarks: [[Float]]; let fitBits: [String]?; let sha256: String? }
        struct Padded: Decodable { let bbox: String; let width: Int; let height: Int; let orientation: Int; let expected: [Int]? }
        let aligned: [Aligned]
        let paddedCrops: [Padded]
    }

    private func sharedFixture() throws -> SharedFixture {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        return try JSONDecoder().decode(SharedFixture.self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/face-alignment.json")))
    }

    private func pattern(_ width: Int, _ height: Int) -> [UInt8] {
        var out = [UInt8]()
        out.reserveCapacity(width * height * 4)
        for y in 0..<height {
            for x in 0..<width {
                out += [UInt8((x * 7 + y * 13) % 256), UInt8((x * x + 3 * y) % 256), UInt8(((x ^ y) * 5) % 256), 255]
            }
        }
        return out
    }

    /// The shared Rust engine's `face_align::align_112_rgba` checks the same digests.
    @Test func sharedAlignedCropsMatchBitForBit() throws {
        let fixture = try sharedFixture()
        #expect(fixture.aligned.count >= 4)
        for item in fixture.aligned {
            let landmarks = item.landmarks.map { ($0[0], $0[1]) }
            let fit = FaceAlign.fitSimilarity(src: landmarks, dst: FaceAlign.template)
            let bits = fit.map { [$0.0, $0.1, $0.2, $0.3].map { String(format: "%08x", $0.bitPattern) } }
            #expect(bits == item.fitBits, "\(item.name)")
            let source = FaceAlign.SourcePixels(rgba: pattern(item.width, item.height), width: item.width, height: item.height)
            let crop = FaceAlign.align112(source: source, landmarks: landmarks)
            let digest = crop.flatMap { image -> String? in
                guard let data = image.dataProvider?.data as Data? else { return nil }
                return SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
            }
            #expect(digest == item.sha256, "\(item.name)")
        }
    }

    @Test func sharedPaddedCropsMatchTheRustRects() throws {
        for item in try sharedFixture().paddedCrops {
            let space = CGColorSpaceCreateDeviceRGB()
            var pixels = pattern(item.width, item.height)
            let image = try #require(pixels.withUnsafeMutableBytes { buffer in
                CGContext(data: buffer.baseAddress, width: item.width, height: item.height, bitsPerComponent: 8, bytesPerRow: item.width * 4,
                          space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue)?.makeImage()
            })
            let crop = FaceClustering.cropFaceCGImage(cgImage: image, bboxString: item.bbox, sourceOrientation: item.orientation)
            guard let expected = item.expected else { #expect(crop == nil, "\(item.bbox)"); continue }
            let found = try #require(crop, "\(item.bbox)")
            #expect(found.width == expected[2] && found.height == expected[3], "\(item.bbox)")
            var first = [UInt8](repeating: 0, count: 4)
            first.withUnsafeMutableBytes { buffer in
                let context = CGContext(data: buffer.baseAddress, width: 1, height: 1, bitsPerComponent: 8, bytesPerRow: 4, space: space,
                                        bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue)
                context?.draw(found, in: CGRect(x: 0, y: 1 - found.height, width: found.width, height: found.height))
            }
            let origin = pattern(item.width, item.height)
            let index = (expected[1] * item.width + expected[0]) * 4
            #expect(Array(first[0..<3]) == Array(origin[index..<index + 3]), "\(item.bbox) origin")
        }
    }

    @Test func nearbyFaceCannotSupplyLandmarks() {
        let stored = CGRect(x: 0.4, y: 0.4, width: 0.02, height: 0.02)
        let neighbor = CGRect(x: 0.43, y: 0.4, width: 0.02, height: 0.02)
        #expect(FaceLandmarkMatch.index(stored: stored, candidates: [neighbor]) == nil)
        #expect(FaceLandmarkMatch.index(stored: stored, candidates: [neighbor, stored]) == 1)
    }

    @Test func displacedOverlappingFaceIsRejected() {
        let stored = CGRect(x: 0.4, y: 0.4, width: 0.1, height: 0.1)
        #expect(FaceLandmarkMatch.index(stored: stored, candidates: [CGRect(x: 0.45, y: 0.4, width: 0.1, height: 0.1)]) == nil)
        #expect(FaceLandmarkMatch.index(stored: stored, candidates: [CGRect(x: 0.401, y: 0.399, width: 0.1, height: 0.1)]) == 0)
    }

    @Test func ambiguousAndInvalidGeometryAbstains() {
        let stored = CGRect(x: 0.4, y: 0.4, width: 0.1, height: 0.1)
        #expect(FaceLandmarkMatch.index(stored: stored, candidates: [stored, stored]) == nil)
        #expect(FaceLandmarkMatch.index(stored: .zero, candidates: [stored]) == nil)
        #expect(FaceLandmarkMatch.index(stored: CGRect(x: CGFloat.nan, y: 0, width: 0.1, height: 0.1), candidates: [stored]) == nil)
        #expect(FaceLandmarkMatch.index(stored: stored, candidates: [CGRect(x: 0, y: 0, width: 5, height: 5)]) == nil)
    }

    @Test func batchedAlignmentPreservesPixelsAndRejectsInvalidPoints() throws {
        let space = CGColorSpaceCreateDeviceRGB()
        let context = try #require(CGContext(data: nil, width: 112, height: 112, bitsPerComponent: 8, bytesPerRow: 448, space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        context.setFillColor(CGColor(red: 0.2, green: 0.7, blue: 0.1, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: 112, height: 112))
        context.setFillColor(CGColor(red: 0.9, green: 0.1, blue: 0.8, alpha: 1))
        context.fill(CGRect(x: 20, y: 25, width: 18, height: 35))
        let image = try #require(context.makeImage())
        let pixels = try #require(FaceAlign.pixels(source: image))
        for offset: Float in [0, 3] {
            let landmarks = FaceAlign.template.map { ($0.0 + offset, $0.1) }
            let before = try #require(FaceAlign.align112(source: image, landmarks: landmarks))
            let after = try #require(FaceAlign.align112(source: pixels, landmarks: landmarks))
            #expect(before.dataProvider?.data as Data? == after.dataProvider?.data as Data?)
        }
        var invalid = FaceAlign.template
        invalid[0].0 = .nan
        #expect(FaceAlign.align112(source: pixels, landmarks: invalid) == nil)
        #expect(FaceAlign.fitSimilarity(src: FaceAlign.template, dst: invalid) == nil)
    }
}
