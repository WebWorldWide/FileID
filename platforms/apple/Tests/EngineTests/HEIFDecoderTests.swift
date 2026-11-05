import CoreGraphics
import Foundation
import ImageIO
import Testing
import UniformTypeIdentifiers
@testable import FileIDEngine

@Suite("HEIF decoder")
struct HEIFDecoderTests {
    /// Four coloured quadrants, so a wrong rotation or mirror shows up as a colour swap.
    private static func quadrants(width: Int, height: Int, displayP3: Bool = false) throws -> CGImage {
        let space = try #require(CGColorSpace(name: displayP3 ? CGColorSpace.displayP3 : CGColorSpace.sRGB))
        let ctx = try #require(CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                         space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let colours: [(CGFloat, CGFloat, CGFloat)] = [(0.9, 0.1, 0.1), (0.1, 0.8, 0.1), (0.1, 0.2, 0.9), (0.95, 0.85, 0.1)]
        for (index, colour) in colours.enumerated() {
            ctx.setFillColor(red: colour.0, green: colour.1, blue: colour.2, alpha: 1)
            ctx.fill(CGRect(x: (index % 2) * width / 2, y: (index / 2) * height / 2, width: width / 2, height: height / 2))
        }
        return try #require(ctx.makeImage())
    }

    /// Nil where this machine has no HEVC encoder (some virtual machines).
    private static func writeHEIC(_ image: CGImage, orientation: Int, in directory: URL, name: String) -> URL? {
        let url = directory.appendingPathComponent(name)
        guard let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.heic.identifier as CFString, 1, nil)
        else { return nil }
        CGImageDestinationAddImage(destination, image, [kCGImagePropertyOrientation: orientation,
                                                        kCGImageDestinationLossyCompressionQuality: 0.9] as CFDictionary)
        return CGImageDestinationFinalize(destination) ? url : nil
    }

    private static func imageIO(_ url: URL, maxPixelSize: Int) -> CGImage? {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        return decodeBoundedImage(source, maxPixelSize: maxPixelSize)
    }

    private static func pixels(_ image: CGImage, width: Int, height: Int) throws -> [UInt8] {
        var bytes = [UInt8](repeating: 0, count: width * height * 4)
        let ctx = try #require(bytes.withUnsafeMutableBytes {
            CGContext(data: $0.baseAddress, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                      space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        })
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        return bytes
    }

    private static func temporaryDirectory() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDHEIF-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    @Test("tiled HEICs decode to ImageIO's size, orientation and colours",
          arguments: [(1, false), (3, false), (6, false), (8, true)])
    func matchesImageIO(orientation: Int, displayP3: Bool) throws {
        let directory = try Self.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        guard let url = Self.writeHEIC(try Self.quadrants(width: 2100, height: 1400, displayP3: displayP3),
                                       orientation: orientation, in: directory, name: "photo.heic") else { return }
        let ours = try #require(HEIFDecoder.decode(path: url.path, maxPixelSize: 1536))
        let reference = try #require(Self.imageIO(url, maxPixelSize: 1536))
        #expect(ours.width == reference.width && ours.height == reference.height)
        let width = 48, height = max(1, 48 * reference.height / reference.width)
        let a = try Self.pixels(ours, width: width, height: height)
        let b = try Self.pixels(reference, width: width, height: height)
        let difference = zip(a, b).enumerated().filter { $0.offset % 4 != 3 }
            .map { abs(Int($0.element.0) - Int($0.element.1)) }.reduce(0, +)
        #expect(Double(difference) / Double(width * height * 3) < 4)
    }

    @Test("layouts it does not handle return nil for ImageIO to decode")
    func declinesUnhandledInput() throws {
        let directory = try Self.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let random = directory.appendingPathComponent("random.heic")
        var generator = SystemRandomNumberGenerator()
        try Data((0..<4096).map { _ in UInt8.random(in: 0...255, using: &generator) }).write(to: random)
        #expect(HEIFDecoder.decode(path: random.path, maxPixelSize: 1536) == nil)
        #expect(HEIFDecoder.decode(path: directory.appendingPathComponent("missing.heic").path, maxPixelSize: 1536) == nil)
        let empty = directory.appendingPathComponent("empty.heic")
        try Data().write(to: empty)
        #expect(HEIFDecoder.decode(path: empty.path, maxPixelSize: 1536) == nil)
        // An odd-sized single image carries a clean-aperture crop, which is left to ImageIO.
        if let odd = Self.writeHEIC(try Self.quadrants(width: 401, height: 301),
                                    orientation: 1, in: directory, name: "odd.heic") {
            #expect(HEIFDecoder.decode(path: odd.path, maxPixelSize: 1536) == nil)
            let loaded = try #require(Tagging.loadImageAndEXIF(url: odd, sizeBytes: 100_000))
            #expect(loaded.0.width == 401 && loaded.0.height == 301)
        }
    }

    @Test("corrupted and truncated HEICs never crash the decoder")
    func survivesCorruption() throws {
        let directory = try Self.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        guard let url = Self.writeHEIC(try Self.quadrants(width: 1100, height: 800),
                                       orientation: 6, in: directory, name: "photo.heic") else { return }
        let original = [UInt8](try Data(contentsOf: url))
        let mutated = directory.appendingPathComponent("mutated.heic")
        var state: UInt64 = 0x9E37_79B9_7F4A_7C15
        func next() -> Int {
            state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return Int(truncatingIfNeeded: state >> 33)
        }
        let header = min(original.count, 2048)
        for round in 0..<400 {
            var bytes = original
            if round % 5 == 4 {
                bytes = Array(bytes.prefix(next() % original.count))
            } else {
                for _ in 0..<(1 + next() % 6) {
                    let index = round % 2 == 0 ? next() % header : next() % bytes.count
                    bytes[index] = UInt8(truncatingIfNeeded: next())
                }
            }
            try Data(bytes).write(to: mutated)
            if let image = HEIFDecoder.decode(path: mutated.path, maxPixelSize: 1536) {
                #expect(image.width > 0 && image.height > 0 && max(image.width, image.height) <= 1536)
            }
        }
    }

    @Test("the plane budget holds a second large decode until the first releases")
    func planeBudgetWaits() async throws {
        let budget = HEIFDecoder.PlaneBudget(limitBytes: 100)
        budget.acquire(80)
        let started = Date()
        let waiter = Task.detached {
            budget.acquire(40)
            budget.release(40)
            return Date().timeIntervalSince(started)
        }
        try await Task.sleep(for: .milliseconds(150))
        #expect(budget.inUse == 80)
        budget.release(80)
        let waited = await waiter.value
        #expect(waited >= 0.1)
        #expect(budget.inUse == 0)
        budget.acquire(500)
        #expect(budget.inUse == 500)
        budget.release(500)
    }

    @Test("the scan loads HEICs through the HEIF decoder")
    func scanUsesDecoder() throws {
        let directory = try Self.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        guard let url = Self.writeHEIC(try Self.quadrants(width: 2100, height: 1400),
                                       orientation: 6, in: directory, name: "photo.heic") else { return }
        let loaded = try #require(Tagging.loadImageAndEXIF(url: url, sizeBytes: 500_000))
        let direct = try #require(HEIFDecoder.decode(path: url.path, maxPixelSize: 1536))
        #expect(loaded.0.width == direct.width && loaded.0.height == direct.height)
        #expect(loaded.0.width == 1024 && loaded.0.height == 1536)
        #expect(loaded.0.dataProvider?.data == direct.dataProvider?.data)
    }

    @Test("Deep Analyze, face clustering and Takes load HEICs through the HEIF decoder, other formats through ImageIO")
    func analysisUsesDecoder() async throws {
        let directory = try Self.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        guard let url = Self.writeHEIC(try Self.quadrants(width: 2100, height: 1400),
                                       orientation: 6, in: directory, name: "photo.heic") else { return }
        let deep = try #require(await DeepAnalyze.loadCGImage(url: url, maxPixelSize: 1024))
        let deepDirect = try #require(HEIFDecoder.decode(path: url.path, maxPixelSize: 1024))
        #expect(deep.height == 1024 && deep.width < deep.height)
        #expect(deep.dataProvider?.data == deepDirect.dataProvider?.data)
        let source = try #require(CGImageSourceCreateWithURL(url as CFURL, nil))
        let clustering = try #require(decodeBoundedImage(source, path: url.path, maxPixelSize: 2048))
        let clusteringDirect = try #require(HEIFDecoder.decode(path: url.path, maxPixelSize: 2048))
        #expect(clustering.height == 2048 && clustering.width < clustering.height)
        #expect(clustering.dataProvider?.data == clusteringDirect.dataProvider?.data)
        let jpeg = directory.appendingPathComponent("photo.jpg")
        let destination = try #require(CGImageDestinationCreateWithURL(jpeg as CFURL, UTType.jpeg.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(destination, try Self.quadrants(width: 300, height: 200), nil)
        #expect(CGImageDestinationFinalize(destination))
        let jpegSource = try #require(CGImageSourceCreateWithURL(jpeg as CFURL, nil))
        #expect(!isHEIF(jpegSource) && isHEIF(source))
        #expect(decodeBoundedImage(jpegSource, path: jpeg.path, maxPixelSize: 2048)?.width == 300)
    }
}
