import CoreGraphics
import Foundation
import ImageIO
import Testing
import UniformTypeIdentifiers
@testable import FileIDEngine
@testable import FileIDImaging

@Suite("JPEG MPF preview")
struct JPEGPreviewTests {
    private static func quadrants(width: Int, height: Int, tint: CGFloat = 0, space name: CFString = CGColorSpace.sRGB) throws -> CGImage {
        let space = try #require(CGColorSpace(name: name))
        let ctx = try #require(CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                         space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let colours: [(CGFloat, CGFloat, CGFloat)] = [(0.9, 0.1, 0.1), (0.1, 0.8, 0.1), (0.1, 0.2, 0.9), (0.95, 0.85, tint)]
        for (index, colour) in colours.enumerated() {
            ctx.setFillColor(red: colour.0, green: colour.1, blue: colour.2, alpha: 1)
            ctx.fill(CGRect(x: (index % 2) * width / 2, y: (index / 2) * height / 2, width: width / 2, height: height / 2))
        }
        return try #require(ctx.makeImage())
    }

    private static func jpeg(_ image: CGImage, properties: [CFString: Any] = [:]) throws -> Data {
        let data = NSMutableData()
        let destination = try #require(CGImageDestinationCreateWithData(data, UTType.jpeg.identifier as CFString, 1, nil))
        var options = properties
        options[kCGImageDestinationLossyCompressionQuality] = 0.9
        CGImageDestinationAddImage(destination, image, options as CFDictionary)
        #expect(CGImageDestinationFinalize(destination))
        return data as Data
    }

    private static func le16(_ value: Int) -> [UInt8] { [UInt8(value & 0xFF), UInt8(value >> 8 & 0xFF)] }
    private static func le32(_ value: Int) -> [UInt8] { le16(value & 0xFFFF) + le16(value >> 16 & 0xFFFF) }

    /// A primary JPEG with an MPF APP2 segment after SOI and `preview` appended,
    /// laid out like a camera file. `previewType` is the MP entry type code.
    private static func mpfFile(primary: Data, preview: Data, previewType: Int = 0x010002, truncate: Int = 0) -> Data {
        let ifdEntries = 3
        let ifdSize = 2 + ifdEntries * 12 + 4
        let listOffset = 8 + ifdSize
        let payloadSize = 4 + listOffset + 32
        let segmentLength = payloadSize + 2
        let tiffStart = 2 + 4 + 4
        let previewStart = 2 + 2 + segmentLength + (primary.count - 2)
        var payload: [UInt8] = Array("MPF".utf8) + [0]
        payload += [0x49, 0x49, 0x2A, 0x00] + le32(8)
        payload += le16(ifdEntries)
        payload += le16(0xB000) + le16(7) + le32(4) + Array("0100".utf8)
        payload += le16(0xB001) + le16(4) + le32(1) + le32(2)
        payload += le16(0xB002) + le16(7) + le32(32) + le32(listOffset)
        payload += le32(0)
        payload += le32(0x2003_0000) + le32(primary.count + 4 + segmentLength) + le32(0) + le16(0) + le16(0)
        payload += le32(previewType) + le32(preview.count) + le32(previewStart - tiffStart) + le16(0) + le16(0)
        var out = Data([0xFF, 0xD8, 0xFF, 0xE2, UInt8(segmentLength >> 8), UInt8(segmentLength & 0xFF)])
        out.append(contentsOf: payload)
        out.append(primary.dropFirst(2))
        out.append(preview)
        return truncate > 0 ? out.prefix(out.count - truncate) : out
    }

    private static func write(_ data: Data) throws -> URL {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDJPEGPreview-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = directory.appendingPathComponent("camera.jpg")
        try data.write(to: url)
        return url
    }

    private static func sRGB(orientation: Int = 1) -> [CFString: Any] {
        [kCGImagePropertyOrientation: orientation, kCGImagePropertyExifDictionary: [kCGImagePropertyExifColorSpace: 1]]
    }

    private static func corners(_ image: CGImage) throws -> [[Int]] {
        let side = 16
        var bytes = [UInt8](repeating: 0, count: side * side * 4)
        let ctx = try #require(bytes.withUnsafeMutableBytes {
            CGContext(data: $0.baseAddress, width: side, height: side, bitsPerComponent: 8, bytesPerRow: side * 4,
                      space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        })
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: side, height: side))
        return [(3, 3), (12, 3), (3, 12), (12, 12)].map { x, y in
            let at = (y * side + x) * 4
            return [Int(bytes[at]), Int(bytes[at + 1]), Int(bytes[at + 2])]
        }
    }

    @Test("a camera JPEG's large preview stands in for the full image, upright for every orientation",
          arguments: 1...8)
    func usesPreviewUpright(orientation: Int) throws {
        let primary = try Self.jpeg(try Self.quadrants(width: 2400, height: 1600), properties: Self.sRGB(orientation: orientation))
        let preview = try Self.jpeg(try Self.quadrants(width: 1616, height: 1080))
        let url = try Self.write(Self.mpfFile(primary: primary, preview: preview))
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        let source = try #require(CGImageSourceCreateWithURL(url as CFURL, nil))
        let scan = try #require(decodeScanImage(source, path: url.path, maxPixelSize: 1536))
        let full = try #require(decodeBoundedImage(source, maxPixelSize: 1536))
        #expect(max(scan.width, scan.height) == 1616)
        #expect((scan.width > scan.height) == (full.width > full.height))
        for (got, want) in zip(try Self.corners(scan), try Self.corners(full)) {
            #expect(zip(got, want).allSatisfy { abs($0 - $1) <= 12 }, "orientation \(orientation): \(got) vs \(want)")
        }
    }

    @Test("a preview larger than the target is scaled to it")
    func scalesLargePreview() throws {
        let primary = try Self.jpeg(try Self.quadrants(width: 6000, height: 4000), properties: Self.sRGB())
        let preview = try Self.jpeg(try Self.quadrants(width: 2400, height: 1600))
        let url = try Self.write(Self.mpfFile(primary: primary, preview: preview))
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        let source = try #require(CGImageSourceCreateWithURL(url as CFURL, nil))
        let image = try #require(JPEGPreview.decode(source, path: url.path, maxPixelSize: 1536))
        #expect(image.width == 1536)
    }

    @Test("files without a usable preview fall back to the full image")
    func declines() throws {
        let primary = try Self.jpeg(try Self.quadrants(width: 2400, height: 1600), properties: Self.sRGB())
        let good = try Self.jpeg(try Self.quadrants(width: 1616, height: 1080))
        let cases: [(String, Data)] = [
            ("no MPF", primary),
            ("small preview", Self.mpfFile(primary: primary, preview: try Self.jpeg(try Self.quadrants(width: 640, height: 427)))),
            ("other shape", Self.mpfFile(primary: primary, preview: try Self.jpeg(try Self.quadrants(width: 1600, height: 1200)))),
            ("disparity entry", Self.mpfFile(primary: primary, preview: good, previewType: 0x020002)),
            ("truncated", Self.mpfFile(primary: primary, preview: good, truncate: 100)),
            ("not sRGB", Self.mpfFile(primary: try Self.jpeg(try Self.quadrants(width: 2400, height: 1600, space: CGColorSpace.adobeRGB1998),
                                                            properties: [kCGImagePropertyExifDictionary: [kCGImagePropertyExifColorSpace: 65535]]),
                                     preview: good)),
            ("primary near the target", Self.mpfFile(primary: try Self.jpeg(try Self.quadrants(width: 1800, height: 1200), properties: Self.sRGB()),
                                                     preview: try Self.jpeg(try Self.quadrants(width: 1200, height: 800)))),
        ]
        for (name, data) in cases {
            let url = try Self.write(data)
            defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
            let source = try #require(CGImageSourceCreateWithURL(url as CFURL, nil))
            #expect(JPEGPreview.decode(source, path: url.path, maxPixelSize: 1536) == nil, "\(name)")
            #expect(decodeScanImage(source, path: url.path, maxPixelSize: 1536) != nil, "\(name)")
        }
    }

    @Test("corrupted MPF segments never crash")
    func corruptedSegments() throws {
        let primary = try Self.jpeg(try Self.quadrants(width: 2400, height: 1600), properties: Self.sRGB())
        let preview = try Self.jpeg(try Self.quadrants(width: 1616, height: 1080))
        let file = Self.mpfFile(primary: primary, preview: preview)
        var generator = SystemRandomNumberGenerator()
        for _ in 0..<300 {
            var bytes = file
            for _ in 0..<4 {
                bytes[Int.random(in: 6..<min(80, bytes.count), using: &generator)] = UInt8.random(in: 0...255, using: &generator)
            }
            let url = try Self.write(bytes)
            defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
            guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { continue }
            if let image = JPEGPreview.decode(source, path: url.path, maxPixelSize: 1536) {
                #expect(max(image.width, image.height) == 1616)
            }
        }
    }
}
