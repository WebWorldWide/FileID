import CryptoKit
import Foundation

/// Dual-write of each Trash batch in the shared Rust engine's canonical journal
/// (`trash_log.json`: NDJSON lines `{json}\t{hex HMAC-SHA256}` keyed by
/// `log-hmac.key`, both in the state folder), so a batch made here restores
/// after the app switches to the Rust engine. Kept for one release
/// (DECISIONS 2026-11-10). Batches a kill interrupted are still covered: the
/// Rust engine imports `trash-journal.json` once at startup.
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
}
