// Stage 6: Best Takes' face judgement for the Rust engine
// (VisionTakeFaceAnalyzer and TakeMediaAnalysis.signals in the Swift engine).
// The adapter decodes and draws the photo, and runs Vision's landmarks,
// capture quality and human rectangles; the Rust engine keeps tiling, merging,
// Face Mesh and scoring (take_scoring.rs, catalog_take_auto.rs).
import CoreGraphics
import Foundation
import ImageIO
import Vision

enum FacesTake {
    enum Decode: UInt32 {
        /// `decodeBoundedImage` then `TakeMediaAnalysis.fit` (sRGB, scaled to fit).
        case analysis = 0
        /// `decodeScanImage` drawn unscaled into sRGB (`FaceMesh.Pixels`).
        case detail = 1
        /// `decodeBoundedImage` drawn unscaled into sRGB (the tile source).
        case wide = 2
    }

    /// Packed sRGB RGB8 of the photo as `kind` asks, and the source's long side.
    static func decode(path: String, kind: Decode, longSide: Int) -> (rgb: [UInt8], width: Int, height: Int, sourceLongSide: Int)? {
        let url = URL(fileURLWithPath: path)
        guard let source = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary) else { return nil }
        let image: CGImage?
        switch kind {
        case .analysis, .wide: image = decodeBoundedImage(source, path: path, maxPixelSize: longSide)
        case .detail: image = decodeScanImage(source, path: path, maxPixelSize: longSide)
        }
        guard let image else { return nil }
        let scale = kind == .analysis ? min(1, Double(longSide) / Double(max(image.width, image.height))) : 1
        let w = max(1, Int((Double(image.width) * scale).rounded())), h = max(1, Int((Double(image.height) * scale).rounded()))
        guard let rgbx = draw(image, width: w, height: h, interpolate: kind == .analysis) else { return nil }
        var rgb = [UInt8](repeating: 0, count: w * h * 3)
        for i in 0..<(w * h) {
            rgb[i * 3] = rgbx[i * 4]
            rgb[i * 3 + 1] = rgbx[i * 4 + 1]
            rgb[i * 3 + 2] = rgbx[i * 4 + 2]
        }
        return (rgb, w, h, sourceLongSide(source))
    }

    static func sourceLongSide(_ source: CGImageSource) -> Int {
        guard let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] else { return 0 }
        return max(properties[kCGImagePropertyPixelWidth] as? Int ?? 0, properties[kCGImagePropertyPixelHeight] as? Int ?? 0)
    }

    /// `TakeMediaAnalysis.fit` (high interpolation) or `FaceMesh.Pixels` (default).
    private static func draw(_ image: CGImage, width: Int, height: Int, interpolate: Bool) -> [UInt8]? {
        var buffer = [UInt8](repeating: 0, count: width * height * 4)
        let drawn = buffer.withUnsafeMutableBytes { raw -> Bool in
            guard let context = CGContext(data: raw.baseAddress, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                                          space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                          bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue)
            else { return false }
            if interpolate { context.interpolationQuality = .high }
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        return drawn ? buffer : nil
    }

    /// The sRGB image Vision reads, over packed RGB8, with the bitmap layout of
    /// `TakeMediaAnalysis.fit`'s context: Vision's boxes and landmarks move by
    /// up to 0.006 of the frame when the same bytes are tagged 32-bit big-endian.
    static func image(rgb: UnsafeBufferPointer<UInt8>, width: Int, height: Int) -> CGImage? {
        guard width > 0, height > 0, rgb.count == width * height * 3 else { return nil }
        var rgbx = [UInt8](repeating: 255, count: width * height * 4)
        for i in 0..<(width * height) {
            rgbx[i * 4] = rgb[i * 3]
            rgbx[i * 4 + 1] = rgb[i * 3 + 1]
            rgbx[i * 4 + 2] = rgb[i * 3 + 2]
        }
        return rgbx.withUnsafeMutableBytes { raw -> CGImage? in
            CGContext(data: raw.baseAddress, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                      space: CGColorSpace(name: CGColorSpace.sRGB)!,
                      bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)?.makeImage()
        }
    }

    /// A mesh input in top-left pixels of a `width` × `height` image: Vision's
    /// box and the centres of its eye and nose regions (`meshDetection`).
    /// Floats cross as bit patterns: x1, y1, x2, y2, eyeA, eyeB, nose; `boxOnly`
    /// when the face has no landmarks.
    struct Mesh: Encodable {
        let bits: [UInt32]
        let boxOnly: Bool
    }

    struct Face: Encodable {
        /// minX, 1 − maxY, width, height (Double bits).
        let box: [UInt64]
        /// Shorter side in pixels of the analysed image (Double bits).
        let minSide: UInt64
        let quality: UInt64?
        let eyes: UInt64?
        let mesh: Mesh
    }

    struct Person: Encodable {
        let box: [UInt64]
    }

    struct Analysis: Encodable {
        let complete: Bool
        let faces: [Face]
        let people: [Person]?
    }

    static let closedRatio = 0.12
    static let openRatio = 0.26

    /// `VisionTakeFaceAnalyzer.analyze` (with `people`) or `.faces(in:)`, and
    /// the kept face observations for Face Mesh inputs on a larger decode.
    static func analyze(_ image: CGImage, people withPeople: Bool) -> (Analysis, [VNFaceObservation]) {
        let handler = VNImageRequestHandler(cgImage: image, options: [:])
        let landmarks = VNDetectFaceLandmarksRequest()
        let quality = VNDetectFaceCaptureQualityRequest()
        let humans = VNDetectHumanRectanglesRequest()
        humans.upperBodyOnly = false
        let box = FacesVisionBox((handler, landmarks, quality, humans))
        let finished = facesRunVisionWithTimeout {
            let (handler, landmarks, quality, humans) = box.value
            do {
                try handler.perform(withPeople ? [landmarks, humans] : [landmarks])
                if let found = landmarks.results, !found.isEmpty {
                    quality.inputFaceObservations = found
                    try handler.perform([quality])
                }
            } catch {}
        }
        guard finished else { return (Analysis(complete: false, faces: [], people: nil), []) }
        let people = withPeople ? (humans.results ?? []).prefix(16).map { h in
            Person(box: [h.boundingBox.minX, 1 - h.boundingBox.maxY, h.boundingBox.width, h.boundingBox.height].map { Double($0).bitPattern })
        } : nil
        let found = landmarks.results ?? []
        let qualities = quality.results ?? []
        let kept = Array(found.prefix(16))
        let w = Double(image.width), h = Double(image.height)
        let faces = kept.enumerated().map { index, face -> Face in
            let r = face.boundingBox
            let q = index < qualities.count ? qualities[index].faceCaptureQuality.map(Double.init) : nil
            return Face(box: [r.minX, 1 - r.maxY, r.width, r.height].map { Double($0).bitPattern },
                        minSide: min(r.width * w, r.height * h).bitPattern,
                        quality: q?.bitPattern, eyes: eyesOpen(face)?.bitPattern,
                        mesh: mesh(face, width: Float(image.width), height: Float(image.height)))
        }
        return (Analysis(complete: true, faces: faces, people: people), kept)
    }

    static func mesh(_ face: VNFaceObservation, width: Float, height: Float) -> Mesh {
        let r = face.boundingBox
        let x1 = Float(r.minX) * width, x2 = Float(r.maxX) * width
        let y1 = (1 - Float(r.maxY)) * height, y2 = (1 - Float(r.minY)) * height
        let size = CGSize(width: CGFloat(width), height: CGFloat(height))
        func centre(_ region: VNFaceLandmarkRegion2D?) -> (Float, Float)? {
            guard let points = region?.pointsInImage(imageSize: size), !points.isEmpty else { return nil }
            let n = CGFloat(points.count)
            let x = points.reduce(0) { $0 + $1.x } / n, y = points.reduce(0) { $0 + $1.y } / n
            return (Float(x), height - Float(y))
        }
        guard let a = centre(face.landmarks?.leftEye), let b = centre(face.landmarks?.rightEye),
              let nose = centre(face.landmarks?.nose) else {
            let w = x2 - x1, h = y2 - y1
            let guess: [Float] = [x1, y1, x2, y2, x1 + 0.3 * w, y1 + 0.4 * h, x1 + 0.7 * w, y1 + 0.4 * h, x1 + 0.5 * w, y1 + 0.6 * h]
            return Mesh(bits: guess.map(\.bitPattern), boxOnly: true)
        }
        return Mesh(bits: [x1, y1, x2, y2, a.0, a.1, b.0, b.1, nose.0, nose.1].map(\.bitPattern), boxOnly: false)
    }

    /// `VisionTakeFaceAnalyzer.eyesOpen`: eye contour height over width.
    static func eyesOpen(_ face: VNFaceObservation) -> Double? {
        let ratios = [face.landmarks?.leftEye, face.landmarks?.rightEye].compactMap { region -> Double? in
            guard let points = region?.normalizedPoints, points.count >= 4 else { return nil }
            let xs = points.map { Double($0.x) }, ys = points.map { Double($0.y) }
            let width = xs.max()! - xs.min()!
            guard width > 0 else { return nil }
            return (ys.max()! - ys.min()!) / width
        }
        guard !ratios.isEmpty else { return nil }
        let ratio = ratios.reduce(0, +) / Double(ratios.count)
        return min(1, max(0, (ratio - closedRatio) / (openRatio - closedRatio)))
    }
}

/// Face observations kept for `fileid_apple_take_mesh` until released.
final class FacesObservations: @unchecked Sendable {
    static let shared = FacesObservations()
    private let lock = NSLock()
    private var kept: [UInt64: [VNFaceObservation]] = [:]
    private var next: UInt64 = 1

    func insert(_ faces: [VNFaceObservation]) -> UInt64 {
        lock.withLock {
            let id = next
            next += 1
            kept[id] = faces
            return id
        }
    }

    func faces(_ id: UInt64) -> [VNFaceObservation]? {
        lock.withLock { kept[id] }
    }

    func release(_ id: UInt64) {
        lock.withLock { kept[id] = nil }
    }

    var count: Int {
        lock.withLock { kept.count }
    }
}

/// Ferries non-Sendable Vision objects to the timeout queue; one thread
/// touches them at a time (`_VisionUncheckedBox` in the Swift engine).
final class FacesVisionBox<T>: @unchecked Sendable {
    let value: T
    init(_ v: T) { self.value = v }
}

/// `runVisionWithTimeout`: 10 s, after which the call is abandoned and the
/// result treated as not run.
func facesRunVisionWithTimeout(_ body: @escaping () -> Void) -> Bool {
    let sem = DispatchSemaphore(value: 0)
    let box = FacesVisionBox(body)
    DispatchQueue.global(qos: .userInitiated).async {
        autoreleasepool { box.value() }
        sem.signal()
    }
    return sem.wait(timeout: .now() + 10) == .success
}
