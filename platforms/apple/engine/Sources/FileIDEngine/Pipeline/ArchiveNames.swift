import Foundation

/// The names of the files inside a zip, so a search for a file finds the archive
/// that holds it. Reads only the zip's central directory (its table of contents at
/// the end of the file); nothing is extracted or decompressed.
enum ArchiveNames {
    static let maxNamesBytes = 256 * 1024
    private static let maxDirectoryBytes = 32 * 1024 * 1024

    static func list(url: URL) -> String? {
        guard url.pathExtension.lowercased() == "zip",
              let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        guard let size = try? handle.seekToEnd(), size >= 22 else { return nil }
        // The end-of-central-directory record sits in the last 22 bytes plus an optional comment (≤ 64 KiB).
        let tailLength = min(size, 22 + 65_535)
        guard (try? handle.seek(toOffset: size - tailLength)) != nil,
              let tail = try? handle.read(upToCount: Int(tailLength)), tail.count == Int(tailLength) else { return nil }
        let bytes = [UInt8](tail)
        guard let eocd = stride(from: bytes.count - 22, through: 0, by: -1).first(where: {
            bytes[$0] == 0x50 && bytes[$0 + 1] == 0x4B && bytes[$0 + 2] == 0x05 && bytes[$0 + 3] == 0x06
        }) else { return nil }
        let entries = Int(le16(bytes, eocd + 10)), length = UInt64(le32(bytes, eocd + 12)), offset = UInt64(le32(bytes, eocd + 16))
        // ZIP64 archives store 0xFFFFFFFF here; they are rare for user archives and left unlisted.
        guard length > 0, length <= UInt64(maxDirectoryBytes), offset != 0xFFFF_FFFF,
              offset + length <= size - tailLength + UInt64(eocd),
              (try? handle.seek(toOffset: offset)) != nil,
              let directory = try? handle.read(upToCount: Int(length)), directory.count == Int(length) else { return nil }
        let dir = [UInt8](directory)
        var names: [String] = [], total = 0, at = 0
        for _ in 0..<entries {
            guard at + 46 <= dir.count, dir[at] == 0x50, dir[at + 1] == 0x4B, dir[at + 2] == 0x01, dir[at + 3] == 0x02 else { break }
            let nameLength = Int(le16(dir, at + 28)), extra = Int(le16(dir, at + 30)), comment = Int(le16(dir, at + 32))
            guard at + 46 + nameLength <= dir.count else { break }
            let name = String(decoding: dir[(at + 46)..<(at + 46 + nameLength)], as: UTF8.self)
            at += 46 + nameLength + extra + comment
            guard !name.hasSuffix("/"), !name.hasPrefix("__MACOSX/"), !name.hasPrefix("."), !name.contains("/.") else { continue }
            total += name.utf8.count + 1
            if total > maxNamesBytes { break }
            names.append(name)
        }
        return names.isEmpty ? nil : names.joined(separator: "\n")
    }

    private static func le16(_ b: [UInt8], _ i: Int) -> UInt16 { UInt16(b[i]) | UInt16(b[i + 1]) << 8 }
    private static func le32(_ b: [UInt8], _ i: Int) -> UInt32 {
        UInt32(b[i]) | UInt32(b[i + 1]) << 8 | UInt32(b[i + 2]) << 16 | UInt32(b[i + 3]) << 24
    }
}
