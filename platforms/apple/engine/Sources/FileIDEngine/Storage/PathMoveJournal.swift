import Foundation
import GRDB
import FileIDShared

/// Write-ahead log for engine-side file moves (Library renames). An intent is appended
/// before the move and a matching "done" after the catalog row is updated, so a kill in
/// between leaves a record; startup points the row at the new path when the file there
/// is provably the same one (same inode). Append-only NDJSON, cleared after each repair.
enum PathMoveJournal {
    struct Intent: Codable, Equatable {
        var id: String
        var fileID: Int64
        var from: String
        var to: String
        var fileRef: Int64?
    }

    private struct Line: Codable {
        var intent: Intent?
        var done: String?
    }

    static var defaultURL: URL { EngineStateRoot.directory.appendingPathComponent("path-moves.ndjson") }
    private static let lock = NSLock()

    @discardableResult
    static func begin(fileID: Int64, from: String, to: String, fileRef: UInt64?, at url: URL = defaultURL) -> String {
        let intent = Intent(id: UUID().uuidString, fileID: fileID, from: from, to: to, fileRef: fileRef.map { Int64(bitPattern: $0) })
        append(Line(intent: intent), at: url)
        return intent.id
    }

    static func finish(_ id: String, at url: URL = defaultURL) {
        append(Line(done: id), at: url)
    }

    static func pending(at url: URL = defaultURL) -> [Intent] {
        lock.lock(); defer { lock.unlock() }
        return unfinished(at: url)
    }

    static func reconcile(database: Database, at url: URL = defaultURL) async {
        let intents = pending(at: url)
        if !intents.isEmpty {
            let repairs = intents.filter { intent in
                !FileManager.default.fileExists(atPath: intent.from)
                    && Restructure.fileRefMatches(dbRef: intent.fileRef, currentRef: Discovery.inode(of: URL(fileURLWithPath: intent.to)))
            }
            try? await database.pool.write { db in
                for intent in repairs { try updatePath(db, fileID: intent.fileID, from: intent.from, to: intent.to) }
            }
        }
        clear(at: url)
    }

    /// Points one catalog row at its new path; a no-op unless the row still names `from`.
    static func updatePath(_ db: GRDB.Database, fileID: Int64, from: String, to: String) throws {
        let ext = URL(fileURLWithPath: to).pathExtension.lowercased()
        try db.execute(
            sql: "UPDATE OR ABORT files SET path_text = ?, path_hash = ?, path_search = ?, extension = ? WHERE id = ? AND path_text = ?",
            arguments: [to, StablePathHash.hash(to), to.precomposedStringWithCanonicalMapping,
                        ext.isEmpty ? nil : ext, fileID, from])
    }

    private static func append(_ line: Line, at url: URL) {
        guard var data = try? JSONEncoder().encode(line) else { return }
        data.append(0x0A)
        lock.lock(); defer { lock.unlock() }
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil, attributes: [.posixPermissions: 0o600])
        }
        guard let handle = try? FileHandle(forWritingTo: url) else { return }
        defer { try? handle.close() }
        _ = try? handle.seekToEnd()
        try? handle.write(contentsOf: data)
        try? handle.synchronize()
    }

    private static func unfinished(at url: URL) -> [Intent] {
        guard let data = FileManager.default.contents(atPath: url.path) else { return [] }
        let text = String(decoding: data, as: UTF8.self)
        var intents: [Intent] = [], done = Set<String>()
        for raw in text.split(separator: "\n") {
            guard let line = try? JSONDecoder().decode(Line.self, from: Data(raw.utf8)) else { continue }
            if let intent = line.intent { intents.append(intent) }
            if let id = line.done { done.insert(id) }
        }
        return intents.filter { !done.contains($0.id) }
    }

    private static func clear(at url: URL) {
        lock.lock(); defer { lock.unlock() }
        try? FileManager.default.removeItem(at: url)
    }
}
