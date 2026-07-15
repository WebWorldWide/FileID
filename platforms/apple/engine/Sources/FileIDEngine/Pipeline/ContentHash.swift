import Foundation
import CryptoKit

/// Fast persisted content identity for rename healing and duplicate candidate
/// hints. Files above 16 MB use samples and therefore require a separate live
/// full-file digest before byte-exact display or destructive action.
enum ContentHash {
    /// Files at or below this size are hashed in full; larger files use the
    /// head+interior+tail+size composite. Matches Windows `FULL_HASH_MAX_BYTES`.
    static let fullHashMaxBytes: UInt64 = 16 * 1024 * 1024
    private static let chunk = 1024 * 1024          // 1 MB head/tail span (Windows CHUNK)
    private static let interiorSamples: UInt64 = 4   // Windows INTERIOR_SAMPLES
    private static let interiorChunk = 64 * 1024     // 64 KB (Windows INTERIOR_CHUNK)

    /// 32-byte SHA-256 full or sampled identity for `url`, or nil on I/O error.
    /// Same bytes always match; sampled matches are not proof of byte equality.
    static func compute(url: URL, size: UInt64) -> Data? {
        compute(url: url, size: size, fullMax: fullHashMaxBytes)
    }

    /// Testable core with the full-vs-composite threshold injected so the
    /// composite branch can be exercised on small fixtures (mirrors the Windows
    /// engine's `hash_with_threshold`).
    static func compute(url: URL, size: UInt64, fullMax: UInt64) -> Data? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        var hasher = SHA256()
        do {
            if size <= fullMax {
                while let block = try handle.read(upToCount: chunk), !block.isEmpty {
                    hasher.update(data: block)
                }
            } else {
                // Clamp the head/tail span to the file size; head+tail overlap on
                // a file barely above the threshold is harmless (still deterministic).
                let span = min(size, UInt64(chunk))
                let spanInt = Int(span)
                hasher.update(data: try readFill(handle, spanInt))                 // head
                // Interior samples — deterministic evenly-spaced 64 KB chunks so two
                // DISTINCT same-size files sharing head+tail (camera bursts, padded
                // containers) don't collide. Skipped when they'd overlap head/tail.
                for k in 1...interiorSamples {
                    let off = (size &* k) / (interiorSamples + 1)
                    if off < span || off &+ UInt64(interiorChunk) > size &- span { continue }
                    try handle.seek(toOffset: off)
                    hasher.update(data: try readFill(handle, interiorChunk))
                }
                try handle.seek(toOffset: size &- span)
                hasher.update(data: try readFill(handle, spanInt))                 // tail
                // size_le disambiguates files sharing head+tail but differing inside.
                var le = size.littleEndian
                hasher.update(data: Data(bytes: &le, count: MemoryLayout<UInt64>.size))
            }
        } catch {
            return nil
        }
        return Data(hasher.finalize())
    }

    /// Read exactly `count` bytes (or until EOF). A single `read(upToCount:)` may
    /// return fewer bytes than asked even mid-file, so loop — mirrors the Windows
    /// engine's `read_fill`.
    private static func readFill(_ handle: FileHandle, _ count: Int) throws -> Data {
        var out = Data()
        out.reserveCapacity(count)
        while out.count < count {
            guard let part = try handle.read(upToCount: count - out.count), !part.isEmpty else { break }
            out.append(part)
        }
        return out
    }
}
