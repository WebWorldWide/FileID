// FaceAlign similarity-fit math (the heart of 5-point alignment). The Vision
// landmark extraction + geometry-to-template-slot mapping are validated on a Mac
// by eye (FILEID_FACE_ALIGN=1, compare People clustering); these guard the
// least-squares 2D similarity solve that maps detected landmarks → the template.
import Testing
import Foundation
@testable import FileIDEngine

@Suite("FaceAlign — similarity fit")
struct FaceAlignTests {
    @Test("template→template fits the identity transform")
    func identity() throws {
        let t = FaceAlign.template
        let fit = try #require(FaceAlign.fitSimilarity(src: t, dst: t))
        #expect(abs(fit.0 - 1) < 1e-4)   // a ≈ 1
        #expect(abs(fit.1) < 1e-4)       // b ≈ 0 (no rotation)
        #expect(abs(fit.2) < 1e-3)       // tx ≈ 0
        #expect(abs(fit.3) < 1e-3)       // ty ≈ 0
    }

    @Test("recovers a known scale + translation")
    func scaleTranslate() throws {
        let t = FaceAlign.template
        // dst = 2·src + (10, 20): fit should report a=2, b=0, t=(10,20).
        let dst = t.map { (p: (Float, Float)) in (p.0 * 2 + 10, p.1 * 2 + 20) }
        let fit = try #require(FaceAlign.fitSimilarity(src: t, dst: dst))
        #expect(abs(fit.0 - 2) < 1e-3)
        #expect(abs(fit.1) < 1e-3)
        #expect(abs(fit.2 - 10) < 1e-2)
        #expect(abs(fit.3 - 20) < 1e-2)
    }

    @Test("recovers a 90° rotation (a≈0, |b|≈1)")
    func rotation() throws {
        let t = FaceAlign.template
        // Rotate src by +90°: (x,y) → (−y, x). The similarity [[a,−b],[b,a]] that
        // maps src→dst is then a=0, b=1 (since [[0,−1],[1,0]]·(x,y) = (−y,x)).
        let dst = t.map { (p: (Float, Float)) in (-p.1, p.0) }
        let fit = try #require(FaceAlign.fitSimilarity(src: t, dst: dst))
        #expect(abs(fit.0) < 1e-3)        // a ≈ 0
        #expect(abs(abs(fit.1) - 1) < 1e-3) // |b| ≈ 1
    }
}
