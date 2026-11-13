import CoreGraphics
import Foundation
import ImageIO

/// The large preview a camera stores inside its JPEGs as a CIPA DC-007 (MPF)
/// "large thumbnail" (Sony 1616x1080, Nikon and Fujifilm similar). Used under
/// the same size rule as RAW previews in `decodeBoundedImage`: at least two
/// thirds of the target, native size up to a third larger, scaled above that.
/// The preview is the whole frame (stretched to 1080 rows on Sony), so
/// normalized boxes on it match the full image.
enum JPEGPreview {
    struct Entry: Equatable {
        let offset: Int
        let size: Int
    }

    static let maxPreviewBytes = 8 << 20
    private static let largeThumbnailTypes: Set<UInt32> = [0x010001, 0x010002]

    static func decode(_ source: CGImageSource, path: String, maxPixelSize: Int) -> CGImage? {
        guard let primary = primaryInfo(source) else { return nil }
        let cap = maxPixelSize * 4 / 3
        guard max(primary.width, primary.height) >= cap else { return nil }
        guard let data = previewData(path: path) else { return nil }
        guard let preview = CGImageSourceCreateWithData(data as CFData, nil),
              let props = CGImageSourceCopyPropertiesAtIndex(preview, 0, nil) as? [CFString: Any],
              let width = props[kCGImagePropertyPixelWidth] as? Int,
              let height = props[kCGImagePropertyPixelHeight] as? Int,
              (props[kCGImagePropertyOrientation] as? Int ?? 1) == 1,
              sameShape(width, height, primary.width, primary.height) else { return nil }
        let side = max(width, height)
        guard side * 3 >= maxPixelSize * 2 else { return nil }
        let image: CGImage?
        if side < cap {
            image = CGImageSourceCreateImageAtIndex(preview, 0, [kCGImageSourceShouldCacheImmediately: true] as CFDictionary)
        } else {
            let options: [CFString: Any] = [
                kCGImageSourceShouldCacheImmediately: true,
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceThumbnailMaxPixelSize: maxPixelSize
            ]
            image = CGImageSourceCreateThumbnailAtIndex(preview, 0, options as CFDictionary)
        }
        guard let image else { return nil }
        return primary.orientation == 1 ? image : oriented(image, primary.orientation)
    }

    private struct Primary {
        let width: Int
        let height: Int
        let orientation: Int
    }

    /// Stored (unrotated) size and orientation of the full image. Skips files
    /// not tagged sRGB, whose preview colour cannot be trusted to match.
    private static func primaryInfo(_ source: CGImageSource) -> Primary? {
        guard let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = props[kCGImagePropertyPixelWidth] as? Int,
              let height = props[kCGImagePropertyPixelHeight] as? Int else { return nil }
        let exif = props[kCGImagePropertyExifDictionary] as? [CFString: Any]
        guard (exif?[kCGImagePropertyExifColorSpace] as? Int) == 1 else { return nil }
        let orientation = props[kCGImagePropertyOrientation] as? Int ?? 1
        guard (1...8).contains(orientation) else { return nil }
        return Primary(width: width, height: height, orientation: orientation)
    }

    static func sameShape(_ width: Int, _ height: Int, _ primaryWidth: Int, _ primaryHeight: Int) -> Bool {
        guard width > 0, height > 0, primaryWidth > 0, primaryHeight > 0 else { return false }
        let ratio = Double(width) / Double(height)
        let primaryRatio = Double(primaryWidth) / Double(primaryHeight)
        return abs(ratio / primaryRatio - 1) < 0.01
    }

    static func previewData(path: String) -> Data? {
        let descriptor = open(path, O_RDONLY | O_CLOEXEC)
        guard descriptor >= 0 else { return nil }
        defer { close(descriptor) }
        var info = stat()
        guard fstat(descriptor, &info) == 0, info.st_size > 0 else { return nil }
        let fileSize = Int(info.st_size)
        guard let entry = largestThumbnail(descriptor: descriptor, fileSize: fileSize),
              let data = readExactly(descriptor, offset: entry.offset, count: entry.size),
              data.count > 3, data[0] == 0xFF, data[1] == 0xD8 else { return nil }
        return data
    }

    static let headBytes = 64 << 10

    /// Walks the marker segments before the first scan, finds the MPF APP2
    /// segment and returns its largest large-thumbnail entry. The walk runs
    /// over one read of the file's head; segments past it are read on demand.
    static func largestThumbnail(descriptor: Int32, fileSize: Int) -> Entry? {
        let head = readPrefix(descriptor, count: headBytes)
        func bytes(_ offset: Int, _ count: Int) -> Data? {
            guard offset >= 0, count > 0 else { return nil }
            if offset + count <= head.count { return head.subdata(in: offset..<offset + count) }
            return readExactly(descriptor, offset: offset, count: count)
        }
        guard let soi = bytes(0, 2), soi[0] == 0xFF, soi[1] == 0xD8 else { return nil }
        var position = 2
        for _ in 0..<64 {
            guard let header = bytes(position, 4), header[0] == 0xFF else { return nil }
            let marker = header[1]
            if marker == 0xDA || marker == 0xD9 { return nil }
            let length = Int(header[2]) << 8 | Int(header[3])
            guard length >= 2 else { return nil }
            if marker == 0xE2, length >= 16,
               let segment = bytes(position + 4, length - 2),
               let entries = mpfEntries(segment, segmentOffset: position + 4) {
                let fits = entries.filter { $0.offset > 0 && $0.size > 0 && $0.size <= maxPreviewBytes && $0.offset + $0.size <= fileSize }
                return fits.max { $0.size < $1.size }
            }
            position += 2 + length
        }
        return nil
    }

    /// Large-thumbnail entries of an APP2 payload that starts with "MPF\0";
    /// offsets are made absolute (MPF offsets count from its TIFF header).
    static func mpfEntries(_ segment: Data, segmentOffset: Int) -> [Entry]? {
        let bytes = [UInt8](segment)
        guard bytes.count >= 16, bytes[0] == 0x4D, bytes[1] == 0x50, bytes[2] == 0x46, bytes[3] == 0 else { return nil }
        let reader = TIFFReader(bytes: bytes, base: 4)
        guard let little = reader.littleEndian, let ifd = reader.u32(4, little).map({ Int($0) }),
              let tagCount = reader.u16(ifd, little) else { return nil }
        var count = 0
        var listOffset = 0
        for index in 0..<Int(tagCount) {
            let tag = ifd + 2 + index * 12
            guard let id = reader.u16(tag, little), let value = reader.u32(tag + 8, little) else { return nil }
            if id == 0xB001 { count = Int(value) }
            if id == 0xB002 { listOffset = Int(value) }
        }
        guard count > 0, count <= 16, listOffset > 0 else { return nil }
        var entries: [Entry] = []
        for index in 0..<count {
            let at = listOffset + index * 16
            guard let attribute = reader.u32(at, little), let size = reader.u32(at + 4, little),
                  let offset = reader.u32(at + 8, little) else { return nil }
            guard largeThumbnailTypes.contains(attribute & 0x00FF_FFFF), offset > 0 else { continue }
            entries.append(Entry(offset: segmentOffset + 4 + Int(offset), size: Int(size)))
        }
        return entries
    }

    private struct TIFFReader {
        let bytes: [UInt8]
        let base: Int

        var littleEndian: Bool? {
            guard bytes.count >= base + 8 else { return nil }
            if bytes[base] == 0x49, bytes[base + 1] == 0x49 { return true }
            if bytes[base] == 0x4D, bytes[base + 1] == 0x4D { return false }
            return nil
        }

        func u16(_ offset: Int, _ little: Bool) -> UInt16? {
            let at = base + offset
            guard offset >= 0, at + 2 <= bytes.count else { return nil }
            let a = UInt16(bytes[at]), b = UInt16(bytes[at + 1])
            return little ? a | b << 8 : a << 8 | b
        }

        func u32(_ offset: Int, _ little: Bool) -> UInt32? {
            guard let a = u16(offset, little), let b = u16(offset + 2, little) else { return nil }
            return little ? UInt32(a) | UInt32(b) << 16 : UInt32(a) << 16 | UInt32(b)
        }
    }

    private static func readPrefix(_ descriptor: Int32, count: Int) -> Data {
        var data = Data(count: count)
        let done = data.withUnsafeMutableBytes { raw -> Int in
            guard let base = raw.baseAddress else { return 0 }
            var total = 0
            while total < count {
                let got = pread(descriptor, base + total, count - total, off_t(total))
                if got < 0 && errno == EINTR { continue }
                if got <= 0 { break }
                total += got
            }
            return total
        }
        return data.prefix(done)
    }

    private static func readExactly(_ descriptor: Int32, offset: Int, count: Int) -> Data? {
        guard offset >= 0, count > 0 else { return nil }
        var data = Data(count: count)
        let done = data.withUnsafeMutableBytes { raw -> Int in
            guard let base = raw.baseAddress else { return -1 }
            var total = 0
            while total < count {
                let got = pread(descriptor, base + total, count - total, off_t(offset + total))
                if got < 0 && errno == EINTR { continue }
                if got <= 0 { break }
                total += got
            }
            return total
        }
        return done == count ? data : nil
    }

    /// Draws `image` upright for an EXIF orientation (2–8).
    static func oriented(_ image: CGImage, _ orientation: Int) -> CGImage? {
        let width = image.width, height = image.height
        let swaps = orientation >= 5
        let outWidth = swaps ? height : width
        let outHeight = swaps ? width : height
        let space = image.colorSpace ?? CGColorSpace(name: CGColorSpace.sRGB)!
        guard let context = CGContext(data: nil, width: outWidth, height: outHeight, bitsPerComponent: 8, bytesPerRow: 0,
                                      space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return nil }
        context.concatenate(transform(orientation, width: CGFloat(width), height: CGFloat(height)))
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        return context.makeImage()
    }

    /// Maps the stored image (drawn at the origin, y up) onto the upright canvas.
    static func transform(_ orientation: Int, width w: CGFloat, height h: CGFloat) -> CGAffineTransform {
        switch orientation {
        case 2: return CGAffineTransform(a: -1, b: 0, c: 0, d: 1, tx: w, ty: 0)
        case 3: return CGAffineTransform(a: -1, b: 0, c: 0, d: -1, tx: w, ty: h)
        case 4: return CGAffineTransform(a: 1, b: 0, c: 0, d: -1, tx: 0, ty: h)
        case 5: return CGAffineTransform(a: 0, b: -1, c: -1, d: 0, tx: h, ty: w)
        case 6: return CGAffineTransform(a: 0, b: -1, c: 1, d: 0, tx: 0, ty: w)
        case 7: return CGAffineTransform(a: 0, b: 1, c: 1, d: 0, tx: 0, ty: 0)
        case 8: return CGAffineTransform(a: 0, b: 1, c: -1, d: 0, tx: h, ty: 0)
        default: return .identity
        }
    }
}
