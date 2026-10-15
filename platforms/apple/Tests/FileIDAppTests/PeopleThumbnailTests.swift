import AppKit
import Foundation
import GRDB
import ImageIO
import Testing
@testable import FileID

@Suite("People thumbnail source coordinates", .serialized)
@MainActor
struct PeopleThumbnailTests {
    private let bitmapInfo = CGBitmapInfo.byteOrder32Big.rawValue | CGImageAlphaInfo.premultipliedLast.rawValue
    private let bbox = #"{"x":8,"y":8,"w":16,"h":16,"coordinateSpace":"pixel-top-left","sourceWidth":84,"sourceHeight":64}"#

    private func jpeg(orientation: Int) throws -> Data {
        let width = 84
        let height = 64
        var bytes = [UInt8](repeating: 0, count: width * height * 4)
        for y in 0..<height {
            for x in 0..<width {
                let offset = (y * width + x) * 4
                let green = (8..<24).contains(x) && (8..<24).contains(y)
                bytes[offset + 1] = green ? 255 : 0
                bytes[offset + 2] = green ? 0 : 255
                bytes[offset + 3] = 255
            }
        }
        let provider = try #require(CGDataProvider(data: Data(bytes) as CFData))
        let image = try #require(CGImage(width: width, height: height, bitsPerComponent: 8,
            bitsPerPixel: 32, bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: bitmapInfo), provider: provider,
            decode: nil, shouldInterpolate: false, intent: .defaultIntent))
        let data = NSMutableData()
        let destination = try #require(CGImageDestinationCreateWithData(data, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, image, [kCGImagePropertyOrientation: orientation,
            kCGImageDestinationLossyCompressionQuality: 1.0] as CFDictionary)
        #expect(CGImageDestinationFinalize(destination))
        return data as Data
    }

    private func upright(_ data: Data) throws -> NSImage {
        let source = try #require(CGImageSourceCreateWithData(data as CFData, nil))
        let image = try #require(CGImageSourceCreateThumbnailAtIndex(source, 0, [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 360
        ] as CFDictionary))
        return NSImage(cgImage: image, size: NSSize(width: image.width, height: image.height))
    }

    private func sample(_ image: NSImage) throws -> (green: Int, blue: Int) {
        let pixels = try #require(image.cgImage(forProposedRect: nil, context: nil, hints: nil))
        var bytes = [UInt8](repeating: 0, count: 4)
        return try bytes.withUnsafeMutableBytes { buffer in
            let context = try #require(CGContext(data: buffer.baseAddress, width: 1, height: 1,
                bitsPerComponent: 8, bytesPerRow: 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: bitmapInfo))
            context.interpolationQuality = .none
            context.draw(pixels, in: CGRect(x: 0, y: 0, width: 1, height: 1))
            return (Int(buffer[1]), Int(buffer[2]))
        }
    }

    @Test("Imported raw-pixel crops select the same subject under all EXIF orientations", arguments: 1...8)
    func importedCrop(orientation: Int) throws {
        let image = try upright(jpeg(orientation: orientation))
        let crop = try #require(ThumbnailService.cropFace(in: image, bbox: bbox, sourceOrientation: orientation))
        let color = try sample(crop)
        #expect(color.green > 200)
        #expect(color.blue < 40)
        if orientation == 6 {
            let oldCrop = try #require(ThumbnailService.cropFace(in: image, bbox: bbox))
            #expect(try sample(oldCrop).blue > 200)
        }
    }

    @Test("Native upright crops remain unchanged and ambiguous rotated legacy boxes defer")
    func uprightAndLegacy() throws {
        let image = try upright(jpeg(orientation: 6))
        let csv = "0.625,0.7142857142857143,0.25,0.19047619047619047"
        let crop = try #require(ThumbnailService.cropFace(in: image, bbox: csv, sourceOrientation: 6))
        #expect(try sample(crop).green > 200)
        #expect(ThumbnailService.cropFace(in: image, bbox: #"{"x":8,"y":8,"w":16,"h":16}"#,
            sourceOrientation: 6) == nil)
    }

    @Test("Metadata reads preserve the image and safely defer unavailable sources")
    func sourceMetadata() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDPeople-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let url = root.appendingPathComponent("rotated.jpg")
        let bytes = try jpeg(orientation: 6)
        try bytes.write(to: url)
        #expect(await ThumbnailService.sourceOrientation(for: url) == 6)
        #expect(try Data(contentsOf: url) == bytes)
        #expect(await ThumbnailService.sourceOrientation(for: root.appendingPathComponent("missing.jpg")) == nil)
    }
}
