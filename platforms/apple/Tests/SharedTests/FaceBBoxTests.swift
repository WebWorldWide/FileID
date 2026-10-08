// FaceBBox cross-platform read-tolerance: macOS CSV (normalized, bottom-left)
// passthrough must be byte-identical (no within-platform change), and Windows
// JSON (pixels, top-left) must convert to normalized bottom-left.
import Testing
import Foundation
@testable import FileIDShared

@Suite struct FaceBBoxTests {
    @Test func explicitSourceGeometrySurvivesThumbnailResize() {
        let bbox = #"{"x":400,"y":200,"w":800,"h":400,"coordinateSpace":"pixel-top-left","sourceWidth":4000,"sourceHeight":2000}"#
        let full = FaceBBox.parseNormalized(bbox, imageWidth: 4000, imageHeight: 2000)
        let thumbnail = FaceBBox.parseNormalized(bbox, imageWidth: 1000, imageHeight: 500)
        #expect(full?.x == thumbnail?.x)
        #expect(full?.y == thumbnail?.y)
        #expect(thumbnail?.x == 0.1)
        #expect(thumbnail?.w == 0.2)
        #expect(FaceBBox.parseNormalized(#"{"x":1,"y":1,"w":2,"h":2,"sourceWidth":0,"sourceHeight":20}"#, imageWidth: 100, imageHeight: 100) == nil)
    }

    @Test("macOS CSV is parsed unchanged (dims ignored)")
    func csvPassthrough() throws {
        let b = try #require(FaceBBox.parseNormalized("0.1,0.2,0.3,0.4", imageWidth: 1000, imageHeight: 800))
        #expect(abs(b.x - 0.1) < 1e-9)
        #expect(abs(b.y - 0.2) < 1e-9)
        #expect(abs(b.w - 0.3) < 1e-9)
        #expect(abs(b.h - 0.4) < 1e-9)
        // Same regardless of dims (CSV is already normalized).
        let b2 = try #require(FaceBBox.parseNormalized("0.1,0.2,0.3,0.4", imageWidth: 4000, imageHeight: 3000))
        #expect(b.x == b2.x && b.y == b2.y && b.w == b2.w && b.h == b2.h)
    }

    @Test("Windows JSON pixels (top-left) → normalized bottom-left")
    func jsonPixelConversion() throws {
        // 100,200,300,400 px in a 1000×800 image: w=0.3, h=0.5, x=0.1,
        // yTop=0.25 → yBottom = 1 − 0.25 − 0.5 = 0.25.
        let json = #"{"x":100,"y":200,"w":300,"h":400,"roll":0.1,"yaw":-0.2,"pitch":0.05}"#
        let b = try #require(FaceBBox.parseNormalized(json, imageWidth: 1000, imageHeight: 800))
        #expect(abs(b.x - 0.1) < 1e-6)
        #expect(abs(b.w - 0.3) < 1e-6)
        #expect(abs(b.h - 0.5) < 1e-6)
        #expect(abs(b.y - 0.25) < 1e-6)
    }

    @Test("malformed / insufficient inputs return nil")
    func malformed() {
        #expect(FaceBBox.parseNormalized("", imageWidth: 100, imageHeight: 100) == nil)
        #expect(FaceBBox.parseNormalized("0.1,0.2,0.3", imageWidth: 100, imageHeight: 100) == nil) // <4
        #expect(FaceBBox.parseNormalized("0.1,,0.2,0.3,0.4", imageWidth: 100, imageHeight: 100) == nil)
        #expect(FaceBBox.parseNormalized("0.1,broken,0.2,0.3,0.4", imageWidth: 100, imageHeight: 100) == nil)
        #expect(FaceBBox.parseNormalized("nan,0.2,0.3,0.4", imageWidth: 100, imageHeight: 100) == nil)
        #expect(FaceBBox.parseNormalized("0.1,0.2,-0.3,0.4", imageWidth: 100, imageHeight: 100) == nil)
        #expect(FaceBBox.parseNormalized(#"{"x":1,"y":2,"w":0,"h":4}"#, imageWidth: 100, imageHeight: 100) == nil)
        #expect(FaceBBox.parseNormalized(#"{"x":1,"y":2,"h":4}"#, imageWidth: 100, imageHeight: 100) == nil) // missing w
        #expect(FaceBBox.parseNormalized(#"{"x":1,"y":2,"w":3,"h":4}"#, imageWidth: 0, imageHeight: 0) == nil) // bad dims
    }
    @Test func rawPixelBoxesFollowAllExifOrientations() throws {
        let box = #"{"x":100,"y":160,"w":300,"h":320,"coordinateSpace":"pixel-top-left","sourceWidth":1000,"sourceHeight":800}"#
        let expected: [(Double,Double,Double,Double)] = [
            (0.1,0.4,0.3,0.4),(0.6,0.4,0.3,0.4),(0.6,0.2,0.3,0.4),(0.1,0.2,0.3,0.4),
            (0.2,0.6,0.4,0.3),(0.4,0.6,0.4,0.3),(0.4,0.1,0.4,0.3),(0.2,0.1,0.4,0.3)
        ]
        for orientation in 1...8 {
            let actual = try #require(FaceBBox.parseNormalized(box, imageWidth: 800, imageHeight: 1000, sourceOrientation: orientation))
            let wanted = expected[orientation-1]
            #expect(abs(actual.x-wanted.0)<1e-9)
            #expect(abs(actual.y-wanted.1)<1e-9)
            #expect(abs(actual.w-wanted.2)<1e-9)
            #expect(abs(actual.h-wanted.3)<1e-9)
        }
        #expect(FaceBBox.parseNormalized(box, imageWidth: 800, imageHeight: 1000, sourceOrientation: 9) == nil)
        #expect(FaceBBox.parseNormalized(#"{"x":100,"y":160,"w":300,"h":320}"#, imageWidth: 800, imageHeight: 1000, sourceOrientation: 6) == nil)
        let native = try #require(FaceBBox.parseNormalized("0.1,0.2,0.3,0.4", imageWidth: 800, imageHeight: 1000, sourceOrientation: 6))
        #expect(native.x == 0.1 && native.y == 0.2 && native.w == 0.3 && native.h == 0.4)
    }

}
