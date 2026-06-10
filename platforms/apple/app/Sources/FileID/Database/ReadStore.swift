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
        AppSupportPath.fileID.appendingPathComponent("fileid.sqlite")
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
                var affectedPersons = Set<Int64>()
                for chunk in stride(from: 0, to: ids.count, by: 500) {
                    let slice = ids[chunk..<min(chunk + 500, ids.count)]
                    let placeholders = slice.map { _ in "?" }.joined(separator: ", ")
                    // Capture persons whose faces are about to be cascade-
                    // deleted so we can fix their counts/representative below.
                    let pids = try Int64.fetchAll(db, sql: """
                        SELECT DISTINCT person_id FROM face_prints
                        WHERE person_id IS NOT NULL AND file_id IN (\(placeholders))
                        """, arguments: StatementArguments(slice))
                    affectedPersons.formUnion(pids)
                    let stmt = "DELETE FROM files WHERE id IN (\(placeholders))"
                    try db.execute(sql: stmt, arguments: StatementArguments(slice))
                    total += db.changesCount
                }
                // Reconcile persons: ON DELETE CASCADE removed their face rows
                // but leaves persons.file_count stale and representative_face_id
                // dangling at a now-deleted face.
                for pid in affectedPersons {
                    try db.execute(sql: """
                        UPDATE persons SET file_count =
                            (SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id = ?)
                        WHERE id = ?
                        """, arguments: [pid, pid])
                    try db.execute(sql: """
                        UPDATE persons SET representative_face_id =
                            (SELECT id FROM face_prints WHERE person_id = ? ORDER BY id LIMIT 1)
                        WHERE id = ?
                          AND (representative_face_id IS NULL
                               OR representative_face_id NOT IN
                                  (SELECT id FROM face_prints WHERE person_id = ?))
                        """, arguments: [pid, pid, pid])
                }
                return total
            }
            SpotlightIndexer.deindex(ids: ids)
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

                // Duplicate groups by phash (groups of size > 1). Mirror the
                // Cleanup list exactly: filter failed = 0, and compute
                // reclaimable bytes against the ACTUAL keeper (the same
                // aesthetic↓, size↓, createdAt↑, path-length↑ rank the list
                // uses), not MAX(size). The old MAX(size) keeper diverged from
                // the displayed keeper whenever aesthetic decided it.
                let dupRow = try Row.fetchOne(db, sql: """
                    WITH ranked AS (
                        SELECT phash, size_bytes,
                               ROW_NUMBER() OVER (
                                   PARTITION BY phash
                                   ORDER BY COALESCE(aesthetic, 0) DESC,
                                            size_bytes DESC,
                                            COALESCE(created_at, 1e18) ASC,
                                            LENGTH(path_text) ASC
                               ) AS rk,
                               COUNT(*) OVER (PARTITION BY phash) AS n
                        FROM files
                        WHERE phash IS NOT NULL AND phash != 0 AND failed = 0
                    )
                    SELECT
                        (SELECT COUNT(DISTINCT phash) FROM ranked WHERE n > 1) AS groups,
                        COALESCE((SELECT SUM(size_bytes) FROM ranked WHERE n > 1 AND rk > 1), 0) AS reclaimable
                    """)
                self.totalDuplicateGroups = dupRow?["groups"] ?? 0
                let reclaimableBytes: Int64 = dupRow?["reclaimable"] ?? 0
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
                let trimmedSearch = search.trimmingCharacters(in: .whitespaces)
                if !trimmedSearch.isEmpty {
                    // Escape SQL LIKE metacharacters so a search for
                    // "100%_discount" matches the literal string and not
                    // "100" + arbitrary chars + "_discount". The
                    // ESCAPE '\' clause is appended to every LIKE so
                    // SQLite knows about the escape character we used.
                    // NFC-normalize first: SQLite LIKE compares bytes, and
                    // path_search stores the NFC form (v16) so an NFC query
                    // matches names regardless of on-disk normalization.
                    let escapedSearch = trimmedSearch
                        .precomposedStringWithCanonicalMapping
                        .replacingOccurrences(of: "\\", with: "\\\\")
                        .replacingOccurrences(of: "%", with: "\\%")
                        .replacingOccurrences(of: "_", with: "\\_")
                    let like = "%\(escapedSearch)%"
                    let ftsQuery = FTSQuery.quoted(trimmedSearch)
                    // Keyword search across filename, OCR text,
                    // vision tags, smart names, and VLM captions.
                    // CLIP semantic search runs separately when
                    // the encoder is installed.
                    sql += """
                         AND (
                              id IN (SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH ?)
                              OR path_search LIKE ? ESCAPE '\\'
                              OR vlm_proposed_name LIKE ? ESCAPE '\\'
                              OR vlm_description LIKE ? ESCAPE '\\'
                              OR id IN (SELECT file_id FROM tags WHERE tag LIKE ? ESCAPE '\\')
                              OR id IN (
                                  SELECT face_prints.file_id FROM face_prints
                                  INNER JOIN persons ON persons.id = face_prints.person_id
                                  WHERE persons.name LIKE ? ESCAPE '\\'
                                     OR persons.first_name LIKE ? ESCAPE '\\'
                                     OR persons.last_name LIKE ? ESCAPE '\\'
                              )
                            )
                        """
                    args += [ftsQuery, like, like, like, like, like, like, like]
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

    /// CLIP text → image semantic search. Embeds the query via the
    /// CLIP text encoder, ranks files by cosine over their stored
    /// image embeddings. Returns nil when the text encoder isn't
    /// installed (caller falls back to keyword search).
    public func semanticSearch(query: String, limit: Int = 60) -> [FileRow]? {
        guard let textVec = CLIPTextEncoder.shared.embedText(query) else { return nil }
        return rankByCosine(against: textVec, limit: limit)
    }

    /// "More photos like this one" — top-K by cosine over CLIP
    /// image embeddings. Doesn't need the text encoder.
    public func similarFiles(toFileID seedID: Int64, limit: Int = 24) -> [FileRow] {
        guard let q = queue else { return [] }
        let seedVec: [Float] = (try? q.read { db -> [Float] in
            guard let blob = try Data.fetchOne(db, sql:
                "SELECT embedding FROM clip_embeddings WHERE file_id = ?",
                arguments: [seedID]) else { return [] }
            return blobToFloats(blob)
        }) ?? []
        guard !seedVec.isEmpty else { return [] }
        return rankByCosine(against: seedVec, limit: limit, excludeID: seedID)
    }

    /// Top-K files ranked by cosine similarity to the given query
    /// vector (in CLIP image-embedding space). Used by both visual
    /// similarity (seed = a file's embedding) and semantic search
    /// (seed = a CLIP text embedding).
    public func rankByCosine(against query: [Float], limit: Int = 60,
                              excludeID: Int64? = nil) -> [FileRow] {
        guard let q = queue, !query.isEmpty else { return [] }
        return (try? q.read { db -> [FileRow] in
            // failed = 0 at SQL time (parity with Windows
            // SemanticSearchAsync): a failed row scored here would land
            // in the top-N, then be dropped at materialization below —
            // displacing a real result, not just wasting dot products.
            let sql: String
            let args: StatementArguments
            if let exclude = excludeID {
                sql = """
                    SELECT e.file_id, e.embedding FROM clip_embeddings e
                    JOIN files f ON f.id = e.file_id
                    WHERE f.failed = 0 AND e.file_id != ?
                    """
                args = [exclude]
            } else {
                sql = """
                    SELECT e.file_id, e.embedding FROM clip_embeddings e
                    JOIN files f ON f.id = e.file_id
                    WHERE f.failed = 0
                    """
                args = []
            }
            let rows = try Row.fetchAll(db, sql: sql, arguments: args)
            struct Scored { let id: Int64; let score: Float }
            var scored: [Scored] = []
            scored.reserveCapacity(rows.count)
            for r in rows {
                guard let fid: Int64 = r["file_id"],
                      let blob: Data = r["embedding"] else { continue }
                let v = blobToFloats(blob)
                guard v.count == query.count else { continue }
                var s: Float = 0
                for i in 0..<v.count { s += query[i] * v[i] }
                scored.append(Scored(id: fid, score: s))
            }
            scored.sort { $0.score > $1.score }
            let topIDs = scored.prefix(limit).map { $0.id }
            guard !topIDs.isEmpty else { return [] }
            let placeholders = topIDs.map { _ in "?" }.joined(separator: ",")
            let fileArgs: [DatabaseValueConvertible] = topIDs.map { Int($0) }
            let fileRows = try Row.fetchAll(db, sql: """
                SELECT * FROM files WHERE id IN (\(placeholders)) AND failed = 0
                """, arguments: StatementArguments(fileArgs))
            let byID = Dictionary(uniqueKeysWithValues: fileRows.map {
                (Int64($0["id"] ?? 0), Self.toFileRow($0))
            })
            return topIDs.compactMap { byID[$0] }
        }) ?? []
    }

    private func blobToFloats(_ data: Data) -> [Float] {
        let count = data.count / MemoryLayout<Float>.stride
        guard count > 0 else { return [] }
        return data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> [Float] in
            let base = raw.baseAddress!.assumingMemoryBound(to: Float.self)
            return Array(UnsafeBufferPointer(start: base, count: count))
        }
    }

    /// Bulk fetch FileRows for a list of ids, preserving the input
    /// order. Used by Memory detail to render the photos in the order
    /// the memory builder produced them (chronological).
    public func files(forFileIDs ids: [Int64]) -> [FileRow] {
        guard let q = queue, !ids.isEmpty else { return [] }
        return (try? q.read { db -> [FileRow] in
            let placeholders = ids.map { _ in "?" }.joined(separator: ",")
            let args: [DatabaseValueConvertible] = ids.map { Int($0) }
            let rows = try Row.fetchAll(db, sql: """
                SELECT * FROM files WHERE id IN (\(placeholders)) AND failed = 0
                """, arguments: StatementArguments(args))
            let byID = Dictionary(uniqueKeysWithValues: rows.map {
                (Int64($0["id"] ?? 0), Self.toFileRow($0))
            })
            return ids.compactMap { byID[$0] }
        }) ?? []
    }

    /// Path → URL lookup for a single file id. Used by Memories +
    /// Spotlight indexing to find a thumbnail for a hero image without
    /// fetching the whole FileRow.
    public func fileURL(forID id: Int64) -> URL? {
        guard let q = queue else { return nil }
        return (try? q.read { db in
            try String.fetchOne(db, sql: "SELECT path_text FROM files WHERE id = ?",
                                  arguments: [id])
        })
        .flatMap { $0 }
        .map { URL(fileURLWithPath: $0) }
    }

    public func tags(forFileID id: Int64) -> [String] {
        guard let q = queue else { return [] }
        return (try? q.read { db in
            try String.fetchAll(db, sql: "SELECT tag FROM tags WHERE file_id = ? ORDER BY tag", arguments: [id])
        }) ?? []
    }

    /// Top vision-classified tags by confidence (insert order preserved by
    /// rowid; VisionWorker emits results pre-sorted descending). Used by
    /// Library tiles for at-a-glance content cues — no need to open the
    /// preview sheet to see what a photo contains.
    public func topVisionTags(forFileID id: Int64, limit: Int) -> [String] {
        guard let q = queue, limit > 0 else { return [] }
        return (try? q.read { db in
            try String.fetchAll(db, sql: """
                SELECT tag FROM tags
                WHERE file_id = ? AND source = 'auto'
                ORDER BY rowid
                LIMIT ?
                """, arguments: [id, limit])
        }) ?? []
    }

    /// Bulk-fetch top vision tags for many files in one SQL query
    /// (the per-tile call would fire 1000+ queries on a large grid).
    /// Each file gets at most `limit` tags in confidence-descending order.
    public func topVisionTagsBulk(forFileIDs ids: [Int64], limit: Int = 2)
        -> [Int64: [String]]
    {
        guard let q = queue, !ids.isEmpty, limit > 0 else { return [:] }
        return (try? q.read { db -> [Int64: [String]] in
            let placeholders = ids.map { _ in "?" }.joined(separator: ",")
            let args: [DatabaseValueConvertible] = ids.map { Int($0) }
            // Window-function ranking to keep only the top `limit` per
            // file_id. Single round-trip; result post-processed by
            // grouping in Swift.
            let rows = try Row.fetchAll(db, sql: """
                SELECT file_id, tag, rowid_rank FROM (
                    SELECT t.file_id, t.tag,
                           ROW_NUMBER() OVER (
                               PARTITION BY t.file_id
                               ORDER BY t.rowid ASC
                           ) AS rowid_rank
                    FROM tags t
                    WHERE t.file_id IN (\(placeholders))
                      AND t.source = 'auto'
                ) WHERE rowid_rank <= ?
                """, arguments: StatementArguments(args + [limit]))
            var out: [Int64: [String]] = [:]
            out.reserveCapacity(ids.count)
            for r in rows {
                guard let fid: Int64 = r["file_id"],
                      let tag: String = r["tag"] else { continue }
                out[fid, default: []].append(tag)
            }
            return out
        }) ?? [:]
    }

    // MARK: - Cleanup queries

    /// Duplicate groups. Files within each group are sorted keeper-first.
    public func duplicateGroups() -> [DuplicateGroup] {
        guard let q = queue else { return [] }
        do {
            return try q.read { db in
                // Single-pass query: pull every duplicate-group file in
                // one read instead of N+1 (a SELECT per phash). On a
                // 50K library with thousands of duplicate groups, the
                // old shape was ~5K reads each holding a read lock —
                // 10–50 s of UI lag. Now it's two reads total.
                let groupCounts = try Row.fetchAll(db, sql: """
                    SELECT phash, COUNT(*) AS n
                    FROM files
                    WHERE phash IS NOT NULL AND phash != 0 AND failed = 0
                    GROUP BY phash
                    HAVING n > 1
                    ORDER BY n DESC
                    """)
                guard !groupCounts.isEmpty else { return [] }

                // Order-preserving phash list + lookup-by-phash.
                let orderedPhashes: [Int64] = groupCounts.compactMap { $0["phash"] }

                // Chunked reads — SQLite's default SQLITE_MAX_VARIABLE_NUMBER
                // is 999 per query. A library with 1000+ duplicate groups
                // would silently fail without chunking.
                var byPhash: [Int64: [FileRow]] = [:]
                byPhash.reserveCapacity(orderedPhashes.count)
                let chunkSize = 500
                var idx = 0
                while idx < orderedPhashes.count {
                    let end = min(idx + chunkSize, orderedPhashes.count)
                    let chunk = Array(orderedPhashes[idx..<end])
                    let placeholders = Array(repeating: "?", count: chunk.count).joined(separator: ",")
                    let chunkFiles = try Row.fetchAll(db, sql: """
                        SELECT * FROM files
                        WHERE phash IN (\(placeholders)) AND failed = 0
                        """, arguments: StatementArguments(chunk))
                    for r in chunkFiles {
                        let p: Int64 = r["phash"] ?? 0
                        byPhash[p, default: []].append(Self.toFileRow(r))
                    }
                    idx = end
                }

                var groups: [DuplicateGroup] = []
                groups.reserveCapacity(orderedPhashes.count)
                for phash in orderedPhashes {
                    guard var files = byPhash[phash], files.count > 1 else { continue }
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
                    startedAt: Date(timeIntervalSince1970: r["started_at"]),
                    completedAt: (r["completed_at"] as Double?).map { Date(timeIntervalSince1970: $0) },
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

    public func persons(includeUnknown: Bool = false) -> [PersonRow] {
        guard let q = queue else { return [] }
        let where_ = includeUnknown ? "" : "WHERE IFNULL(p.is_unknown, 0) = 0"
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
                    \(where_)
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

    /// Count of persons currently marked as unknown — for the
    /// "X hidden, show them" footer on the People tab.
    public func hiddenUnknownCount() -> Int {
        guard let q = queue else { return 0 }
        return (try? q.read { db in
            try Int.fetchOne(db, sql:
                "SELECT COUNT(*) FROM persons WHERE IFNULL(is_unknown, 0) = 1") ?? 0
        }) ?? 0
    }

    /// Persons with at least one name field populated and not marked
    /// unknown. Drives the sidebar pipeline indicator + the Deep
    /// Analyze gating ("you must name at least one person first").
    public func namedPersonCount() -> Int {
        guard let q = queue else { return 0 }
        return (try? q.read { db in
            try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM persons
                WHERE IFNULL(is_unknown, 0) = 0
                  AND (
                    (name IS NOT NULL AND name <> '')
                    OR (first_name IS NOT NULL AND first_name <> '')
                    OR (last_name  IS NOT NULL AND last_name  <> '')
                  )
            """) ?? 0
        }) ?? 0
    }

    /// Files that have a VLM-generated caption / proposed name. Used
    /// by the sidebar pipeline to know whether Deep Analyze has run.
    public func totalCaptioned() -> Int {
        guard let q = queue else { return 0 }
        return (try? q.read { db in
            try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM files
                WHERE failed = 0
                  AND vlm_proposed_name IS NOT NULL
                  AND vlm_proposed_name <> ''
            """) ?? 0
        }) ?? 0
    }

    /// Files Deep Analyze can target (image / pdf / video / doc).
    /// Used by the Restructure tab's hint banner to decide whether to
    /// nudge the user toward running Deep Analyze for sharper proposals.
    public func totalAnalyzableFiles() -> Int {
        guard let q = queue else { return 0 }
        return (try? q.read { db in
            try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM files
                WHERE failed = 0
                  AND kind IN ('image', 'pdf', 'video', 'doc')
            """) ?? 0
        }) ?? 0
    }

    /// `totalAnalyzableFiles` + `totalCaptioned` in one table pass —
    /// Restructure's regenerate() needs both, and two separate
    /// full-table COUNTs doubled the scan on large libraries. Same
    /// predicates as the individual functions; one shared snapshot.
    public func filesAnalysisStats() -> (analyzable: Int, captioned: Int) {
        guard let q = queue else { return (0, 0) }
        return (try? q.read { db in
            let row = try Row.fetchOne(db, sql: """
                SELECT
                  SUM(CASE WHEN kind IN ('image', 'pdf', 'video', 'doc')
                      THEN 1 ELSE 0 END) AS analyzable,
                  SUM(CASE WHEN vlm_proposed_name IS NOT NULL
                            AND vlm_proposed_name <> ''
                      THEN 1 ELSE 0 END) AS captioned
                FROM files WHERE failed = 0
            """)
            return ((row?["analyzable"] as Int?) ?? 0,
                    (row?["captioned"] as Int?) ?? 0)
        }) ?? (0, 0)
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

    /// Move every face_print belonging to `source` person AND any of
    /// `fileIDs` to belong to `target` person instead. Used by the
    /// People-tab "Move to another person" multi-select action: when
    /// the clusterer wrongly assigned a photo of Adam to Jack's
    /// cluster, the user picks Adam in Jack's sheet and reassigns
    /// just those photos.
    ///
    /// File-level granularity (not face-print-level): if a file has
    /// multiple faces matched to `source`, all of them move. The
    /// common case is one face per file per person; the edge case is
    /// already a clusterer mistake the user is correcting.
    public func movePersonFaces(fromPersonID source: Int64,
                                  toPersonID target: Int64,
                                  fileIDs: [Int64]) -> Int {
        guard !fileIDs.isEmpty, source != target else { return 0 }
        do {
            let queue = try DatabaseQueue(path: dbURL.path)
            let moved = try queue.write { db -> Int in
                let placeholders = fileIDs.map { _ in "?" }.joined(separator: ",")
                var args: [DatabaseValueConvertible] = [target, source]
                args.append(contentsOf: fileIDs.map { Int($0) })
                try db.execute(
                    sql: """
                        UPDATE face_prints SET person_id = ?
                        WHERE person_id = ? AND file_id IN (\(placeholders))
                        """,
                    arguments: StatementArguments(args)
                )
                let changes = db.changesCount
                // Recount file_count for both source and target.
                try db.execute(sql: """
                    UPDATE persons SET file_count = (
                        SELECT COUNT(DISTINCT file_id) FROM face_prints
                        WHERE person_id = persons.id
                    ) WHERE id IN (?, ?)
                    """, arguments: [source, target])
                return changes
            }
            self.notifyChanged()
            return moved
        } catch {
            self.lastError = "Move person faces failed: \(error)"
            return 0
        }
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
            createdAt: (r["created_at"] as Double?).map { Date(timeIntervalSince1970: $0) },
            modifiedAt: (r["modified_at"] as Double?).map { Date(timeIntervalSince1970: $0) },
            scannedAt: Date(timeIntervalSince1970: r["scanned_at"]),
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
            vlmAnalyzedAt: (r["vlm_analyzed_at"] as Double?).map { Date(timeIntervalSince1970: $0) }
        )
    }

    // MARK: - Deep Analyze queries

    public func deepAnalyzePending(modelKey: String) -> (total: Int, pending: Int) {
        guard let q = queue else { return (0, 0) }
        return (try? q.read { db in
            let total = try Int.fetchOne(db, sql:
                "SELECT COUNT(*) FROM files WHERE kind IN ('image', 'pdf') AND failed = 0") ?? 0
            let pending = try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM files
                WHERE kind IN ('image', 'pdf') AND failed = 0
                  AND (vlm_model IS NULL OR vlm_model != ?)
                """, arguments: [modelKey]) ?? 0
            return (total, pending)
        }) ?? (0, 0)
    }

    /// One busy-tolerant write connection for a whole restructure batch.
    /// Callers update each row right after its move so a crash or DB
    /// failure strands at most one file (which the caller rolls back) —
    /// the old batch-end variant had no busy timeout and swallowed the
    /// error after every file had already moved on disk.
    public func openPathUpdateQueue() throws -> DatabaseQueue {
        try renameWriteQueue()
    }

    public func updatePathText(fileID: Int64, newPath: String, on queue: DatabaseQueue) throws {
        try queue.write { db in
            try db.execute(
                sql: "UPDATE files SET path_text = ?, path_search = ? WHERE id = ?",
                arguments: [newPath, newPath.precomposedStringWithCanonicalMapping, fileID]
            )
        }
    }

    /// All non-failed image files that have a non-empty
    /// `vlm_proposed_name`. Used by the bulk-rename UI.
    public func filesWithProposedNames(limit: Int = 1000) -> [FileRow] {
        guard let q = queue else { return [] }
        do {
            return try q.read { db in
                let rows = try Row.fetchAll(db, sql: """
                    SELECT * FROM files
                    WHERE failed = 0
                      AND vlm_proposed_name IS NOT NULL
                      AND vlm_proposed_name != ''
                    ORDER BY scanned_at DESC LIMIT ?
                    """, arguments: [limit])
                return rows.map { Self.toFileRow($0) }
            }
        } catch {
            self.lastError = "Proposed-name query failed: \(error)"
            return []
        }
    }

    /// Count-only twin of `filesWithProposedNames` — the badge/refresh
    /// paths only need the number, and SELECT * deserialized up to
    /// 5000 full rows per refresh. The inner LIMIT keeps the cap
    /// semantics identical to `filesWithProposedNames(limit:).count`.
    public func countFilesWithProposedNames(limit: Int = 5000) -> Int {
        guard let q = queue else { return 0 }
        return (try? q.read { db in
            try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM (
                    SELECT 1 FROM files
                    WHERE failed = 0
                      AND vlm_proposed_name IS NOT NULL
                      AND vlm_proposed_name != ''
                    LIMIT ?
                )
                """, arguments: [limit]) ?? 0
        }) ?? 0
    }

    /// Apply renames to many files. Returns per-file results; the
    /// caller persists `oldByID` to UserDefaults so the last batch can
    /// be undone.
    public struct RenameOutcome: Sendable, Codable {
        public let fileID: Int64
        public let oldPath: String
        public let newPath: String
        /// Identity at rename time (nil in journals from older builds).
        /// Undo skips the entry on mismatch — a same-named replacement
        /// file at newPath must not be silently renamed.
        public let fileSize: Int64?
        public let modifiedAt: Date?

        init(fileID: Int64, oldPath: String, newPath: String) {
            self.fileID = fileID
            self.oldPath = oldPath
            self.newPath = newPath
            let attrs = try? FileManager.default.attributesOfItem(atPath: newPath)
            self.fileSize = attrs?[.size] as? Int64
            self.modifiedAt = attrs?[.modificationDate] as? Date
        }
    }

    public struct BulkRenameResult: Sendable {
        public let renamed: [RenameOutcome]
        public let failed: Int
        public let firstError: String?
    }

    public func applyProposedNamesBulk(_ files: [FileRow]) -> BulkRenameResult {
        guard !files.isEmpty else {
            return BulkRenameResult(renamed: [], failed: 0, firstError: nil)
        }
        // One connection for the whole batch — opening a fresh
        // DatabaseQueue per file made a 100-file rename open 100
        // connections.
        let queue: DatabaseQueue
        do {
            queue = try renameWriteQueue()
        } catch {
            self.lastError = "DB open for rename failed: \(error)"
            return BulkRenameResult(renamed: [], failed: files.count,
                                    firstError: self.lastError)
        }
        var renamed: [RenameOutcome] = []
        var failed = 0
        var firstError: String?
        for f in files {
            let oldPath = f.pathText
            if let newURL = applyProposedName(file: f, on: queue) {
                if newURL.path != oldPath {
                    renamed.append(RenameOutcome(fileID: f.id,
                                                  oldPath: oldPath,
                                                  newPath: newURL.path))
                }
            } else {
                failed += 1
                if firstError == nil { firstError = self.lastError }
            }
        }
        return BulkRenameResult(renamed: renamed, failed: failed, firstError: firstError)
    }

    /// Reverse a previously-applied rename batch. Walks each entry
    /// backwards: `mv newPath oldPath`. Skips entries whose newPath no
    /// longer exists (user already moved them again somewhere) or no
    /// longer matches the recorded size/mtime — these are reported as
    /// `skipped`.
    public func undoRenames(_ outcomes: [RenameOutcome]) -> (undone: Int, skipped: Int, failed: Int) {
        var undone = 0
        var skipped = 0
        var failed = 0
        let fm = FileManager.default
        for r in outcomes.reversed() {
            let newURL = URL(fileURLWithPath: r.newPath)
            let oldURL = URL(fileURLWithPath: r.oldPath)
            guard fm.fileExists(atPath: newURL.path) else {
                skipped += 1; continue
            }
            if let size = r.fileSize, let date = r.modifiedAt {
                let attrs = try? fm.attributesOfItem(atPath: newURL.path)
                guard let curSize = attrs?[.size] as? Int64,
                      let curDate = attrs?[.modificationDate] as? Date,
                      curSize == size,
                      abs(curDate.timeIntervalSince(date)) < 1
                else {
                    // A different file occupies newPath now — renaming
                    // it would clobber an unrelated file's name and
                    // repoint the DB row at the wrong bytes.
                    skipped += 1; continue
                }
            }
            if fm.fileExists(atPath: oldURL.path) {
                // The old path is now occupied — bail rather than clobber.
                skipped += 1; continue
            }
            do {
                try fm.moveItem(at: newURL, to: oldURL)
                // Only count as undone once the DB agrees. The DB restore used
                // to be a `try?`-swallow, leaving the row pointing at a
                // now-nonexistent path on failure. If it fails, roll the file
                // back so disk and DB stay consistent and report it as failed.
                do {
                    var config = Configuration()
                    config.busyMode = .timeout(5)
                    let q = try DatabaseQueue(path: dbURL.path, configuration: config)
                    try q.write { db in
                        try db.execute(
                            sql: "UPDATE files SET path_text = ?, path_search = ? WHERE id = ?",
                            arguments: [oldURL.path,
                                        oldURL.path.precomposedStringWithCanonicalMapping,
                                        r.fileID]
                        )
                    }
                    undone += 1
                } catch {
                    try? fm.moveItem(at: oldURL, to: newURL)
                    failed += 1
                }
            } catch {
                failed += 1
            }
        }
        self.notifyChanged()
        return (undone, skipped, failed)
    }

    /// Rename the file on disk to its proposed VLM name and update the
    /// DB row. Returns the new path or nil on failure.
    public func applyProposedName(file: FileRow) -> URL? {
        do {
            return applyProposedName(file: file, on: try renameWriteQueue())
        } catch {
            self.lastError = "DB open for rename failed: \(error)"
            return nil
        }
    }

    // Busy timeout so momentary WAL contention with the engine writer
    // retries instead of failing immediately (which used to strand the
    // file: renamed on disk but the DB row still pointing at oldPath).
    private func renameWriteQueue() throws -> DatabaseQueue {
        var config = Configuration()
        config.busyMode = .timeout(5)
        return try DatabaseQueue(path: dbURL.path, configuration: config)
    }

    private func applyProposedName(file: FileRow, on queue: DatabaseQueue) -> URL? {
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
            try queue.write { db in
                try db.execute(
                    sql: """
                        UPDATE files
                        SET path_text = ?, path_search = ?, vlm_proposed_name = NULL
                        WHERE id = ?
                        """,
                    arguments: [target.path,
                                target.path.precomposedStringWithCanonicalMapping,
                                file.id]
                )
            }
            self.notifyChanged()
            return target
        } catch {
            // DB update failed after the on-disk move — roll the file back so
            // disk and DB stay consistent and the rename remains undoable.
            self.lastError = "DB update after rename failed: \(error)"
            try? FileManager.default.moveItem(at: target, to: oldURL)
            return nil
        }
    }
}
