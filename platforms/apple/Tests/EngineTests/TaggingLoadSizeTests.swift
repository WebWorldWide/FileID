// F-C6-008 regression: loadImageAndEXIF must reuse the size Discovery already
// stat'd (DiscoveredFile.sizeBytes) instead of issuing a second
// FileManager.attributesOfItem per image — an extra SMB/NFS round-trip on NAS.
//
// The redundant-stat removal is provable without mocking FileManager: drive the
// <256 B skip with the *passed* size while a real, well-over-256 B image sits on
// disk. If the function re-statted it would see the on-disk size (>=256) and
// decode; honoring the parameter means it short-circuits to nil. The positive
// control (large passed size → decodes) confirms the decode path is otherwise
// intact, so the only thing steering the result is the parameter, not a stat.
import Testing
import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers
@testable import FileIDEngine

@Suite("Tagging.loadImageAndEXIF reuses the discovered size (F-C6-008)")
struct TaggingLoadSizeTests {

    /// Writes a real, decodable PNG (>256 B) to a fresh temp dir; returns its URL.
    private func makeTempPNG(width: Int = 64, height: Int = 64) throws -> URL {
        let cs = CGColorSpaceCreateDeviceRGB()
        let ctx = try #require(CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: 0, space: cs,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ))
        ctx.setFillColor(CGColor(red: 0.2, green: 0.4, blue: 0.6, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
        let cg = try #require(ctx.makeImage())

        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDTaggingLoad-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("img.png")
        let dest = try #require(CGImageDestinationCreateWithURL(
            url as CFURL, UTType.png.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(dest, cg, nil)
        #expect(CGImageDestinationFinalize(dest))
        return url
    }

    @Test("a sub-256 passed size skips decode even though the file on disk is larger")
    func honorsPassedSizeWithoutRestat() throws {
        let url = try makeTempPNG()
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }

        // Precondition: the actual file is well over the 256 B floor, so a fresh
        // stat would say "decode". (If this ever fails the test is meaningless.)
        let onDisk = try #require(
            (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? Int)
        #expect(onDisk >= 256)

        // Passing a sub-256 size must short-circuit to nil. A re-stat would see
        // `onDisk` (>=256) and return a decoded image — so nil proves the
        // function trusts the parameter and never re-stats.
        #expect(Tagging.loadImageAndEXIF(url: url, sizeBytes: 100) == nil)
    }

    @Test("a >=256 passed size still decodes the image (positive control)")
    func decodesWhenPassedSizeAboveFloor() throws {
        let url = try makeTempPNG()
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }

        let result = Tagging.loadImageAndEXIF(url: url, sizeBytes: 1_000_000)
        #expect(result != nil)
    }

    // R-13: a 0/absent discovered size means UNKNOWN, not tiny — Discovery's
    // .fileSizeKey can come back empty on some SMB/NFS volumes, leaving a valid
    // image at sizeBytes 0. The loader must re-stat and decode it rather than
    // short-circuit to decode-failed via the <256 B guard.
    @Test("a 0 discovered size re-stats and still decodes (not decode-failed)")
    func zeroSizeFallsBackToStatAndDecodes() throws {
        let url = try makeTempPNG()
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }

        // The on-disk file is well over 256 B; only the *passed* size is unknown.
        let onDisk = try #require(
            (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? Int)
        #expect(onDisk >= 256)

        let result = Tagging.loadImageAndEXIF(url: url, sizeBytes: 0)
        #expect(result != nil,
                "a 0 discovered size must re-stat and decode, not skip a valid image as corrupt")
    }
}
