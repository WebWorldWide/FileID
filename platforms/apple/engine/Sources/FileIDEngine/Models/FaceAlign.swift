// 5-point similarity-transform face alignment to the canonical 112×112 ArcFace
// reference template (used by SFace). Least-squares 2D similarity (scale +
// rotation + translation, no shear) from the detected 5 landmarks to the
// template, then bilinear-sample the source into a 112×112 RGB crop.
//
// Faithful Swift port of the Windows engine's `models/face_align.rs` so macOS
// and Windows produce comparable SFace embeddings (true cross-platform parity
// is only approximate — the two platforms use different detectors, YuNet vs
// Apple Vision, so the landmark positions differ slightly).
//
// Landmarks MUST be in SOURCE-IMAGE PIXEL coordinates (top-left origin) and in
// FileID order: [left_eye, right_eye, nose, mouth_left, mouth_right]. Apple
// Vision reports landmarks normalized to the face bounding box with a
// BOTTOM-LEFT origin, so the caller (the face-detection pass) must convert to
// absolute top-left pixel coordinates before calling `align112`.

import CoreGraphics
import Foundation

enum FaceAlign {
    static let out = 112

    /// Template in FileID landmark order [left_eye, right_eye, nose,
    /// mouth_left, mouth_right] — the standard ArcFace 5-point template,
    /// identical to the Windows engine's `face_align::TEMPLATE`.
    static let template: [(Float, Float)] = [
        (73.5318, 51.5014), // left_eye
        (38.2946, 51.6963), // right_eye
        (56.0252, 71.7366), // nose
        (70.7299, 92.2041), // mouth_left
        (41.5493, 92.3655), // mouth_right
    ]

    /// Align a face to a 112×112 RGBA8 CGImage from its 5 landmarks (FileID
    /// order, source-image pixel coords, top-left origin). Returns nil on a
    /// degenerate fit or if the source can't be rendered. The result feeds
    /// `ArcFaceService.embed(_:)` exactly like the old bbox crop did.
    static func align112(source: CGImage, landmarks: [(Float, Float)]) -> CGImage? {
        guard landmarks.count == 5 else { return nil }
        guard let (rgba, w, h) = renderRGBA8(source) else { return nil }
        guard let (a, b, tx, ty) = fitSimilarity(src: landmarks, dst: template) else { return nil }
        let det = a * a + b * b
        if abs(det) < 1e-9 { return nil }

        var outRGBA = [UInt8](repeating: 255, count: out * out * 4) // alpha pre-filled
        for oy in 0..<out {
            for ox in 0..<out {
                // Inverse map (dst → src): src = L⁻¹·(dst − t), L = [[a,−b],[b,a]].
                let dx = Float(ox) - tx
                let dy = Float(oy) - ty
                let sx = (a * dx + b * dy) / det
                let sy = (-b * dx + a * dy) / det
                let px = bilinear(rgba, w, h, sx, sy)
                let o = (oy * out + ox) * 4
                outRGBA[o] = px.0
                outRGBA[o + 1] = px.1
                outRGBA[o + 2] = px.2
                // outRGBA[o + 3] stays 255
            }
        }
        return makeCGImage(outRGBA, width: out, height: out)
    }

    // MARK: - Similarity fit (4×4 normal equations)

    /// Least-squares 2D similarity fit: solve (a, b, tx, ty) minimizing
    /// Σ‖[[a,−b],[b,a]]·src_i + [tx,ty] − dst_i‖² via the 4×4 normal equations.
    static func fitSimilarity(src: [(Float, Float)], dst: [(Float, Float)]) -> (Float, Float, Float, Float)? {
        guard src.count == dst.count, !src.isEmpty else { return nil }
        let n = Double(src.count)
        var sxx = 0.0, sx = 0.0, sy = 0.0
        var ba = 0.0, bb = 0.0, bxx = 0.0, byy = 0.0
        for k in 0..<src.count {
            let x = Double(src[k].0), y = Double(src[k].1)
            let cx = Double(dst[k].0), cy = Double(dst[k].1)
            sxx += x * x + y * y
            sx += x
            sy += y
            ba += x * cx + y * cy   // Σ(x·X + y·Y)
            bb += x * cy - y * cx   // Σ(x·Y − y·X)
            bxx += cx               // ΣX
            byy += cy               // ΣY
        }
        let m: [[Double]] = [
            [sxx, 0.0, sx, sy],
            [0.0, sxx, -sy, sx],
            [sx, -sy, n, 0.0],
            [sy, sx, 0.0, n],
        ]
        guard let p = solve4(m, [ba, bb, bxx, byy]) else { return nil }
        return (Float(p[0]), Float(p[1]), Float(p[2]), Float(p[3]))
    }

    /// 4×4 linear solve via Gaussian elimination with partial pivoting.
    static func solve4(_ matrix: [[Double]], _ rhs: [Double]) -> [Double]? {
        var m = matrix
        var r = rhs
        for col in 0..<4 {
            var piv = col
            for row in (col + 1)..<4 where abs(m[row][col]) > abs(m[piv][col]) {
                piv = row
            }
            if abs(m[piv][col]) < 1e-12 { return nil }
            m.swapAt(col, piv)
            r.swapAt(col, piv)
            for row in (col + 1)..<4 {
                let f = m[row][col] / m[col][col]
                for k in col..<4 { m[row][k] -= f * m[col][k] }
                r[row] -= f * r[col]
            }
        }
        var x = [Double](repeating: 0, count: 4)
        for col in stride(from: 3, through: 0, by: -1) {
            var s = r[col]
            for k in (col + 1)..<4 { s -= m[col][k] * x[k] }
            x[col] = s / m[col][col]
        }
        return x
    }

    // MARK: - Sampling + buffers

    /// Bilinear RGB sample with edge clamping from a tightly-packed RGBA8
    /// buffer (stride 4; alpha ignored).
    private static func bilinear(_ rgba: [UInt8], _ width: Int, _ height: Int, _ x: Float, _ y: Float) -> (UInt8, UInt8, UInt8) {
        let x0 = Int(floor(x))
        let y0 = Int(floor(y))
        let fx = x - Float(x0)
        let fy = y - Float(y0)
        func sample(_ xi: Int, _ yi: Int) -> (Float, Float, Float) {
            let xc = min(max(xi, 0), width - 1)
            let yc = min(max(yi, 0), height - 1)
            let o = (yc * width + xc) * 4
            return (Float(rgba[o]), Float(rgba[o + 1]), Float(rgba[o + 2]))
        }
        let p00 = sample(x0, y0)
        let p10 = sample(x0 + 1, y0)
        let p01 = sample(x0, y0 + 1)
        let p11 = sample(x0 + 1, y0 + 1)
        func lerp(_ c: (Float, Float, Float) -> Float) -> UInt8 {
            let top = c(p00) * (1 - fx) + c(p10) * fx
            let bot = c(p01) * (1 - fx) + c(p11) * fx
            return UInt8(min(max((top * (1 - fy) + bot * fy).rounded(), 0), 255))
        }
        return (lerp { $0.0 }, lerp { $0.1 }, lerp { $0.2 })
    }

    /// Render a CGImage into a tightly-packed RGBA8 buffer (RGBX, top-left
    /// origin) at its native size. Mirrors `ArcFaceService.makeNCHWTensor`'s
    /// channel layout so the bytes line up.
    private static func renderRGBA8(_ src: CGImage) -> ([UInt8], Int, Int)? {
        let w = src.width, h = src.height
        guard w > 0, h > 0 else { return nil }
        var rgba = [UInt8](repeating: 0, count: w * h * 4)
        let cs = CGColorSpaceCreateDeviceRGB()
        let info = CGImageAlphaInfo.noneSkipLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue
        let ok: Bool = rgba.withUnsafeMutableBytes { ptr in
            guard let ctx = CGContext(data: ptr.baseAddress, width: w, height: h,
                                      bitsPerComponent: 8, bytesPerRow: w * 4,
                                      space: cs, bitmapInfo: info) else { return false }
            ctx.interpolationQuality = .high
            ctx.draw(src, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        return ok ? (rgba, w, h) : nil
    }

    private static func makeCGImage(_ rgba: [UInt8], width: Int, height: Int) -> CGImage? {
        var data = rgba
        let cs = CGColorSpaceCreateDeviceRGB()
        let info = CGImageAlphaInfo.noneSkipLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue
        return data.withUnsafeMutableBytes { ptr -> CGImage? in
            guard let ctx = CGContext(data: ptr.baseAddress, width: width, height: height,
                                      bitsPerComponent: 8, bytesPerRow: width * 4,
                                      space: cs, bitmapInfo: info) else { return nil }
            return ctx.makeImage()
        }
    }
}
