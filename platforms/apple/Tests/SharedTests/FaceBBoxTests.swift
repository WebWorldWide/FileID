// FaceBBox cross-platform read-tolerance: macOS CSV (normalized, bottom-left)
// passthrough must be byte-identical (no within-platform change), and Windows
// JSON (pixels, top-left) must convert to normalized bottom-left.
import Testing
import Foundation
@testable import FileIDShared

@Suite struct FaceBBoxTests {
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
        #expect(FaceBBox.parseNormalized(#"{"x":1,"y":2,"h":4}"#, imageWidth: 100, imageHeight: 100) == nil) // missing w
        #expect(FaceBBox.parseNormalized(#"{"x":1,"y":2,"w":3,"h":4}"#, imageWidth: 0, imageHeight: 0) == nil) // bad dims
    }
}
