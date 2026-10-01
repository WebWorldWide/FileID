import Testing
import Foundation
import CoreGraphics
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
