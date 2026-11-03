import Foundation
import GRDB
import FileIDShared

/// Undo for engine-side Trash moves. Each `trashFiles` batch records where
/// every file landed in the Trash plus a snapshot of its catalog rows (file,
/// CLIP vector, tags, take signals), so `restoreFromTrash` puts the file back
/// and the catalog exactly as it was: a restored burst regroups at once
/// instead of waiting for a rescan.
enum TrashJournal {
    struct Value: Codable, Equatable {
        var int: Int64?
        var real: Double?
        var text: String?
        var blob: Data?
    }

    struct TableRow: Codable, Equatable {
        var table: String
        var columns: [String: Value]
    }

    struct Item: Codable, Equatable {
        var fileID: Int64
        var originalPath: String
        var trashedPath: String
        var rows: [TableRow]
    }

    struct Batch: Codable, Equatable {
        var id: String
        var createdAt: Double
        var items: [Item]
    }

    static let snapshotTables = ["files": "id", "clip_embeddings": "file_id", "tags": "file_id", "take_quality": "file_id"]
    static let maxBatches = 50
    private static let lock = NSLock()

    static var defaultURL: URL { EngineStateRoot.directory.appendingPathComponent("trash-journal.json") }

    static func snapshot(_ db: GRDB.Database, fileID: Int64) throws -> [TableRow] {
        var out: [TableRow] = []
        for table in ["files", "clip_embeddings", "tags", "take_quality"] {
            guard let key = snapshotTables[table], try db.tableExists(table) else { continue }
            for row in try Row.fetchAll(db, sql: "SELECT * FROM \(table) WHERE \(key)=?", arguments: [fileID]) {
                var columns: [String: Value] = [:]
                for (name, value) in row {
                    switch value.storage {
                    case .null: columns[name] = Value()
                    case .int64(let v): columns[name] = Value(int: v)
                    case .double(let v): columns[name] = Value(real: v)
                    case .string(let v): columns[name] = Value(text: v)
                    case .blob(let v): columns[name] = Value(blob: v)
                    }
                }
                out.append(TableRow(table: table, columns: columns))
            }
        }
        return out
    }

    static func restoreRows(_ db: GRDB.Database, _ rows: [TableRow]) throws {
        for row in rows {
            guard snapshotTables[row.table] != nil, try db.tableExists(row.table) else { continue }
            let existing = Set(try db.columns(in: row.table).map(\.name))
            let names = row.columns.keys.filter { existing.contains($0) }.sorted()
            guard !names.isEmpty else { continue }
            let values: [DatabaseValueConvertible?] = names.map { name in
                let v = row.columns[name]!
                if let i = v.int { return i }
                if let r = v.real { return r }
                if let t = v.text { return t }
                if let b = v.blob { return b }
                return nil
            }
            let columns = names.map { "\"\($0)\"" }.joined(separator: ",")
            let marks = names.map { _ in "?" }.joined(separator: ",")
            try db.execute(sql: "INSERT OR IGNORE INTO \(row.table)(\(columns)) VALUES(\(marks))", arguments: StatementArguments(values))
        }
    }

    static func load(at url: URL = defaultURL) -> [Batch] {
        guard let data = FileManager.default.contents(atPath: url.path) else { return [] }
        return (try? JSONDecoder().decode([Batch].self, from: data)) ?? []
    }

    static func save(_ batches: [Batch], at url: URL = defaultURL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(Array(batches.suffix(maxBatches))).write(to: url, options: [.atomic])
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
    }

    static func append(_ batch: Batch, at url: URL = defaultURL) throws {
        lock.lock(); defer { lock.unlock() }
        try save(load(at: url) + [batch], at: url)
    }

    static func take(_ id: String, at url: URL = defaultURL) -> Batch? {
        lock.lock(); defer { lock.unlock() }
        var batches = load(at: url)
        guard let index = batches.firstIndex(where: { $0.id == id }) else { return nil }
        let batch = batches.remove(at: index)
        try? save(batches, at: url)
        return batch
    }

    static func putBack(_ batch: Batch, at url: URL = defaultURL) {
        lock.lock(); defer { lock.unlock() }
        try? save(load(at: url) + [batch], at: url)
    }

    /// Moves each file back from the Trash and re-inserts its catalog rows.
    /// Items that cannot be restored stay in the journal for a later retry.
    static func restore(batchID: String, database: Database, journal: URL = defaultURL,
                        move: (URL, URL) throws -> Void = { try FileManager.default.moveItem(at: $0, to: $1) }) async -> BulkActionResult {
        guard let batch = take(batchID, at: journal) else {
            return BulkActionResult(action: "restoreFromTrash", succeeded: 0, failed: 1,
                                    messages: [BulkActionItem(ok: false, message: "Nothing to put back: this Trash batch is unknown or already restored.")])
        }
        var messages: [BulkActionItem] = []
        var leftover: [Item] = []
        for item in batch.items {
            let original = URL(fileURLWithPath: item.originalPath), trashed = URL(fileURLWithPath: item.trashedPath)
            guard !FileManager.default.fileExists(atPath: original.path) else {
                messages.append(BulkActionItem(fileID: item.fileID, ok: false, message: "Cannot restore: another file is already at \(original.lastPathComponent)."))
                continue
            }
            guard FileManager.default.fileExists(atPath: trashed.path) else {
                messages.append(BulkActionItem(fileID: item.fileID, ok: false, message: "\(original.lastPathComponent) is no longer in the Trash."))
                continue
            }
            do {
                try FileManager.default.createDirectory(at: original.deletingLastPathComponent(), withIntermediateDirectories: true)
                try move(trashed, original)
            } catch {
                leftover.append(item)
                messages.append(BulkActionItem(fileID: item.fileID, ok: false, message: "Could not put \(original.lastPathComponent) back."))
                continue
            }
            do {
                try await database.pool.write { db in
                    let taken = try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM files WHERE path_text=? OR id=?)",
                                                  arguments: [item.originalPath, item.fileID]) ?? false
                    if !taken { try restoreRows(db, item.rows) }
                }
                messages.append(BulkActionItem(fileID: item.fileID, ok: true, message: item.originalPath))
            } catch {
                messages.append(BulkActionItem(fileID: item.fileID, ok: false, message: "Restored the file, but the catalog update failed; rescan the folder."))
            }
        }
        if !leftover.isEmpty { putBack(Batch(id: batch.id, createdAt: batch.createdAt, items: leftover), at: journal) }
        return BulkActionResult(action: "restoreFromTrash", succeeded: messages.filter(\.ok).count,
                                failed: messages.filter { !$0.ok }.count, messages: messages)
    }
}
