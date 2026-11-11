// Stage 5: the scan's working images. A decoded CGImage stays here behind a
// handle so Vision, CLIP and RAM++ read the same pixels the Swift engine's
// Tagging.processImage reads, with no copy through Rust. Decode, EXIF, dHash
// and the video keyframe follow Tagging.swift and DeepAnalyze.swift line for
// line; the shared scan conformance case pins the results against the Swift
// engine.
import AVFoundation
import CoreGraphics
import Foundation
import ImageIO

final class ScanImages: @unchecked Sendable {
    static let shared = ScanImages()
    private let lock = NSLock()
    private var images: [UInt64: CGImage] = [:]
    private var next: UInt64 = 1

    func insert(_ image: CGImage) -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        let id = next
        next += 1
        images[id] = image
        return id
    }

    func image(_ id: UInt64) -> CGImage? {
        lock.lock()
        defer { lock.unlock() }
        return images[id]
    }

    func release(_ id: UInt64) {
        lock.lock()
        images[id] = nil
        lock.unlock()
    }

    var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return images.count
    }
}

enum ScanDecode {
    struct EXIF {
        var cameraModel: String?
        var lat: Double?
        var lon: Double?
        var capturedAt: Date?
    }

    /// `Tagging.loadImageAndEXIF`: files under 256 B are refused, the long edge
    /// is FILEID_SCAN_MAX_PIXELS (default 1536), EXIF comes from the same source.
    static func load(path: String, sizeBytes: Int64, maxPixels: Int?) -> (CGImage, EXIF)? {
        let url = URL(fileURLWithPath: path)
        var effectiveSize = sizeBytes
        if effectiveSize <= 0,
           let n = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber {
            effectiveSize = n.int64Value
        }
        if effectiveSize > 0 && effectiveSize < 256 { return nil }
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        let limit = maxPixels ?? ProcessInfo.processInfo.environment["FILEID_SCAN_MAX_PIXELS"]
            .flatMap { Int($0) }.map { max(256, min(4096, $0)) } ?? 1536
        guard let image = decodeScanImage(src, path: url.path, maxPixelSize: limit) else { return nil }
        return (image, readEXIF(from: src))
    }

    /// `Tagging.computeDHash`: 9x8 grey through CoreGraphics, bit row*8+col set
    /// when a pixel is brighter than its right neighbour.
    static func dHash(_ cgImage: CGImage) -> UInt64 {
        guard cgImage.width > 0, cgImage.height > 0 else { return 0 }
        let w = 9, h = 8
        var pixels = [UInt8](repeating: 0, count: w * h)
        let cs = CGColorSpaceCreateDeviceGray()
        guard let ctx = CGContext(
            data: &pixels, width: w, height: h, bitsPerComponent: 8,
            bytesPerRow: w, space: cs,
            bitmapInfo: CGImageAlphaInfo.none.rawValue
        ) else { return 0 }
        ctx.draw(cgImage, in: CGRect(x: 0, y: 0, width: CGFloat(w), height: CGFloat(h)))
        var hash: UInt64 = 0
        for row in 0..<h {
            for col in 0..<(w - 1) where pixels[row * w + col] > pixels[row * w + col + 1] {
                hash |= (UInt64(1) << UInt64(row * 8 + col))
            }
        }
        return hash
    }

    static func readEXIF(from src: CGImageSource) -> EXIF {
        guard let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any] else { return EXIF() }
        let exifDict = props[kCGImagePropertyExifDictionary] as? [CFString: Any]
        let capturedAt = [(kCGImagePropertyExifDateTimeOriginal, kCGImagePropertyExifOffsetTimeOriginal, kCGImagePropertyExifSubsecTimeOriginal),
                          (kCGImagePropertyExifDateTimeDigitized, kCGImagePropertyExifOffsetTimeDigitized, kCGImagePropertyExifSubsecTimeDigitized)]
            .lazy.compactMap { key, offsetKey, subsecKey in
                (exifDict?[key] as? String).flatMap { exifCaptureDate($0, offset: exifDict?[offsetKey] as? String) }
                    .map { $0.addingTimeInterval(exifSubsecFraction(exifDict?[subsecKey] as? String)) }
            }.first
        let tiff = props[kCGImagePropertyTIFFDictionary] as? [CFString: Any]
        let gps = props[kCGImagePropertyGPSDictionary] as? [CFString: Any]
        var lat = gps?[kCGImagePropertyGPSLatitude] as? Double
        var lon = gps?[kCGImagePropertyGPSLongitude] as? Double
        if let latRef = gps?[kCGImagePropertyGPSLatitudeRef] as? String, latRef == "S", let l = lat { lat = -l }
        if let lonRef = gps?[kCGImagePropertyGPSLongitudeRef] as? String, lonRef == "W", let l = lon { lon = -l }
        return EXIF(cameraModel: tiff?[kCGImagePropertyTIFFModel] as? String, lat: lat, lon: lon, capturedAt: capturedAt)
    }

    static func exifSubsecFraction(_ text: String?) -> Double {
        let digits = String((text ?? "").trimmingCharacters(in: .whitespaces).prefix { $0.isASCII && $0.isNumber }.prefix(9))
        guard !digits.isEmpty, let value = Double(digits) else { return 0 }
        return value / pow(10, Double(digits.count))
    }

    static func exifCaptureDate(_ text: String, offset: String?, now: Date = Date()) -> Date? {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy:MM:dd HH:mm:ss"
        formatter.timeZone = .current
        if let offset = offset?.trimmingCharacters(in: .whitespaces), offset.count == 6,
           let sign = offset.first, sign == "+" || sign == "-",
           let hours = Int(offset.dropFirst().prefix(2)), let minutes = Int(offset.suffix(2)),
           let zone = TimeZone(secondsFromGMT: (sign == "-" ? -1 : 1) * (hours * 3600 + minutes * 60)) {
            formatter.timeZone = zone
        }
        guard let date = formatter.date(from: text.trimmingCharacters(in: CharacterSet(charactersIn: "\0 "))) else { return nil }
        let unix = date.timeIntervalSince1970
        return unix >= 631_152_000 && unix <= now.timeIntervalSince1970 + 86_400 ? date : nil
    }

    /// Tightly packed sRGB RGB8 of `image` at its own size.
    static func rgb(_ image: CGImage) -> [UInt8]? {
        let w = image.width, h = image.height
        guard w > 0, h > 0 else { return nil }
        var rgba = [UInt8](repeating: 0, count: w * h * 4)
        let drawn = rgba.withUnsafeMutableBytes { buffer -> Bool in
            guard let ctx = CGContext(data: buffer.baseAddress, width: w, height: h, bitsPerComponent: 8,
                                      bytesPerRow: w * 4, space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue)
            else { return false }
            ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        guard drawn else { return nil }
        var rgb = [UInt8](repeating: 0, count: w * h * 3)
        for i in 0..<(w * h) {
            rgb[i * 3] = rgba[i * 4]
            rgb[i * 3 + 1] = rgba[i * 4 + 1]
            rgb[i * 3 + 2] = rgba[i * 4 + 2]
        }
        return rgb
    }
}

/// `Tagging.boundedVideoKeyframe` over `DeepAnalyze.extractTimedVideoFrame`:
/// the frame at a quarter of the duration (1 s when unknown, then 0), at most
/// `maxPixelSize` on the long edge, abandoned after `timeout`.
enum ScanVideo {
    private final class Box: @unchecked Sendable {
        private let lock = NSLock()
        private var value: CGImage?
        func set(_ v: CGImage?) { lock.lock(); value = v; lock.unlock() }
        func get() -> CGImage? { lock.lock(); defer { lock.unlock() }; return value }
    }

    private struct GeneratorRef: @unchecked Sendable { let generator: AVAssetImageGenerator }
    private struct AssetRef: @unchecked Sendable { let asset: AVURLAsset }

    static func keyframe(path: String, maxPixelSize: Int, timeout: TimeInterval) -> CGImage? {
        let box = Box()
        let done = DispatchSemaphore(value: 0)
        let task = Task.detached(priority: .utility) {
            box.set(await extract(url: URL(fileURLWithPath: path), maxPixelSize: maxPixelSize))
            done.signal()
        }
        if done.wait(timeout: .now() + timeout) == .timedOut {
            task.cancel()
            return nil
        }
        return box.get()
    }

    private static func extract(url: URL, maxPixelSize: Int) async -> CGImage? {
        let asset = AVURLAsset(url: url, options: [AVURLAssetPreferPreciseDurationAndTimingKey: false])
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.requestedTimeToleranceBefore = CMTime(seconds: 0.5, preferredTimescale: 600)
        generator.requestedTimeToleranceAfter = CMTime(seconds: 0.5, preferredTimescale: 600)
        generator.maximumSize = CGSize(width: maxPixelSize, height: maxPixelSize)
        let ref = GeneratorRef(generator: generator)
        func generate(at time: CMTime) async -> CGImage? {
            await withTaskCancellationHandler {
                await withCheckedContinuation { (continuation: CheckedContinuation<CGImage?, Never>) in
                    ref.generator.generateCGImageAsynchronously(for: time) { image, _, _ in
                        continuation.resume(returning: image)
                    }
                }
            } onCancel: {
                ref.generator.cancelAllCGImageGeneration()
            }
        }
        let duration = await duration(AssetRef(asset: asset), timeoutSeconds: 8)
        guard !Task.isCancelled else {
            generator.cancelAllCGImageGeneration()
            return nil
        }
        let target: CMTime
        if let duration, duration.isFinite, duration > 0 {
            target = CMTime(seconds: duration * 0.25, preferredTimescale: 600)
        } else {
            target = CMTime(seconds: 1, preferredTimescale: 600)
        }
        if let image = await generate(at: target) { return image }
        guard !Task.isCancelled else { return nil }
        return await generate(at: .zero)
    }

    private static func duration(_ asset: AssetRef, timeoutSeconds: UInt64) async -> Double? {
        await withTaskGroup(of: Double?.self) { group in
            group.addTask { (try? await asset.asset.load(.duration))?.seconds }
            group.addTask {
                try? await Task.sleep(nanoseconds: timeoutSeconds * 1_000_000_000)
                return nil
            }
            let first = await group.next() ?? nil
            group.cancelAll()
            return first
        }
    }
}

private func scanImageInfo(_ image: CGImage, exif: ScanDecode.EXIF?) -> [String: Any] {
    var info: [String: Any] = ["width": image.width, "height": image.height, "dhash": String(ScanDecode.dHash(image))]
    if let exif {
        info["cameraModel"] = exif.cameraModel
        info["lat"] = exif.lat
        info["lon"] = exif.lon
        info["capturedAt"] = exif.capturedAt?.timeIntervalSince1970
    }
    return info
}

func scanJSON(_ object: Any) -> UnsafeMutablePointer<CChar>? {
    guard JSONSerialization.isValidJSONObject(object),
          let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
          let text = String(data: data, encoding: .utf8) else { return nil }
    return strdup(text)
}

func scanString(_ pointer: UnsafePointer<CChar>?) -> String? {
    pointer.map { String(cString: $0) }
}

@_cdecl("fileid_apple_image_decode")
public func fileid_apple_image_decode(
    _ path: UnsafePointer<CChar>?,
    _ sizeBytes: Int64,
    _ maxPixelSize: UInt32,
    _ outImage: UnsafeMutablePointer<UInt64>?
) -> UnsafeMutablePointer<CChar>? {
    outImage?.pointee = 0
    guard let path = scanString(path) else { return nil }
    return autoreleasepool {
        guard let (image, exif) = ScanDecode.load(path: path, sizeBytes: sizeBytes,
                                                  maxPixels: maxPixelSize == 0 ? nil : Int(maxPixelSize)) else { return nil }
        let info = scanImageInfo(image, exif: exif)
        outImage?.pointee = ScanImages.shared.insert(image)
        return scanJSON(info)
    }
}

@_cdecl("fileid_apple_video_keyframe")
public func fileid_apple_video_keyframe(
    _ path: UnsafePointer<CChar>?,
    _ maxPixelSize: UInt32,
    _ timeoutMs: UInt32,
    _ outImage: UnsafeMutablePointer<UInt64>?
) -> UnsafeMutablePointer<CChar>? {
    outImage?.pointee = 0
    guard let path = scanString(path) else { return nil }
    return autoreleasepool {
        guard let image = ScanVideo.keyframe(path: path, maxPixelSize: Int(max(1, maxPixelSize)),
                                             timeout: TimeInterval(timeoutMs) / 1000) else { return nil }
        let info = scanImageInfo(image, exif: nil)
        outImage?.pointee = ScanImages.shared.insert(image)
        return scanJSON(info)
    }
}

@_cdecl("fileid_apple_image_rgb")
public func fileid_apple_image_rgb(
    _ image: UInt64,
    _ outPixels: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outLength: UnsafeMutablePointer<Int>?,
    _ outWidth: UnsafeMutablePointer<UInt32>?,
    _ outHeight: UnsafeMutablePointer<UInt32>?
) -> Bool {
    outPixels?.pointee = nil
    outLength?.pointee = 0
    guard let cgImage = ScanImages.shared.image(image), let rgb = autoreleasepool(invoking: { ScanDecode.rgb(cgImage) }),
          let buffer = malloc(rgb.count)?.assumingMemoryBound(to: UInt8.self) else { return false }
    rgb.withUnsafeBufferPointer { buffer.update(from: $0.baseAddress!, count: rgb.count) }
    outPixels?.pointee = buffer
    outLength?.pointee = rgb.count
    outWidth?.pointee = UInt32(cgImage.width)
    outHeight?.pointee = UInt32(cgImage.height)
    return true
}

@_cdecl("fileid_apple_image_release")
public func fileid_apple_image_release(_ image: UInt64) {
    ScanImages.shared.release(image)
}
