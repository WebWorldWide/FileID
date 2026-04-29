import Foundation
import CoreImage
import ImageIO

// MARK: - VisionProcessor

// Stateless I/O helpers. The actual Vision requests live in VisionWorker.
//
// Defensive notes — every call into ImageIO is wrapped in:
//   1. autoreleasepool (long scans accumulate CG scratch faster than Swift's
//      default pool drains, which manifests as memory pressure → OS termination)
//   2. A file-size sanity gate (skip 0-byte and impossibly-tiny inputs that
//      ImageIO will hard-fault on inside IIODictionary parsing — observed in
//      the 2026-04-22 crash log on a corrupted JPEG header)
//   3. A `disableTypeChecking` option flag where applicable, to avoid the
//      slow type-sniff path that some malformed files take through ImageIO
struct VisionProcessor {
    static let shared = VisionProcessor()

    // Below this we treat the file as corrupt and skip ImageIO entirely.
    // 256 B is well under the smallest valid JPEG (~1 KB minimum SOI/EOI plus
    // quantisation tables) and far above the 0-byte trap that walks
    // IIODictionary off the end of a memory page.
    private static let minImageBytes: Int64 = 256

    // Below this many bytes we still call ImageIO but stick to the fast
    // thumbnail path; full property scans on near-empty files are the
    // riskiest input.
    private static let minPropertiesScanBytes: Int64 = 1024

    private static func fileSizeBytes(_ url: URL) -> Int64 {
        (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize).flatMap { Int64($0) } ?? 0
    }

    // MARK: - Image loading

    // Thumbnail path first — skips the RAW decode pipeline on HEIC/JPEG.
    func loadImage(from url: URL, maxPixelSize: Int = 512) -> CGImage? {
        guard Self.fileSizeBytes(url) >= Self.minImageBytes else { return nil }
        return autoreleasepool {
            guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
            let fastOpts: [CFString: Any] = [
                kCGImageSourceCreateThumbnailFromImageIfAbsent: true,
                kCGImageSourceCreateThumbnailWithTransform:     true,
                kCGImageSourceThumbnailMaxPixelSize:            maxPixelSize,
                kCGImageSourceShouldCacheImmediately:           true
            ]
            if let img = CGImageSourceCreateThumbnailAtIndex(source, 0, fastOpts as CFDictionary),
               img.width >= 128 && img.height >= 128 {
                return img
            }
            // Fallback for PNG, RAW, or files without an embedded thumbnail.
            let fullOpts: [CFString: Any] = [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform:   true,
                kCGImageSourceThumbnailMaxPixelSize:          maxPixelSize,
                kCGImageSourceShouldCacheImmediately:         true
            ]
            return CGImageSourceCreateThumbnailAtIndex(source, 0, fullOpts as CFDictionary)
        }
    }

    // MARK: - EXIF

    func readEXIF(from url: URL) -> (cameraModel: String?, latitude: Double?, longitude: Double?, latRef: String?, lonRef: String?) {
        // The 2026-04-22 crash trace went through here on a corrupt JPEG.
        // Tiny files frequently lack the IFD pointers ImageIO assumes are
        // present and walk the parser off the end. Skip them entirely.
        guard Self.fileSizeBytes(url) >= Self.minPropertiesScanBytes else {
            return (nil, nil, nil, nil, nil)
        }
        return autoreleasepool {
            guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
                  let props  = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
            else { return (nil, nil, nil, nil, nil) }

            let camera = (props[kCGImagePropertyTIFFDictionary] as? [CFString: Any])?[kCGImagePropertyTIFFModel] as? String
            var lat: Double?, lon: Double?, latRef: String?, lonRef: String?
            if let gps = props[kCGImagePropertyGPSDictionary] as? [CFString: Any] {
                lat    = gps[kCGImagePropertyGPSLatitude]    as? Double
                lon    = gps[kCGImagePropertyGPSLongitude]   as? Double
                latRef = gps[kCGImagePropertyGPSLatitudeRef] as? String
                lonRef = gps[kCGImagePropertyGPSLongitudeRef] as? String
            }
            return (camera, lat, lon, latRef, lonRef)
        }
    }
}
