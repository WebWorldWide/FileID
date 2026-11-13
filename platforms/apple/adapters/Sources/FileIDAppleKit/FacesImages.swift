// Stage 6: the photos the Rust engine reads faces from. Decode, Vision
// landmarks, the rendered RGBA buffer, the padded fallback crop and the crop
// JPEGs follow the Swift engine's FaceClustering.extractOneFile, FaceAlign and
// ArcFaceService line for line, so the Rust alignment (face_align.rs) and SFace
// see the same pixels and the faces stay in `macos-landmark-overlap-v3-orientation`.
// The decoded image stays behind a handle; Rust frees it with
// fileid_apple_faces_close.
import CoreGraphics
import FileIDImaging
import Foundation
import ImageIO
import Vision

final class FacesImages: @unchecked Sendable {
    static let shared = FacesImages()
    private let lock = NSLock()
    private var images: [UInt64: CGImage] = [:]
    private var next: UInt64 = 1

    func insert(_ image: CGImage) -> UInt64 {
        lock.withLock {
            let id = next
            next += 1
            images[id] = image
            return id
        }
    }

    func image(_ id: UInt64) -> CGImage? {
        lock.withLock { images[id] }
    }

    func release(_ id: UInt64) {
        lock.withLock { images[id] = nil }
    }

    func releaseAll() {
        lock.withLock { images.removeAll() }
    }

    var count: Int {
        lock.withLock { images.count }
    }
}

enum FacesExtraction {
    /// Exact values cross the interface as IEEE-754 bit patterns: `bounds`
    /// (x, y, w, h as Double bits), `points` (x0, y0 … x4, y4 as Float bits).
    struct Landmarks: Encodable {
        let bounds: [UInt64]
        let points: [UInt32]
    }

    struct Opened: Encodable {
        let width: Int
        let height: Int
        let orientation: Int
        let faces: [Landmarks]
    }

    /// `FaceClustering.loadCGImage`: files under 256 B are refused, the EXIF
    /// orientation must be 1–8, the long edge is at most `maxPixelSize`.
    static func load(path: String, maxPixelSize: Int) -> (image: CGImage, orientation: Int)? {
        let url = URL(fileURLWithPath: path)
        if let attrs = try? FileManager.default.attributesOfItem(atPath: url.path),
           let size = attrs[.size] as? Int, size < 256 {
            return nil
        }
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        let properties = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any]
        let orientation = (properties?[kCGImagePropertyOrientation] as? NSNumber)?.intValue ?? 1
        guard (1...8).contains(orientation), let image = decodeBoundedImage(src, path: url.path, maxPixelSize: maxPixelSize) else { return nil }
        return (image, orientation)
    }

    /// `FaceClustering.detectFaceLandmarks`: one landmark request on the whole
    /// image; per face its normalized bottom-left box and five points in
    /// top-left source pixels [hi-x eye, lo-x eye, nose, hi-x mouth corner,
    /// lo-x mouth corner], assigned by image x.
    static func detectLandmarks(in cg: CGImage) -> [Landmarks] {
        let req = VNDetectFaceLandmarksRequest()
        let handler = VNImageRequestHandler(cgImage: cg, options: [:])
        do {
            try handler.perform([req])
        } catch {
            return []
        }
        let size = CGSize(width: cg.width, height: cg.height)
        let imgH = Float(cg.height)
        func centroid(_ pts: [(Float, Float)]) -> (Float, Float)? {
            guard !pts.isEmpty else { return nil }
            var sx: Float = 0, sy: Float = 0
            for p in pts { sx += p.0; sy += p.1 }
            return (sx / Float(pts.count), sy / Float(pts.count))
        }
        var result: [Landmarks] = []
        for obs in (req.results ?? []) {
            guard let lm = obs.landmarks,
                  let le = lm.leftEye, let re = lm.rightEye,
                  let nose = lm.nose, let lips = lm.outerLips else { continue }
            func tl(_ region: VNFaceLandmarkRegion2D) -> [(Float, Float)] {
                region.pointsInImage(imageSize: size).map { (Float($0.x), imgH - Float($0.y)) }
            }
            let lipsP = tl(lips)
            guard let eyeA = centroid(tl(le)),
                  let eyeB = centroid(tl(re)),
                  let noseC = centroid(tl(nose)),
                  let mouthHi = lipsP.max(by: { $0.0 < $1.0 }),
                  let mouthLo = lipsP.min(by: { $0.0 < $1.0 }) else { continue }
            let hiEye = eyeA.0 >= eyeB.0 ? eyeA : eyeB
            let loEye = eyeA.0 >= eyeB.0 ? eyeB : eyeA
            let five = [hiEye, loEye, noseC, mouthHi, mouthLo].flatMap { [$0.0.bitPattern, $0.1.bitPattern] }
            let r = obs.boundingBox
            let bounds = [r.origin.x, r.origin.y, r.width, r.height].map { Double($0).bitPattern }
            result.append(Landmarks(bounds: bounds, points: five))
        }
        return result
    }

    /// `FaceAlign.renderRGBA8`: tightly packed RGBX in device RGB, top-left origin.
    static func renderRGBA8(_ src: CGImage) -> [UInt8]? {
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
        return ok ? rgba : nil
    }

    /// `ArcFaceService.makeNCHWTensor`'s resize: the crop drawn into a 112×112
    /// device-RGB RGBX context with high interpolation.
    static func tensorPixels(_ src: CGImage, side: Int = 112) -> [UInt8]? {
        var rgba = [UInt8](repeating: 0, count: side * side * 4)
        let cs = CGColorSpaceCreateDeviceRGB()
        let drawn: Bool = rgba.withUnsafeMutableBytes { ptr in
            guard let ctx = CGContext(data: ptr.baseAddress, width: side, height: side,
                                      bitsPerComponent: 8, bytesPerRow: side * 4, space: cs,
                                      bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue)
            else { return false }
            ctx.interpolationQuality = .high
            ctx.draw(src, in: CGRect(x: 0, y: 0, width: side, height: side))
            return true
        }
        return drawn ? rgba : nil
    }

    /// `FaceAlign.makeCGImage`: an RGBX device-RGB image over the bytes.
    static func makeCGImage(_ rgba: [UInt8], width: Int, height: Int) -> CGImage? {
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

    /// `FaceClustering.faceCropJPEG`: quality 0.85.
    static func jpeg(_ crop: CGImage) -> Data? {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, "public.jpeg" as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(destination, crop, [kCGImageDestinationLossyCompressionQuality: 0.85] as CFDictionary)
        return CGImageDestinationFinalize(destination) ? data as Data : nil
    }
}

func facesCopyOut(_ bytes: [UInt8], _ outPointer: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?, _ outLength: UnsafeMutablePointer<Int>?) -> Bool {
    guard !bytes.isEmpty, let buffer = malloc(bytes.count)?.assumingMemoryBound(to: UInt8.self) else { return false }
    bytes.withUnsafeBufferPointer { buffer.update(from: $0.baseAddress!, count: bytes.count) }
    outPointer?.pointee = buffer
    outLength?.pointee = bytes.count
    return true
}

func facesJSON<T: Encodable>(_ value: T) -> UnsafeMutablePointer<CChar>? {
    guard let data = try? JSONEncoder().encode(value), let text = String(data: data, encoding: .utf8) else { return nil }
    return strdup(text)
}
