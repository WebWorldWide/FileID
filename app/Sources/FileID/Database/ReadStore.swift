// Read-only DB store. Engine is the single writer (WAL); app reads
// concurrently. `version` bumps each reload so SwiftUI re-queries.
import Foundation
import GRDB
import FileIDShared

@Observable
public final class ReadStore: @unchecked Sendable {
    private var queue: DatabaseQueue?
    private let dbURL: URL
    public private(set) var version: Int = 0
    public private(set) var totalFiles: Int = 0
    public private(set) var totalImages: Int = 0
    public private(set) var totalDuplicateGroups: Int = 0
    public private(set) var totalReclaimableMB: Double = 0
    public private(set) var lastError: String?

    public init(dbURL: URL = ReadStore.defaultDBURL) {
        self.dbURL = dbURL
    }

    public static var defaultDBURL: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID/fileid.sqlite")
    }

    /// Idempotent. Safe to call after engine creates / migrates the DB.
    public func openIfPossible() {
        guard FileManager.default.fileExists(atPath: dbURL.path) else {
            self.queue = nil
            self.totalFiles = 0
            self.totalImages = 0
            self.totalDuplicateGroups = 0
            self.totalReclaimableMB = 0
            return
        }
        if queue == nil {
            do {
                var config = Configuration()
                config.readonly = true
                self.queue = try DatabaseQueue(path: dbURL.path, configuration: config)
            } catch {
                self.lastError = "Could not open DB: \(error)"
                return
            }
        }
        refreshCounters()
    }

    public func notifyChanged() {
        version &+= 1
        refreshCounters()
    }

    /// Brief writable connection for Cleanup row deletes. WAL allows this
    /// from a separate process without blocking the engine writer.
    public func deleteFiles(ids: [Int64]) -> Int {
        guard !ids.isEmpty else { return 0 }
        do {
            let queue = try DatabaseQueue(path: dbURL.path)
            let deleted = try queue.write { db -> Int in
                var total = 0
                for chunk in stride(from: 0, to: ids.count, by: 500) {
                    let slice = ids[chunk..<min(chunk + 500, ids.count)]
                    let placeholders = slice.map { _ in "?" }.joined(separator: ", ")
                    let stmt = "DELETE FROM files WHERE id IN (\(placeholders))"
                    try db.execute(sql: stmt, arguments: StatementArguments(slice))
                    total += db.changesCount
                }
                return total
            }
            self.notifyChanged()
            return deleted
        } catch {
            self.lastError = "Prune failed: \(error)"
            return 0
        }
    }

    private func refreshCounters() {
        guard let q = queue else { return }
        do {
            try q.read { db in
                self.totalFiles  = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files") ?? 0
                self.totalImages = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE kind = 'image' AND failed = 0") ?? 0

                // Duplicate groups by phash (groups of size > 1).
                let dupRows = try Row.fetchAll(db, sql: """
                    SELECT phash, COUNT(*) AS n, SUM(size_bytes) AS bytes, MAX(size_bytes) AS keeper_bytes
                    FROM files
                    WHERE phash IS NOT NULL AND phash != 0
                    GROUP BY phash
                    HAVING n > 1
                    """)
                self.totalDuplicateGroups = dupRows.count
                let reclaimableBytes: Int64 = dupRows.reduce(0) { acc, r in
                    let total: Int64 = r["bytes"] ?? 0
                    let keeper: Int64 = r["keeper_bytes"] ?? 0
                    return acc + (total - keeper)
                }
                self.totalReclaimableMB = Double(reclaimableBytes) / 1_048_576
            }
        } catch {
            self.lastError = "Counters refresh failed: \(error)"
        }
    }

    // MARK: - Library queries

    public func files(offset: Int = 0, limit: Int = 200,
                      search: String = "",
                      kindFilter: String? = nil) -> [FileRow] {
        guard let q = queue else { return [] }
        do {
            return try q.read { db in
                var sql = "SELECT * FROM files WHERE failed = 0"
                var args: StatementArguments = []
                if !search.trimmingCharacters(in: .whitespaces).isEmpty {
                    sql += " AND (id IN (SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH ?) OR path_text LIKE ?)"
                    args += [search, "%\(search)%"]
                }
                if let k = kindFilter {
                    sql += " AND kind = ?"
                    args += [k]
                }
                sql += " ORDER BY scanned_at DESC LIMIT ? OFFSET ?"
                args += [limit, offset]
                let rows = try Row.fetchAll(db, sql: sql, arguments: args)
                return rows.map { Self.toFileRow($0) }
            }
        } catch {
            self.lastError = "Library query failed: \(error)"
            return []
        }
    }

    public func tags(forFileID id: Int64) -> [String] {
        guard let q = queue else { return [] }
        return (try? q.read { db in
            try String.fetchAll(db, sql: "SELECT tag FROM tags WHERE file_id = ? ORDER BY tag", arguments: [id])
        }) ?? []
    }

    // MARK: - Cleanup queries

    /// Duplicate groups. Files within each group are sorted keeper-first.
    public func duplicateGroups() -> [DuplicateGroup] {
        guard let q = queue else { return [] }
        do {
            return try q.read { db in
                let phashes = try Row.fetchAll(db, sql: """
                    SELECT phash, COUNT(*) AS n
                    FROM files
                    WHERE phash IS NOT NULL AND phash != 0 AND failed = 0
                    GROUP BY phash
                    HAVING n > 1
                    ORDER BY n DESC
                    """)
                var groups: [DuplicateGroup] = []
                groups.reserveCapacity(phashes.count)
                for r in phashes {
                    let phash: Int64 = r["phash"] ?? 0
                    let fileRows = try Row.fetchAll(db, sql: """
                        SELECT * FROM files WHERE phash = ? AND failed = 0
                        """, arguments: [phash])
                    var files = fileRows.map { Self.toFileRow($0) }
                    // Keeper rank: aesthetic ↓, size ↓, earliest createdAt ↑, path depth ↑.
                    files.sort { a, b in
                        if (a.aesthetic ?? 0) != (b.aesthetic ?? 0) {
                            return (a.aesthetic ?? 0) > (b.aesthetic ?? 0)
                        }
                        if a.sizeBytes != b.sizeBytes { return a.sizeBytes > b.sizeBytes }
                        let ad = a.createdAt ?? .distantFuture
                        let bd = b.createdAt ?? .distantFuture
                        if ad != bd { return ad < bd }
                        return a.pathText.count < b.pathText.count
                    }
                    groups.append(DuplicateGroup(id: phash, files: files))
                }
                return groups
            }
        } catch {
            self.lastError = "Duplicate query failed: \(error)"
            return []
        }
    }

    // MARK: - Scan sessions

    public struct ScanSessionRow: Sendable, Identifiable {
        public let id: String; public let rootPath: String
        public let startedAt: Date; public let completedAt: Date?
        public let lastFileIndex: Int?; public let totalFiles: Int?
        public let status: String
    }

    public func recentSessions(limit: Int = 10) -> [ScanSessionRow] {
        guard let q = queue else { return [] }
        return (try? q.read { db in
            let rows = try Row.fetchAll(db, sql: """
                SELECT * FROM scan_sessions ORDER BY started_at DESC LIMIT ?
                """, arguments: [limit])
            return rows.map { r in
                ScanSessionRow(
                    id: r["id"], rootPath: r["root_path"],
                    startedAt: Date(timeIntervalSinceReferenceDate: r["started_at"]),
                    completedAt: (r["completed_at"] as Double?).map { Date(timeIntervalSinceReferenceDate: $0) },
                    lastFileIndex: r["last_file_index"],
                    totalFiles: r["total_files"],
                    status: r["status"]
                )
            }
        }) ?? []
    }

    // MARK: - People queries

    public struct PersonRow: Sendable, Identifiable {
        public let id: Int64
        public let name: String?            // legacy single-field, fallback for display
        public let title: String?           // e.g. "Uncle"
        public let firstName: String?
        public let middleName: String?
        public let lastName: String?
        public let suffix: String?          // e.g. "Jr"
        public let isUnknown: Bool
        public let representativeFaceID: Int64?
        public let representativeFileID: Int64?
        public let representativeBBox: String?
        public let representativePath: String?
        public let fileCount: Int
        public let faceCount: Int

        /// Structured name → legacy `name` → "Person <id>".
        public var displayName: String {
            if isUnknown { return "Unknown" }
            var parts: [String] = []
            if let t = title?.trimmingCharacters(in: .whitespaces), !t.isEmpty { parts.append(t) }
            if let f = firstName?.trimmingCharacters(in: .whitespaces), !f.isEmpty { parts.append(f) }
            if let m = middleName?.trimmingCharacters(in: .whitespaces), !m.isEmpty { parts.append(m) }
            if let l = lastName?.trimmingCharacters(in: .whitespaces), !l.isEmpty { parts.append(l) }
            if let s = suffix?.trimmingCharacters(in: .whitespaces), !s.isEmpty {
                parts.append(parts.isEmpty ? s : ", \(s)".replacingOccurrences(of: ", ", with: " "))
            }
            if !parts.isEmpty { return parts.joined(separator: " ") }
            if let n = name, !n.isEmpty { return n }
            return "Person \(id)"
        }

        /// True when any name component is set or the person is marked Unknown.
        public var hasAnyName: Bool {
            if isUnknown { return true }
            let parts = [title, firstName, middleName, lastName, suffix, name]
            return parts.contains { !($0?.trimmingCharacters(in: .whitespaces).isEmpty ?? true) }
        }
    }

    public func persons() -> [PersonRow] {
        guard let q = queue else { return [] }
        do {
            return try q.read { db in
                let rows = try Row.fetchAll(db, sql: """
                    SELECT
                      p.id, p.name, p.title, p.first_name, p.middle_name,
                      p.last_name, p.suffix, p.is_unknown,
                      p.representative_face_id, p.file_count,
                      f.bbox AS rep_bbox, f.file_id AS rep_file_id,
                      files.path_text AS rep_path,
                      (SELECT COUNT(*) FROM face_prints WHERE person_id = p.id) AS face_count
                    FROM persons p
                    LEFT JOIN face_prints f ON f.id = p.representative_face_id
                    LEFT JOIN files ON files.id = f.file_id
                    ORDER BY p.is_unknown ASC, p.file_count DESC, p.id ASC
                    """)
                return rows.map { r in
                    PersonRow(
                        id: r["id"] ?? 0,
                        name: r["name"],
                        title: r["title"],
                        firstName: r["first_name"],
                        middleName: r["middle_name"],
                        lastName: r["last_name"],
                        suffix: r["suffix"],
                        isUnknown: (r["is_unknown"] ?? 0) != 0,
                        representativeFaceID: r["representative_face_id"],
                        representativeFileID: r["rep_file_id"],
                        representativeBBox: r["rep_bbox"],
                        representativePath: r["rep_path"],
                        fileCount: r["file_count"] ?? 0,
                        faceCount: r["face_count"] ?? 0
                    )
                }
            }
        } catch {
            self.lastError = "People query failed: \(error)"
            return []
        }
    }

    public func updatePerson(id: Int64, title: String?, firstName: String?,
                             middleName: String?, lastName: String?,
                             suffix: String?, isUnknown: Bool) {
        do {
            let queue = try DatabaseQueue(path: dbURL.path)
            try queue.write { db in
                try db.execute(sql: """
                    UPDATE persons
                    SET title = ?, first_name = ?, middle_name = ?,
                        last_name = ?, suffix = ?, is_unknown = ?
                    WHERE id = ?
                    """, arguments: [
                        nilIfBlank(title), nilIfBlank(firstName),
                        nilIfBlank(middleName), nilIfBlank(lastName),
                        nilIfBlank(suffix), isUnknown ? 1 : 0, id
                    ])
            }
            self.notifyChanged()
        } catch {
            self.lastError = "Person update failed: \(error)"
        }
    }

    private func nilIfBlank(_ s: String?) -> String? {
        guard let s, !s.trimmingCharacters(in: .whitespaces).isEmpty else { return nil }
        return s.trimmingCharacters(in: .whitespaces)
    }

    public func files(forPersonID personID: Int64, limit: Int = 200) -> [FileRow] {
        guard let q = queue else { return [] }
        do {
            return try q.read { db in
                let rows = try Row.fetchAll(db, sql: """
                    SELECT DISTINCT files.* FROM files
                    INNER JOIN face_prints ON face_prints.file_id = files.id
                    WHERE face_prints.person_id = ? AND files.failed = 0
                    ORDER BY files.scanned_at DESC LIMIT ?
                    """, arguments: [personID, limit])
                return rows.map { Self.toFileRow($0) }
            }
        } catch {
            self.lastError = "People-file query failed: \(error)"
            return []
        }
    }

    public func totalFacePrints() -> Int {
        guard let q = queue else { return 0 }
        return (try? q.read { db in
            try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM face_prints") ?? 0
        }) ?? 0
    }

    /// Reassign every face_print on `sources` to `target`, delete the
    /// source rows, recompute target's file_count. Returns the new
    /// file_count or nil on failure.
    public func mergePersons(target: Int64, sources: [Int64]) -> Int? {
        let validSources = sources.filter { $0 != target }
        guard !validSources.isEmpty else { return nil }
        do {
            let queue = try DatabaseQueue(path: dbURL.path)
            let newCount: Int = try queue.write { db in
                let placeholders = validSources.map { _ in "?" }.joined(separator: ",")
                // 1. Reassign every face_print from the source persons to
                //    the target.
                var args: [DatabaseValueConvertible] = [target]
                args.append(contentsOf: validSources.map { Int($0) })
                try db.execute(
                    sql: "UPDATE face_prints SET person_id = ? WHERE person_id IN (\(placeholders))",
                    arguments: StatementArguments(args)
                )
                try db.execute(
                    sql: "DELETE FROM persons WHERE id IN (\(placeholders))",
                    arguments: StatementArguments(validSources.map { Int($0) })
                )
                try db.execute(sql: """
                    UPDATE persons SET file_count = (
                        SELECT COUNT(DISTINCT file_id)
                        FROM face_prints
                        WHERE person_id = ?
                    )
                    WHERE id = ?
                    """, arguments: [target, target])
                let n = try Int.fetchOne(db, sql:
                    "SELECT file_count FROM persons WHERE id = ?",
                    arguments: [target]) ?? 0
                return n
            }
            self.notifyChanged()
            return newCount
        } catch {
            self.lastError = "Merge failed: \(error)"
            return nil
        }
    }

    /// Apply many (target, source) merges in a single transaction.
    /// Resolves merge chains via union-find: if A→B and B→C, A's faces
    /// land on C. Returns the number of source clusters actually merged
    /// (chained-away duplicates count once).
    public func mergePersonsBatch(_ pairs: [(target: Int64, source: Int64)]) -> Int {
        guard !pairs.isEmpty else { return 0 }

        // Union-find over every person id touched.
        var parent: [Int64: Int64] = [:]
        func find(_ x: Int64) -> Int64 {
            var r = x
            while let p = parent[r], p != r { r = p }
            // Path compression.
            var cur = x
            while let p = parent[cur], p != r {
                parent[cur] = r
                cur = p
            }
            return r
        }
        func union(target: Int64, source: Int64) {
            let rt = find(target), rs = find(source)
            if rt == rs { return }
            // Always point the source root at the target root so the
            // caller's preferred target wins (named cluster, etc.).
            parent[rs] = rt
        }
        for (t, s) in pairs where t != s {
            if parent[t] == nil { parent[t] = t }
            if parent[s] == nil { parent[s] = s }
            union(target: t, source: s)
        }

        // Collect: per-final-target → list of source ids being absorbed.
        var byTarget: [Int64: [Int64]] = [:]
        for id in parent.keys {
            let root = find(id)
            if id != root {
                byTarget[root, default: []].append(id)
            }
        }
        guard !byTarget.isEmpty else { return 0 }

        var totalSources = 0
        do {
            let q = try DatabaseQueue(path: dbURL.path)
            try q.write { db in
                for (target, sources) in byTarget {
                    for chunk in stride(from: 0, to: sources.count, by: 500).map({
                        Array(sources[$0..<min($0 + 500, sources.count)])
                    }) {
                        let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                        var args: [DatabaseValueConvertible] = [target]
                        args.append(contentsOf: chunk.map { Int($0) })
                        try db.execute(
                            sql: "UPDATE face_prints SET person_id = ? WHERE person_id IN (\(placeholders))",
                            arguments: StatementArguments(args)
                        )
                        try db.execute(
                            sql: "DELETE FROM persons WHERE id IN (\(placeholders))",
                            arguments: StatementArguments(chunk.map { Int($0) })
                        )
                        totalSources += chunk.count
                    }
                }
                // Recompute file_count for every surviving target in one shot.
                let targetIDs = Array(byTarget.keys)
                for chunk in stride(from: 0, to: targetIDs.count, by: 500).map({
                    Array(targetIDs[$0..<min($0 + 500, targetIDs.count)])
                }) {
                    let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                    try db.execute(sql: """
                        UPDATE persons SET file_count = (
                            SELECT COUNT(DISTINCT file_id)
                            FROM face_prints
                            WHERE face_prints.person_id = persons.id
                        )
                        WHERE id IN (\(placeholders))
                        """, arguments: StatementArguments(chunk.map { Int($0) }))
                }
            }
            self.notifyChanged()
            return totalSources
        } catch {
            self.lastError = "Batch merge failed: \(error)"
            return 0
        }
    }

    // MARK: - Helpers

    private static func toFileRow(_ r: Row) -> FileRow {
        FileRow(
            id: r["id"],
            pathText: r["path_text"],
            sizeBytes: r["size_bytes"],
            createdAt: (r["created_at"] as Double?).map { Date(timeIntervalSinceReferenceDate: $0) },
            modifiedAt: (r["modified_at"] as Double?).map { Date(timeIntervalSinceReferenceDate: $0) },
            scannedAt: Date(timeIntervalSinceReferenceDate: r["scanned_at"]),
            kind: r["kind"], extension: r["extension"],
            phash: r["phash"], aesthetic: r["aesthetic"],
            hasFaces: (r["has_faces"] as Int?? ?? 0) != 0,
            hasText: (r["has_text"] as Int?? ?? 0) != 0,
            cameraModel: r["camera_model"],
            locationLat: r["location_lat"], locationLon: r["location_lon"],
            failed: (r["failed"] as Int?? ?? 0) != 0,
            errorMessage: r["error_message"],
            vlmDescription: r["vlm_description"],
            vlmProposedName: r["vlm_proposed_name"],
            vlmModel: r["vlm_model"],
            vlmAnalyzedAt: (r["vlm_analyzed_at"] as Double?).map { Date(timeIntervalSinceReferenceDate: $0) }
        )
    }

    // MARK: - Deep Analyze queries

    public func deepAnalyzePending(modelKey: String) -> (total: Int, pending: Int) {
        guard let q = queue else { return (0, 0) }
        return (try? q.read { db in
            let total = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE kind = 'image' AND failed = 0") ?? 0
            let pending = try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM files
                WHERE kind = 'image' AND failed = 0
                  AND (vlm_model IS NULL OR vlm_model != ?)
                """, arguments: [modelKey]) ?? 0
            return (total, pending)
        }) ?? (0, 0)
    }

    /// Bulk path_text update for Restructure post-move.
    public func updatePathTexts(_ pairs: [(Int64, String)]) async {
        guard !pairs.isEmpty else { return }
        do {
            let q = try DatabaseQueue(path: dbURL.path)
            try await q.write { db in
                for (id, path) in pairs {
                    try db.execute(
                        sql: "UPDATE files SET path_text = ? WHERE id = ?",
                        arguments: [path, id]
                    )
                }
            }
            self.notifyChanged()
        } catch {
            self.lastError = "Path update failed: \(error)"
        }
    }

    /// Rename the file on disk to its proposed VLM name and update the
    /// DB row. Returns the new path or nil on failure.
    public func applyProposedName(file: FileRow) -> URL? {
        guard let proposed = file.vlmProposedName, !proposed.isEmpty else { return nil }
        let oldURL = file.url
        let dir = oldURL.deletingLastPathComponent()
        let ext = oldURL.pathExtension
        let baseName = ext.isEmpty ? proposed : "\(proposed).\(ext)"
        var target = dir.appendingPathComponent(baseName)
        var bump = 2
        while FileManager.default.fileExists(atPath: target.path) && target != oldURL {
            let bumped = ext.isEmpty ? "\(proposed)_\(bump)" : "\(proposed)_\(bump).\(ext)"
            target = dir.appendingPathComponent(bumped)
            bump += 1
            if bump > 99 { return nil }
        }
        guard target != oldURL else { return oldURL }
        do {
            try FileManager.default.moveItem(at: oldURL, to: target)
        } catch {
            self.lastError = "Rename failed: \(error.localizedDescription)"
            return nil
        }
        do {
            let queue = try DatabaseQueue(path: dbURL.path)
            try queue.write { db in
                try db.execute(
                    sql: "UPDATE files SET path_text = ?, vlm_proposed_name = NULL WHERE id = ?",
                    arguments: [target.path, file.id]
                )
            }
            self.notifyChanged()
            return target
        } catch {
            self.lastError = "DB update after rename failed: \(error)"
            return nil
        }
    }
}
