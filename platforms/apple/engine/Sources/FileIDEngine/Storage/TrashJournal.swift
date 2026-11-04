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

    /// Replaces the batch with the same id, or appends it. A trash batch is rewritten after
    /// every file so a killed engine leaves each moved file restorable.
    static func upsert(_ batch: Batch, at url: URL = defaultURL) throws {
        lock.lock(); defer { lock.unlock() }
        try save(load(at: url).filter { $0.id != batch.id } + [batch], at: url)
    }

    static func find(_ id: String, at url: URL = defaultURL) -> Batch? {
        lock.lock(); defer { lock.unlock() }
        return load(at: url).first { $0.id == id }
    }

    /// Drops the batch once a restore pass is over, keeping only the items still to retry.
    static func finish(_ batch: Batch, leftover: [Item], at url: URL = defaultURL) {
        lock.lock(); defer { lock.unlock() }
        let rest = load(at: url).filter { $0.id != batch.id }
        try? save(leftover.isEmpty ? rest : rest + [Batch(id: batch.id, createdAt: batch.createdAt, items: leftover)], at: url)
    }

    static func fileRef(_ item: Item) -> Int64? {
        item.rows.first(where: { $0.table == "files" })?.columns["file_ref"]?.int
    }

    /// The Trash folders a file from `original` can land in: the user's Trash, and for other
    /// volumes that volume's `.Trashes/<uid>`.
    static func trashFolders(for original: URL) -> [URL] {
        var folders = [UserHome.trashURL]
        if let volume = (try? original.deletingLastPathComponent().resourceValues(forKeys: [.volumeURLKey]))?.volume,
           volume.path != "/" {
            folders.append(volume.appendingPathComponent(".Trashes/\(getuid())", isDirectory: true))
        }
        return folders
    }

    /// Where the item is in the Trash now: its recorded location, or for a write-ahead entry
    /// (killed before the location was saved) the Trash entry with the file's own inode.
    static func locate(_ item: Item, in folders: [URL]? = nil) -> URL? {
        if !item.trashedPath.isEmpty, FileManager.default.fileExists(atPath: item.trashedPath) {
            return URL(fileURLWithPath: item.trashedPath)
        }
        guard let ref = fileRef(item) else { return nil }
        let original = URL(fileURLWithPath: item.originalPath)
        let stem = original.deletingPathExtension().lastPathComponent
        for folder in folders ?? trashFolders(for: original) {
            guard let names = try? FileManager.default.contentsOfDirectory(atPath: folder.path) else { continue }
            // Finder keeps the name or appends a counter ("photo 2.jpg"); only those are inode-checked.
            for name in names where name.hasPrefix(stem) {
                let candidate = folder.appendingPathComponent(name)
                if Restructure.fileRefMatches(dbRef: ref, currentRef: Discovery.inode(of: candidate)) { return candidate }
            }
        }
        return nil
    }

    /// Startup repair after a kill mid-`trashFiles`: completes write-ahead entries, drops catalog
    /// rows of files that are provably in the Trash, and forgets entries for files that never left.
    static func reconcile(database: Database, journal url: URL = defaultURL, trashFolders folders: [URL]? = nil) async {
        let batches = loadLocked(at: url)
        guard !batches.isEmpty else { return }
        var changed = false
        var repaired: [Batch] = []
        var staleRows: [(Int64, String)] = []
        for var batch in batches {
            var items: [Item] = []
            for var item in batch.items {
                let original = URL(fileURLWithPath: item.originalPath)
                if let found = locate(item, in: folders) {
                    if found.path != item.trashedPath { item.trashedPath = found.path; changed = true }
                    if !FileManager.default.fileExists(atPath: original.path) { staleRows.append((item.fileID, item.originalPath)) }
                    items.append(item)
                } else if item.trashedPath.isEmpty, Restructure.fileRefMatches(dbRef: fileRef(item), currentRef: Discovery.inode(of: original)) {
                    changed = true
                } else {
                    items.append(item)
                }
            }
            batch.items = items
            if !items.isEmpty { repaired.append(batch) } else { changed = true }
        }
        if !staleRows.isEmpty {
            let rows = staleRows
            try? await database.pool.write { db in
                for (id, path) in rows {
                    try db.execute(sql: "DELETE FROM files WHERE id = ? AND path_text = ?", arguments: [id, path])
                }
            }
        }
        if changed { replace(batches.map(\.id), with: repaired, at: url) }
    }

    private static func loadLocked(at url: URL) -> [Batch] {
        lock.lock(); defer { lock.unlock() }
        return load(at: url)
    }

    /// Swaps the reconciled batches in, keeping any batch written since they were read.
    private static func replace(_ ids: [String], with repaired: [Batch], at url: URL) {
        lock.lock(); defer { lock.unlock() }
        let byID = Dictionary(repaired.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        let known = Set(ids)
        try? save(load(at: url).compactMap { known.contains($0.id) ? byID[$0.id] : $0 }, at: url)
    }

    /// A killed restore can leave a file home with its rows not yet re-inserted; the
    /// snapshot's inode proves the file at the original path is that same file.
    static func alreadyRestored(_ item: Item, original: URL, trashed: URL) -> Bool {
        guard !FileManager.default.fileExists(atPath: trashed.path),
              let ref = item.rows.first(where: { $0.table == "files" })?.columns["file_ref"]?.int else { return false }
        return Restructure.fileRefMatches(dbRef: ref, currentRef: Discovery.inode(of: original))
    }

    /// Moves each file back from the Trash and re-inserts its catalog rows.
    /// The batch stays journaled until the pass ends, so a killed restore can be retried;
    /// items that cannot be restored stay in the journal for a later retry.
    static func restore(batchID: String, database: Database, journal: URL = defaultURL,
                        move: (URL, URL) throws -> Void = { try FileManager.default.moveItem(at: $0, to: $1) }) async -> BulkActionResult {
        guard let batch = find(batchID, at: journal) else {
            return BulkActionResult(action: "restoreFromTrash", succeeded: 0, failed: 1,
                                    messages: [BulkActionItem(ok: false, message: "Nothing to put back: this Trash batch is unknown or already restored.")])
        }
        var messages: [BulkActionItem] = []
        var leftover: [Item] = []
        for item in batch.items {
            let original = URL(fileURLWithPath: item.originalPath)
            let trashed = locate(item) ?? URL(fileURLWithPath: item.trashedPath.isEmpty ? "/nonexistent-\(UUID().uuidString)" : item.trashedPath)
            if FileManager.default.fileExists(atPath: original.path) {
                guard alreadyRestored(item, original: original, trashed: trashed) else {
                    messages.append(BulkActionItem(fileID: item.fileID, ok: false, message: "Cannot restore: another file is already at \(original.lastPathComponent)."))
                    continue
                }
            } else {
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
        finish(batch, leftover: leftover, at: journal)
        return BulkActionResult(action: "restoreFromTrash", succeeded: messages.filter(\.ok).count,
                                failed: messages.filter { !$0.ok }.count, messages: messages)
    }
}
