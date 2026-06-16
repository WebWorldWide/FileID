// Byte-exact content hashing for Cleanup's literal-duplicate detection (item 1).
// Mirrors the Windows engine's content_hash tests: identical bytes hash equal
// regardless of path, distinct bytes differ, and the >threshold composite path
// is deterministic + differs from the full hash. SHA-256 (CryptoKit) substitutes
// for BLAKE3 — values are macOS-local, behavior is lockstep.
import Testing
import Foundation
@testable import FileIDEngine

@Suite("ContentHash (byte-exact dedup)")
struct ContentHashTests {
    private func tmp(_ bytes: [UInt8]) throws -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-chash-\(UUID().uuidString).bin")
        try Data(bytes).write(to: url)
        return url
    }

    @Test("Identical bytes hash equal regardless of path")
    func identicalEqual() throws {
        let a = try tmp(Array("the quick brown fox".utf8))
        let b = try tmp(Array("the quick brown fox".utf8))
        defer {
            try? FileManager.default.removeItem(at: a)
            try? FileManager.default.removeItem(at: b)
        }
        let ha = ContentHash.compute(url: a, size: 19)
        let hb = ContentHash.compute(url: b, size: 19)
        #expect(ha != nil)
        #expect(ha == hb)
        #expect(ha?.count == 32)
    }

    @Test("Different bytes hash differently")
    func differentDiffer() throws {
        let a = try tmp(Array("alpha".utf8))
        let b = try tmp(Array("bravo".utf8))
        defer {
            try? FileManager.default.removeItem(at: a)
            try? FileManager.default.removeItem(at: b)
        }
        #expect(ContentHash.compute(url: a, size: 5) != ContentHash.compute(url: b, size: 5))
    }

    @Test("Composite path is deterministic and differs from the full hash")
    func compositeDeterministic() throws {
        let body = (0..<4096).map { UInt8($0 % 251) }
        let p = try tmp(body)
        defer { try? FileManager.default.removeItem(at: p) }
        let size = UInt64(body.count)
        let c1 = ContentHash.compute(url: p, size: size, fullMax: 64)
        let c2 = ContentHash.compute(url: p, size: size, fullMax: 64)
        #expect(c1 != nil)
        #expect(c1 == c2)
        let full = ContentHash.compute(url: p, size: size, fullMax: .max)
        #expect(c1 != full)
    }

    @Test("Composite catches a differing edge byte")
    func compositeEdge() throws {
        var a = [UInt8](repeating: 7, count: 4096)
        var b = [UInt8](repeating: 7, count: 4096)
        a[0] = 1            // head differs
        b[4095] = 2         // tail differs
        let pa = try tmp(a)
        let pb = try tmp(b)
        defer {
            try? FileManager.default.removeItem(at: pa)
            try? FileManager.default.removeItem(at: pb)
        }
        #expect(ContentHash.compute(url: pa, size: 4096, fullMax: 64)
                != ContentHash.compute(url: pb, size: 4096, fullMax: 64))
    }
}
