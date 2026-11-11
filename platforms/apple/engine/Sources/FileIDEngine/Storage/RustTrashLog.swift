import CryptoKit
import Foundation

/// Dual-write of each Trash batch in the shared Rust engine's canonical journal
/// (`trash_log.json`: NDJSON lines `{json}\t{hex HMAC-SHA256}` keyed by
/// `log-hmac.key`, both in the state folder), so a batch made here restores
/// after the app switches to the Rust engine. Kept for one release
/// (DECISIONS 2026-11-10). Batches a kill interrupted are still covered: the
/// Rust engine imports `trash-journal.json` once at startup.
///
/// The other direction, for a switch back to this engine: `find` reads a batch
/// the Rust engine wrote (signature checked with the shared key, the log never
/// modified), so `restoreFromTrash` puts it back with its catalog snapshot.
enum RustTrashLog {
    struct Identity: Codable, Equatable {
        var volume: UInt64
        var file: UInt64
    }

    struct Item: Codable, Equatable {
        var fileID: Int64
        var originalPath: String
        var sourceIdentity: Identity?
        var trashedPath: String?
        var rows: [TrashJournal.TableRow]

        enum CodingKeys: String, CodingKey {
            case fileID = "file_id", originalPath = "original_path", sourceIdentity = "source_identity"
            case trashedPath = "trashed_path", rows
        }
    }

    struct Entry: Codable, Equatable {
        var batchID: String
        var timestamp: Double
        var items: [Item]

        enum CodingKeys: String, CodingKey { case batchID = "batch_id", timestamp, items }
    }

    static func url(beside journal: URL) -> URL {
        journal.deletingLastPathComponent().appendingPathComponent("trash_log.json")
    }

    /// The Rust engine's identity for a path: `lstat` device and inode, the
    /// device widened the way Rust's `MetadataExt::dev` widens it.
    static func identity(of url: URL) -> Identity? {
        var info = stat()
        guard lstat(url.path, &info) == 0 else { return nil }
        return Identity(volume: UInt64(bitPattern: Int64(info.st_dev)), file: UInt64(info.st_ino))
    }

    /// The shared key, created (32 random bytes, 0600) when neither engine has made it yet.
    static func key(in folder: URL) throws -> Data {
        let url = folder.appendingPathComponent("log-hmac.key")
        if let existing = FileManager.default.contents(atPath: url.path), !existing.isEmpty { return existing }
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw CocoaError(.fileWriteUnknown)
        }
        let fd = open(url.path, O_WRONLY | O_CREAT | O_EXCL, 0o600)
        if fd < 0 {
            // Created by someone else in between: use theirs.
            if let existing = FileManager.default.contents(atPath: url.path), !existing.isEmpty { return existing }
            throw CocoaError(.fileWriteUnknown)
        }
        defer { close(fd) }
        let written = bytes.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
        guard written == bytes.count, fsync(fd) == 0 else { throw CocoaError(.fileWriteUnknown) }
        return Data(bytes)
    }

    static func line(_ entry: Entry, key: Data) throws -> Data {
        let json = try JSONEncoder().encode(entry)
        let mac = HMAC<SHA256>.authenticationCode(for: json, using: SymmetricKey(data: key))
        var line = json
        line.append(contentsOf: Array("\t".utf8))
        line.append(contentsOf: Array(Data(mac).map { String(format: "%02x", $0) }.joined().utf8))
        line.append(0x0A)
        return line
    }

    /// Appends one signed entry and syncs it, as `trash_log::append` does.
    static func append(_ entry: Entry, to url: URL) throws {
        let data = try line(entry, key: try key(in: url.deletingLastPathComponent()))
        let fd = open(url.path, O_WRONLY | O_CREAT | O_APPEND, 0o600)
        guard fd >= 0 else { throw CocoaError(.fileWriteNoPermission) }
        defer { close(fd) }
        let written = data.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
        guard written == data.count, fsync(fd) == 0 else { throw CocoaError(.fileWriteUnknown) }
    }

    static func entry(for batch: TrashJournal.Batch, identities: [Int64: Identity]) -> Entry {
        Entry(batchID: batch.id, timestamp: batch.createdAt, items: batch.items.map { item in
            Item(fileID: item.fileID, originalPath: item.originalPath, sourceIdentity: identities[item.fileID],
                 trashedPath: item.trashedPath.isEmpty ? nil : item.trashedPath, rows: item.rows)
        })
    }

    /// What `find` reads: other platforms' fields are ignored, and items
    /// without a snapshot or Trash location still decode.
    private struct LoggedItem: Decodable {
        var fileID: Int64
        var originalPath: String
        var trashedPath: String?
        var rows: [TrashJournal.TableRow]?

        enum CodingKeys: String, CodingKey {
            case fileID = "file_id", originalPath = "original_path", trashedPath = "trashed_path", rows
        }
    }

    private struct LoggedEntry: Decodable {
        var batchID: String
        var timestamp: Double
        var items: [LoggedItem]

        enum CodingKeys: String, CodingKey { case batchID = "batch_id", timestamp, items }
    }

    /// The newest signed entry for `batchID` in the Rust log beside `journal`,
    /// as a Swift batch; nil without the shared key or a valid entry.
    static func find(_ batchID: String, beside journal: URL) -> TrashJournal.Batch? {
        let folder = journal.deletingLastPathComponent()
        guard let key = FileManager.default.contents(atPath: folder.appendingPathComponent("log-hmac.key").path),
              !key.isEmpty,
              let raw = FileManager.default.contents(atPath: url(beside: journal).path) else { return nil }
        let symmetric = SymmetricKey(data: key)
        var found: LoggedEntry?
        for line in raw.split(separator: 0x0A) {
            guard let tab = line.firstIndex(of: 0x09) else { continue }
            let payload = Data(line[line.startIndex..<tab])
            let mac = String(decoding: line[line.index(after: tab)...], as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            guard let macBytes = hexBytes(mac),
                  HMAC<SHA256>.isValidAuthenticationCode(macBytes, authenticating: payload, using: symmetric),
                  let entry = try? JSONDecoder().decode(LoggedEntry.self, from: payload),
                  entry.batchID == batchID else { continue }
            found = entry
        }
        guard let found else { return nil }
        return TrashJournal.Batch(id: found.batchID, createdAt: found.timestamp, items: found.items.map {
            TrashJournal.Item(fileID: $0.fileID, originalPath: $0.originalPath,
                              trashedPath: $0.trashedPath ?? "", rows: $0.rows ?? [])
        })
    }

    private static func hexBytes(_ text: String) -> Data? {
        guard text.count == 64 else { return nil }
        var bytes = Data(capacity: 32)
        var index = text.startIndex
        while index < text.endIndex {
            let next = text.index(index, offsetBy: 2)
            guard let byte = UInt8(text[index..<next], radix: 16) else { return nil }
            bytes.append(byte)
            index = next
        }
        return bytes
    }
}
