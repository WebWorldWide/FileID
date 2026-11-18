import Foundation
import GRDB
import FileIDShared

/// Undo for engine-side Trash moves. Each `trashFiles` batch records where
/// every file landed in the Trash plus a snapshot of its catalog rows (file,
/// CLIP vector, tags, take signals, faces), so `restoreFromTrash` puts the file back
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
        /// The library root that contained the file when it was trashed. Never stored in the
        /// unsigned Swift journal: only the value read from a signature-checked Rust log line
        /// is trusted, and only to authorize a put back after the catalog lost its roots.
        var libraryRoot: String?

        enum CodingKeys: String, CodingKey { case fileID, originalPath, trashedPath, rows }
    }

    struct Batch: Codable, Equatable {
        var id: String
        var createdAt: Double
        var items: [Item]
    }

    /// `path` with the longest existing ancestor resolved through symlinks, so a file that is
    /// gone still compares against the realpath form of the roots.
    static func canonicalPath(_ path: String) -> String {
        var tail: [String] = []
        var head = URL(fileURLWithPath: path).standardizedFileURL.path
        while true {
            if let resolved = realpath(head, nil) {
                defer { free(resolved) }
                return ([String(cString: resolved)] + tail.reversed()).joined(separator: "/").replacingOccurrences(of: "//", with: "/")
            }
            guard head != "/" else { return path }
            tail.append((head as NSString).lastPathComponent)
            head = (head as NSString).deletingLastPathComponent
        }
    }

    static func isInside(_ path: String, root: String) -> Bool {
        root == "/" || path == root || path.hasPrefix(root + "/")
    }

    /// The catalog's scanned roots, canonical.
    static func catalogRoots(_ db: GRDB.Database) throws -> [String] {
        try String.fetchAll(db, sql: "SELECT path FROM library_roots").map(canonicalPath)
    }

    /// The innermost of `roots` (canonical) containing `path`.
    static func containingRoot(_ roots: [String], of path: String) -> String? {
        let candidate = canonicalPath(path)
        return roots.filter { isInside(candidate, root: $0) }.max { $0.split(separator: "/").count < $1.split(separator: "/").count }
    }

    /// Whether a put back may write `original`: inside a current catalog root, or inside the
    /// signed recorded root of the item, which must still exist.
    static func putBackAllowed(original: String, recordedRoot: String?, catalogRoots: [String]) -> Bool {
        var roots = catalogRoots
        if let recordedRoot, let resolved = realpath(recordedRoot, nil) {
            roots.append(String(cString: resolved))
            free(resolved)
        }
        let candidate = canonicalPath(original)
        return roots.contains { isInside(candidate, root: $0) }
    }

    static let snapshotTables = ["files": "id", "clip_embeddings": "file_id", "tags": "file_id", "take_quality": "file_id", "face_prints": "file_id"]
    static let maxBatches = 50
    private static let lock = NSLock()

    static var defaultURL: URL { EngineStateRoot.directory.appendingPathComponent("trash-journal.json") }

    static func snapshot(_ db: GRDB.Database, fileID: Int64) throws -> [TableRow] {
        var out: [TableRow] = []
        for table in ["files", "clip_embeddings", "tags", "take_quality", "face_prints"] {
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

    /// Re-inserts the snapshot rows of file `fileID`. A face whose person was deleted since
    /// returns unassigned; the persons that got a face back are recounted.
    static func restoreRows(_ db: GRDB.Database, _ rows: [TableRow], fileID: Int64) throws {
        var restoredPersons = Set<Int64>()
        for row in rows {
            guard let key = snapshotTables[row.table], try db.tableExists(row.table) else { continue }
            guard row.columns[key]?.int == fileID else { throw TrashRestoreError.rowOfAnotherFile(row.table) }
            let existing = Set(try db.columns(in: row.table).map(\.name))
            let names = row.columns.keys.filter { existing.contains($0) }.sorted()
            guard !names.isEmpty else { continue }
            var facePerson: Int64?
            let values: [DatabaseValueConvertible?] = try names.map { name in
                let v = row.columns[name]!
                if row.table == "face_prints", name == "person_id", let person = v.int {
                    guard try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM persons WHERE id=?)", arguments: [person]) == true else { return nil }
                    facePerson = person
                    return person
                }
                if let i = v.int { return i }
                if let r = v.real { return r }
                if let t = v.text { return t }
                if let b = v.blob { return b }
                return nil
            }
            let columns = names.map { "\"\($0)\"" }.joined(separator: ",")
            let marks = names.map { _ in "?" }.joined(separator: ",")
            try db.execute(sql: "INSERT OR IGNORE INTO \(row.table)(\(columns)) VALUES(\(marks))", arguments: StatementArguments(values))
            if db.changesCount > 0, let person = facePerson { restoredPersons.insert(person) }
        }
        try FileIDEngineMain.reconcilePersons(restoredPersons, db: db)
    }

    enum TrashRestoreError: Error { case rowOfAnotherFile(String) }

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
        if let test = EngineTrash.folders(for: original) { return test }
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
        // After a rollback from the Rust engine: its log is read-only here, with the same proof rules.
        for batch in RustTrashLog.verifiedBatches(beside: url) {
            for item in batch.items {
                guard !FileManager.default.fileExists(atPath: item.originalPath),
                      locate(item, in: folders) != nil else { continue }
                staleRows.append((item.fileID, item.originalPath))
            }
        }
        if !staleRows.isEmpty {
            let rows = staleRows
            try? await database.pool.write { db in
                for (id, path) in rows {
                    let persons = try PersonCounts.persons(ofFiles: [id], db)
                    try db.execute(sql: "DELETE FROM files WHERE id = ? AND path_text = ?", arguments: [id, path])
                    if db.changesCount > 0 { try FileIDEngineMain.reconcilePersons(persons, db: db) }
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
        // A batch the Rust engine made, after a switch back to this engine.
        let ownBatch = find(batchID, at: journal)
        guard let batch = ownBatch ?? RustTrashLog.find(batchID, beside: journal) else {
            return BulkActionResult(action: "restoreFromTrash", succeeded: 0, failed: 1,
                                    messages: [BulkActionItem(ok: false, message: "Nothing to put back: this Trash batch is unknown or already restored.")])
        }
        var messages: [BulkActionItem] = []
        var leftover: [Item] = []
        let currentRoots = ownBatch == nil ? ((try? await database.pool.read { try catalogRoots($0) }) ?? []) : []
        for item in batch.items {
            let original = URL(fileURLWithPath: item.originalPath)
            if ownBatch == nil, !putBackAllowed(original: item.originalPath, recordedRoot: item.libraryRoot, catalogRoots: currentRoots) {
                messages.append(BulkActionItem(fileID: item.fileID, ok: false,
                                               message: "Cannot restore \(original.lastPathComponent): it is outside the folders FileID has scanned."))
                continue
            }
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
                    if !taken { try restoreRows(db, item.rows, fileID: item.fileID) }
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
