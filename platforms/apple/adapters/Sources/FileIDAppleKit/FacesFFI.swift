// Stage 6 C entry points (faces). Declared in adapters/include/fileid_apple.h
// and platforms/windows/src/engine/src/apple/ffi.rs inside the
// "Stage 6: faces" blocks; the safe Rust side is src/apple/faces.rs.
import CoreGraphics
import Foundation

@_cdecl("fileid_apple_faces_open")
public func fileid_apple_faces_open(
    _ path: UnsafePointer<CChar>?,
    _ maxPixelSize: UInt32,
    _ landmarks: Bool,
    _ outJSON: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> UInt64 {
    outJSON?.pointee = nil
    guard let path, maxPixelSize > 0 else { return 0 }
    let opened: (UInt64, UnsafeMutablePointer<CChar>?)? = autoreleasepool {
        guard let loaded = FacesExtraction.load(path: String(cString: path), maxPixelSize: Int(maxPixelSize)) else { return nil }
        let faces = landmarks ? FacesExtraction.detectLandmarks(in: loaded.image) : []
        let summary = FacesExtraction.Opened(width: loaded.image.width, height: loaded.image.height,
                                             orientation: loaded.orientation, faces: faces)
        guard let json = facesJSON(summary) else { return nil }
        return (FacesImages.shared.insert(loaded.image), json)
    }
    guard let opened else { return 0 }
    outJSON?.pointee = opened.1
    return opened.0
}

@_cdecl("fileid_apple_faces_pixels")
public func fileid_apple_faces_pixels(
    _ image: UInt64,
    _ outRGBA: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outLength: UnsafeMutablePointer<Int>?,
    _ outWidth: UnsafeMutablePointer<UInt32>?,
    _ outHeight: UnsafeMutablePointer<UInt32>?
) -> Bool {
    outRGBA?.pointee = nil
    outLength?.pointee = 0
    guard let cg = FacesImages.shared.image(image) else { return false }
    let rgba: [UInt8]? = autoreleasepool { FacesExtraction.renderRGBA8(cg) }
    guard let rgba, facesCopyOut(rgba, outRGBA, outLength) else { return false }
    outWidth?.pointee = UInt32(cg.width)
    outHeight?.pointee = UInt32(cg.height)
    return true
}

@_cdecl("fileid_apple_faces_crop")
public func fileid_apple_faces_crop(
    _ image: UInt64,
    _ x: Int64, _ y: Int64, _ width: Int64, _ height: Int64,
    _ outRGBA112: UnsafeMutablePointer<UInt8>?,
    _ outJPEG: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outJPEGLength: UnsafeMutablePointer<Int>?
) -> Bool {
    outJPEG?.pointee = nil
    outJPEGLength?.pointee = 0
    guard let outRGBA112, width >= 8, height >= 8, let cg = FacesImages.shared.image(image) else { return false }
    return autoreleasepool {
        guard let crop = cg.cropping(to: CGRect(x: CGFloat(x), y: CGFloat(y), width: CGFloat(width), height: CGFloat(height))),
              let pixels = FacesExtraction.tensorPixels(crop) else { return false }
        pixels.withUnsafeBufferPointer { outRGBA112.update(from: $0.baseAddress!, count: pixels.count) }
        if let jpeg = FacesExtraction.jpeg(crop) { _ = facesCopyOut([UInt8](jpeg), outJPEG, outJPEGLength) }
        return true
    }
}

@_cdecl("fileid_apple_faces_jpeg")
public func fileid_apple_faces_jpeg(
    _ rgba: UnsafePointer<UInt8>?,
    _ width: UInt32,
    _ height: UInt32,
    _ outLength: UnsafeMutablePointer<Int>?
) -> UnsafeMutablePointer<UInt8>? {
    outLength?.pointee = 0
    guard let rgba, width > 0, height > 0 else { return nil }
    let count = Int(width) * Int(height) * 4
    return autoreleasepool {
        let bytes = [UInt8](UnsafeBufferPointer(start: rgba, count: count))
        guard let image = FacesExtraction.makeCGImage(bytes, width: Int(width), height: Int(height)),
              let jpeg = FacesExtraction.jpeg(image) else { return nil }
        var out: UnsafeMutablePointer<UInt8>?
        guard facesCopyOut([UInt8](jpeg), &out, outLength) else { return nil }
        return out
    }
}

@_cdecl("fileid_apple_faces_close")
public func fileid_apple_faces_close(_ image: UInt64) {
    FacesImages.shared.release(image)
}

@_cdecl("fileid_apple_take_decode")
public func fileid_apple_take_decode(
    _ path: UnsafePointer<CChar>?,
    _ kind: UInt32,
    _ longSide: UInt32,
    _ outRGB: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outLength: UnsafeMutablePointer<Int>?,
    _ outWidth: UnsafeMutablePointer<UInt32>?,
    _ outHeight: UnsafeMutablePointer<UInt32>?,
    _ outSourceLongSide: UnsafeMutablePointer<UInt32>?
) -> Bool {
    outRGB?.pointee = nil
    outLength?.pointee = 0
    guard let path, longSide > 0, let kind = FacesTake.Decode(rawValue: kind) else { return false }
    let decoded = autoreleasepool { FacesTake.decode(path: String(cString: path), kind: kind, longSide: Int(longSide)) }
    guard let decoded, facesCopyOut(decoded.rgb, outRGB, outLength) else { return false }
    outWidth?.pointee = UInt32(decoded.width)
    outHeight?.pointee = UInt32(decoded.height)
    outSourceLongSide?.pointee = UInt32(clamping: decoded.sourceLongSide)
    return true
}

@_cdecl("fileid_apple_take_faces")
public func fileid_apple_take_faces(
    _ rgb: UnsafePointer<UInt8>?,
    _ width: UInt32,
    _ height: UInt32,
    _ people: Bool,
    _ detailWidth: UInt32,
    _ detailHeight: UInt32
) -> UnsafeMutablePointer<CChar>? {
    guard let rgb, width > 0, height > 0 else { return nil }
    return autoreleasepool {
        let buffer = UnsafeBufferPointer(start: rgb, count: Int(width) * Int(height) * 3)
        guard let image = FacesTake.image(rgb: buffer, width: Int(width), height: Int(height)) else { return nil }
        let detail = detailWidth > 0 && detailHeight > 0 ? (Int(detailWidth), Int(detailHeight)) : nil
        return facesJSON(FacesTake.analyze(image, people: people, detailSize: detail))
    }
}
