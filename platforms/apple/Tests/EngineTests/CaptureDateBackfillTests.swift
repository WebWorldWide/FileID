import CoreGraphics
import Foundation
import GRDB
import ImageIO
import Testing
import FileIDShared
import UniformTypeIdentifiers
@testable import FileIDEngine

@Suite("Capture-date backfill", .serialized)
struct CaptureDateBackfillTests {
    @Test("copy dates on photos are replaced by EXIF times once per folder")
    func backfillsOnce() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDDates-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root); try? FileManager.default.removeItem(at: CaptureDateBackfill.marker(for: root)) }
        let ctx = try #require(CGContext(data: nil, width: 8, height: 8, bitsPerComponent: 8, bytesPerRow: 0,
                                         space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let image = try #require(ctx.makeImage())
        let dated = root.appendingPathComponent("dated.jpg"), plain = root.appendingPathComponent("plain.jpg")
        for (url, exif) in [(dated, ["DateTimeOriginal": "2019:07:04 12:00:00", "OffsetTimeOriginal": "+00:00", "SubsecTimeOriginal": "25"]), (plain, [:])] {
            let destination = try #require(CGImageDestinationCreateWithURL(url as CFURL, UTType.jpeg.identifier as CFString, 1, nil))
            CGImageDestinationAddImage(destination, image, [kCGImagePropertyExifDictionary: exif] as CFDictionary)
            #expect(CGImageDestinationFinalize(destination))
        }
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await db.pool.write { sql in
            for (id, url) in [(Int64(1), dated), (2, plain)] {
                try sql.execute(sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, created_at) VALUES (?,?,?,1,0,'image','jpg',1700000000)",
                                arguments: [id, url.path, StablePathHash.hash(url.path)])
            }
        }
        #expect(await CaptureDateBackfill.runIfNeeded(database: db, root: root, isCancelled: { false }) == 1)
        let stored = try await db.pool.read { try Double.fetchAll($0, sql: "SELECT created_at FROM files ORDER BY id") }
        #expect(stored == [1_562_241_600.25, 1_700_000_000])
        try await db.pool.write { try $0.execute(sql: "UPDATE files SET created_at = 1700000000 WHERE id = 1") }
        #expect(await CaptureDateBackfill.runIfNeeded(database: db, root: root, isCancelled: { false }) == 0)
    }
}
