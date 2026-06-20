// Read-only DB store. Engine is the single writer (WAL); app reads
// concurrently. `version` bumps each reload so SwiftUI re-queries.
import Foundation
import GRDB
import FileIDShared

@Observable
public final class ReadStore: @unchecked Sendable {
    // The read connection is guarded by `connLock`: many SwiftUI views
    // read concurrently off the main actor while `openIfPossible`/`close`
    // can swap or drop it. Without the lock, a reader's retain
    // (`guard let q = queue`) racing the teardown's release (`_queue = nil`)
    // is a data race that can over-release the live SQLite connection —
    // the macOS mirror of the Windows ReadStore.Dispose UAF. Each reader
    // captures a strong `q` under the lock, so ARC keeps the connection
    // alive until the last in-flight read drains, even across `close()`.
    @ObservationIgnored private let connLock = NSLock()
    @ObservationIgnored private var _queue: DatabaseQueue?
    private var queue: DatabaseQueue? {
        get { connLock.lock(); defer { connLock.unlock() }; return _queue }
        set { connLock.lock(); defer { connLock.unlock() }; _queue = newValue }
    }
    // Coalesce + throttle state for the off-main counters refresh. The
    // duplicate-group window query is O(N log N) and must not run on the
    // main thread on every scan tick — see `refreshCounters`.
    @ObservationIgnored private let countersLock = NSLock()
    @ObservationIgnored private var countersDirty = false
    @ObservationIgnored private var countersRunning = false
    private let dbURL: URL
    public private(set) var version: Int = 0
    public private(set) var totalFiles: Int = 0
    public private(set) var totalImages: Int = 0
    public private(set) var totalDuplicateGroups: Int = 0
    public private(set) var totalReclaimableMB: Double = 0
    public private(set) var lastError: String?

    // R3-06: `lastError` is an @Observable property SwiftUI reads on the
    // MainActor, but it's written from off-main detached tasks (bulk rename /
    // merge / undo). A bare off-main write of an Optional<String> racing a
    // MainActor read is undefined behavior under Swift 6. Route every write
    // through `reportError`: it updates a lock-backed shadow synchronously (so
    // the rare synchronous read-after-write paths still see the value) and
    // publishes the observable property on the main actor.
    @ObservationIgnored private let errorLock = NSLock()
    @ObservationIgnored private var _lastErrorShadow: String?
    private func reportError(_ message: String?) {
        errorLock.withLock { _lastErrorShadow = message }
        Task { @MainActor in self.lastError = message }
    }
    private func lastErrorSnapshot() -> String? {
        errorLock.withLock { _lastErrorShadow }
    }

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
                reportError("Could not open DB: \(error)")
                return
            }
        }
        refreshCounters()
    }

    public func notifyChanged() {
        // R3-06: `version` is an @Observable property SwiftUI reads on the
        // MainActor; notifyChanged() is reached OFF main from the bulk-rename /
        // merge / undo detached tasks, so a bare `version &+= 1` is an off-main
        // write racing a MainActor read (torn RMW / lost update). Serialize the
        // increment on the main actor. refreshCounters() is already internally
        // thread-safe (countersLock + Task.detached → MainActor.run publish).
        Task { @MainActor in self.version &+= 1 }
        refreshCounters()
    }

    /// Explicit teardown. Drops our reference to the read connection
    /// under the lock; ARC keeps the underlying SQLite connection alive
    /// until every in-flight read (each holding a strong `q`) drains, so
    /// closing never frees the connection out from under a reader.
    public func close() {
        queue = nil
    }

    /// Brief writable connection for Cleanup row deletes. WAL allows this
    /// from a separate process without blocking the engine writer.
    public func deleteFiles(ids: [Int64]) -> Int {
        guard !ids.isEmpty else { return 0 }
        do {
            let queue = try writeQueue()
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
            reportError("Prune failed: \(error)")
            return 0
        }
    }

    private struct CounterSnapshot {
        let totalFiles: Int
        let totalImages: Int
        let totalDuplicateGroups: Int
        let totalReclaimableMB: Double
    }

    private enum CounterResult {
        case ok(CounterSnapshot)
        case failure(String)
        case noDatabase
    }

    /// Schedule a counters refresh off the main thread. The duplicate-group
    /// window query is O(N log N) over the whole `files` table; run inline it
    /// pegged the UI because a live scan calls `notifyChanged` up to once a
    /// second from `@MainActor` views. A single background worker coalesces
    /// bursts (one in-flight run, re-run once if more requests arrived while
    /// it ran) and throttles successive heavy queries, so the main thread
    /// never pays for scan ticks and the table is scanned at most ~once a
    /// second during a burst. Property writes still land on the main actor.
    private func refreshCounters() {
        countersLock.lock()
        countersDirty = true
        if countersRunning {
            countersLock.unlock()
            return
        }
        countersRunning = true
        countersLock.unlock()

        Task.detached(priority: .utility) { [weak self] in
            while let self {
                // `withLock` (synchronous critical section) instead of bare
                // lock()/unlock(): NSLock's lock() is unavailable in an async
                // context under Swift 6 (it must never be held across a
                // suspension point — it isn't here, but the scoped form makes
                // that guarantee explicit and silences the diagnostic).
                let shouldStop = self.countersLock.withLock { () -> Bool in
                    if !self.countersDirty {
                        self.countersRunning = false
                        return true
                    }
                    self.countersDirty = false
                    return false
                }
                if shouldStop { return }

                switch self.computeCounters() {
                case .ok(let snapshot):
                    await MainActor.run { self.applyCounters(snapshot) }
                case .failure(let message):
                    await MainActor.run { self.lastError = message }
                case .noDatabase:
                    break
                }
                // Throttle: cap the heavy window query to ~once per interval
                // so a steady scan can't spin the background worker; the
                // trailing dirty flag still guarantees a final, accurate run.
                try? await Task.sleep(nanoseconds: 750_000_000)
            }
        }
    }

    private func computeCounters() -> CounterResult {
        guard let q = queue else { return .noDatabase }
        do {
            return try q.read { db -> CounterResult in
                let totalFiles  = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files") ?? 0
                let totalImages = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE kind = 'image' AND failed = 0") ?? 0

                // Duplicate groups by phash (groups of size > 1). Mirror the
                // Cleanup list exactly: filter failed = 0, and compute
                // reclaimable bytes against the ACTUAL keeper (the same
                // aesthetic↓, size↓, createdAt↑, path-length↑ rank the list
                // uses), not MAX(size). The old MAX(size) keeper diverged from
                // the displayed keeper whenever aesthetic decided it.
                let dupRow = try Row.fetchOne(db, sql: """
                    WITH ranked AS (
                        SELECT content_hash, size_bytes,
                               ROW_NUMBER() OVER (
                                   PARTITION BY content_hash
                                   ORDER BY COALESCE(aesthetic, 0) DESC,
                                            size_bytes DESC,
                                            COALESCE(created_at, 1e18) ASC,
                                            LENGTH(path_text) ASC
                               ) AS rk,
                               COUNT(*) OVER (PARTITION BY content_hash) AS n
                        FROM files
                        WHERE content_hash IS NOT NULL AND failed = 0
                    )
                    SELECT
                        (SELECT COUNT(DISTINCT content_hash) FROM ranked WHERE n > 1) AS groups,
                        COALESCE((SELECT SUM(size_bytes) FROM ranked WHERE n > 1 AND rk > 1), 0) AS reclaimable
                    """)
                let groups: Int = dupRow?["groups"] ?? 0
                let reclaimableBytes: Int64 = dupRow?["reclaimable"] ?? 0
                return .ok(CounterSnapshot(
                    totalFiles: totalFiles,
                    totalImages: totalImages,
                    totalDuplicateGroups: groups,
                    totalReclaimableMB: Double(reclaimableBytes) / 1_048_576
                ))
            }
        } catch {
            return .failure("Counters refresh failed: \(error)")
        }
    }

    @MainActor
    private func applyCounters(_ snapshot: CounterSnapshot) {
        self.totalFiles = snapshot.totalFiles
        self.totalImages = snapshot.totalImages
        self.totalDuplicateGroups = snapshot.totalDuplicateGroups
        self.totalReclaimableMB = snapshot.totalReclaimableMB
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
            reportError("Library query failed: \(error)")
            return []
        }
    }

    /// Off-main twin of `files(...)`. The keyword search runs a
    /// multi-table FTS + LIKE + face-join query; on a live scan the view
    /// reloaded it on the MainActor on every throttled batch event,
    /// stuttering the UI. Callers debounce and await this so the query
    /// runs on a background task and only the assignment lands on main.
    public func filesAsync(offset: Int = 0, limit: Int = 200,
                           search: String = "",
                           kindFilter: String? = nil) async -> [FileRow] {
        await Task.detached(priority: .userInitiated) { [self] in
            files(offset: offset, limit: limit, search: search, kindFilter: kindFilter)
        }.value
    }

    /// CLIP text → image semantic search. Embeds the query via the
    /// CLIP text encoder, ranks files by cosine over their stored
    /// image embeddings. Returns nil when the text encoder isn't
    /// installed (caller falls back to keyword search).
    public func semanticSearch(query: String, limit: Int = 60) -> [FileRow]? {
        guard let textVec = CLIPTextEncoder.shared.embedText(query) else { return nil }
        return rankByCosine(against: textVec, limit: limit)
    }

    /// Off-main twin of `semanticSearch`. The full clip_embeddings cosine
    /// scan is O(N·512) and froze the UI for seconds on a 50k library when
    /// run inline on the MainActor. The text embed + ranking happen on a
    /// background task; the caller awaits and publishes results on main.
    public func semanticSearchAsync(query: String, limit: Int = 60) async -> [FileRow]? {
        await Task.detached(priority: .userInitiated) { [self] in
            semanticSearch(query: query, limit: limit)
        }.value
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

    /// Off-main twin of `similarFiles` — the same full-table cosine scan
    /// as semantic search, kept off the MainActor for the same reason.
    public func similarFilesAsync(toFileID seedID: Int64, limit: Int = 24) async -> [FileRow] {
        await Task.detached(priority: .userInitiated) { [self] in
            similarFiles(toFileID: seedID, limit: limit)
        }.value
    }

    /// Top-K files ranked by cosine similarity to the given query
    /// vector (in CLIP image-embedding space). Used by both visual
    /// similarity (seed = a file's embedding) and semantic search
    /// (seed = a CLIP text embedding).
    public func rankByCosine(against query: [Float], limit: Int = 60,
                              excludeID: Int64? = nil) -> [FileRow] {
        guard let q = queue, !query.isEmpty, limit > 0 else { return [] }
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
            // Stream rows through a cursor and keep only a bounded top-K
            // min-heap. fetchAll would hold every 512-float embedding blob
            // resident at once (~1 GB at 500k files); the cursor frees each
            // blob right after it's scored. Ranking is (score desc, row-order
            // asc) — identical to the previous stable sort-then-prefix(limit),
            // so the result set and tie order are unchanged while retained
            // state drops from O(N) to O(limit).
            var heap = TopKByCosine(capacity: limit)
            let cursor = try Row.fetchCursor(db, sql: sql, arguments: args)
            var order = 0
            while let r = try cursor.next() {
                guard let fid: Int64 = r["file_id"],
                      let blob: Data = r["embedding"] else { continue }
                let v = blobToFloats(blob)
                guard v.count == query.count else { continue }
                var s: Float = 0
                for i in 0..<v.count { s += query[i] * v[i] }
                heap.offer(id: fid, score: s, order: order)
                order += 1
            }
            let topIDs = heap.sortedDescending().map { $0.id }
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
    /// Stable Int64 id for a duplicate group keyed by content_hash — the first
    /// 8 bytes of the 32-byte SHA-256, little-endian. Used only as the SwiftUI
    /// Identifiable id; collisions across distinct 256-bit hashes are infeasible.
    private static func dupGroupID(_ hash: Data) -> Int64 {
        var v: UInt64 = 0
        for (i, byte) in hash.prefix(8).enumerated() {
            v |= UInt64(byte) << (8 * i)
        }
        return Int64(bitPattern: v)
    }

    public func duplicateGroups() -> [DuplicateGroup] {
        guard let q = queue else { return [] }
        do {
            return try q.read { db in
                // Single-pass query: pull every duplicate-group file in
                // one read instead of N+1 (a SELECT per phash). On a
                // 50K library with thousands of duplicate groups, the
                // old shape was ~5K reads each holding a read lock —
                // 10–50 s of UI lag. Now it's two reads total.
                // Byte-exact dedup (item 1): two files are duplicates only when
                // their content_hash (SHA-256 of the bytes) is identical — i.e.
                // literally byte-for-byte the same file, not just perceptually
                // similar (the prior phash grouping). Non-images have a NULL
                // content_hash and are excluded, as before. Single-pass: GROUP BY
                // then one chunked SELECT, same shape as the prior phash query.
                let groupCounts = try Row.fetchAll(db, sql: """
                    SELECT content_hash, COUNT(*) AS n
                    FROM files
                    WHERE content_hash IS NOT NULL AND failed = 0
                    GROUP BY content_hash
                    HAVING n > 1
                    ORDER BY n DESC
                    """)
                guard !groupCounts.isEmpty else { return [] }

                // Order-preserving content_hash list + lookup-by-hash.
                let orderedHashes: [Data] = groupCounts.compactMap { $0["content_hash"] }

                // Chunked reads — SQLite's default SQLITE_MAX_VARIABLE_NUMBER
                // is 999 per query. A library with 1000+ duplicate groups
                // would silently fail without chunking.
                var byHash: [Data: [FileRow]] = [:]
                byHash.reserveCapacity(orderedHashes.count)
                let chunkSize = 500
                var idx = 0
                while idx < orderedHashes.count {
                    let end = min(idx + chunkSize, orderedHashes.count)
                    let chunk = Array(orderedHashes[idx..<end])
                    let placeholders = Array(repeating: "?", count: chunk.count).joined(separator: ",")
                    let chunkFiles = try Row.fetchAll(db, sql: """
                        SELECT * FROM files
                        WHERE content_hash IN (\(placeholders)) AND failed = 0
                        """, arguments: StatementArguments(chunk))
                    for r in chunkFiles {
                        guard let h: Data = r["content_hash"] else { continue }
                        byHash[h, default: []].append(Self.toFileRow(r))
                    }
                    idx = end
                }

                var groups: [DuplicateGroup] = []
                groups.reserveCapacity(orderedHashes.count)
                for hash in orderedHashes {
                    guard var files = byHash[hash], files.count > 1 else { continue }
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
                    groups.append(DuplicateGroup(id: Self.dupGroupID(hash), files: files))
                }
                return groups
            }
        } catch {
            reportError("Duplicate query failed: \(error)")
            return []
        }
    }

    /// Off-main twin of `duplicateGroups()`. The materialization does a GROUP BY
    /// over `files`, a chunked SELECT * of every duplicate-group file, FileRow
    /// mapping, and a per-group sort — work proportional to the duplicated-file
    /// count. Run inline on the MainActor, the Cleanup tab re-fired it on every
    /// throttled scan batch (notifyChanged ~once/s), janking the UI. Callers await
    /// this so the heavy read runs on a background task and only the assignment
    /// lands on main. (R3-05)
    public func duplicateGroupsAsync() async -> [DuplicateGroup] {
        await Task.detached(priority: .userInitiated) { [self] in
            duplicateGroups()
        }.value
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
                parts.append(s)
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
                      COUNT(fp.id) AS face_count
                    FROM persons p
                    LEFT JOIN face_prints f ON f.id = p.representative_face_id
                    LEFT JOIN files ON files.id = f.file_id
                    LEFT JOIN face_prints fp ON fp.person_id = p.id
                    \(where_)
                    GROUP BY p.id
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
            reportError("People query failed: \(error)")
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

    /// (clip, text) embedding-row counts — what the butler restructure clusters by.
    /// When BOTH are ~0 the scan ran without the CLIP / BGE models, so a plan can only
    /// fall back to the date/name rule cascade (Documents/<Year>, Photos/<Year>/<Month>);
    /// the Restructure tab surfaces this so the user knows to install the models + rescan.
    public func contentEmbeddingCounts() -> (clip: Int, text: Int) {
        guard let q = queue else { return (0, 0) }
        return (try? q.read { db in
            let clip = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM clip_embeddings") ?? 0
            let text = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM text_embeddings") ?? 0
            return (clip, text)
        }) ?? (0, 0)
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
            let queue = try writeQueue()
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
            reportError("Person update failed: \(error)")
        }
    }

    /// R5-02: bulk mark-as-unknown for the People multi-select action. ONE write
    /// connection + ONE transaction for the whole selection (chunked IN),
    /// mirroring the Windows engine's single markPersonsAsUnknown command. The
    /// previous per-id updatePerson loop opened N connections + ran N transactions
    /// on the main thread and beach-balled the UI for crowd-sized selections.
    /// Touches only is_unknown (the old loop re-wrote name columns from possibly-
    /// stale in-memory rows — this is strictly safer).
    public func markUnknownBatch(ids: [Int64]) -> Int {
        guard !ids.isEmpty else { return 0 }
        do {
            let queue = try writeQueue()
            let updated = try queue.write { db -> Int in
                var total = 0
                for chunk in stride(from: 0, to: ids.count, by: 500) {
                    let slice = ids[chunk..<min(chunk + 500, ids.count)]
                    let placeholders = slice.map { _ in "?" }.joined(separator: ", ")
                    try db.execute(
                        sql: "UPDATE persons SET is_unknown = 1 WHERE id IN (\(placeholders))",
                        arguments: StatementArguments(slice))
                    total += db.changesCount
                }
                return total
            }
            self.notifyChanged()
            return updated
        } catch {
            reportError("Mark-unknown batch failed: \(error)")
            return 0
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
            let queue = try writeQueue()
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
            reportError("Move person faces failed: \(error)")
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
            reportError("People-file query failed: \(error)")
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
            let queue = try writeQueue()
            let newCount: Int = try queue.write { db in
                // R5-01 name preservation: the People drag-merge passes the DROP TARGET
                // as `target` regardless of names, so dragging a NAMED card onto an
                // UNNAMED one would delete the typed name (persons row deleted, no undo).
                // If `target` has no typed name but exactly one entity in the merge set
                // does, that named one becomes the survivor instead. Every other caller
                // already passes the named cluster as `target`, so this is a no-op for
                // them. (audit — People drag-merge name loss)
                let allIDs = [target] + validSources
                let hasTypedName = "(COALESCE(TRIM(name),'')<>'' OR COALESCE(TRIM(title),'')<>'' OR COALESCE(TRIM(first_name),'')<>'' OR COALESCE(TRIM(middle_name),'')<>'' OR COALESCE(TRIM(last_name),'')<>'' OR COALESCE(TRIM(suffix),'')<>'')"
                let idPlaceholders = allIDs.map { _ in "?" }.joined(separator: ",")
                let named: Set<Int64> = Set(try Int64.fetchAll(db, sql:
                    "SELECT id FROM persons WHERE id IN (\(idPlaceholders)) AND is_unknown = 0 AND \(hasTypedName)",
                    arguments: StatementArguments(allIDs.map { Int($0) })))
                let survivor: Int64
                if named.contains(target) {
                    survivor = target
                } else {
                    let namedSources = validSources.filter { named.contains($0) }
                    survivor = namedSources.count == 1 ? namedSources[0] : target
                }
                let losers = allIDs.filter { $0 != survivor }
                let placeholders = losers.map { _ in "?" }.joined(separator: ",")
                // Reassign every face_print from the loser persons to the survivor,
                // then delete the losers.
                var args: [DatabaseValueConvertible] = [survivor]
                args.append(contentsOf: losers.map { Int($0) })
                try db.execute(
                    sql: "UPDATE face_prints SET person_id = ? WHERE person_id IN (\(placeholders))",
                    arguments: StatementArguments(args)
                )
                try db.execute(
                    sql: "DELETE FROM persons WHERE id IN (\(placeholders))",
                    arguments: StatementArguments(losers.map { Int($0) })
                )
                try db.execute(sql: """
                    UPDATE persons SET file_count = (
                        SELECT COUNT(DISTINCT file_id)
                        FROM face_prints
                        WHERE person_id = ?
                    )
                    WHERE id = ?
                    """, arguments: [survivor, survivor])
                let n = try Int.fetchOne(db, sql:
                    "SELECT file_count FROM persons WHERE id = ?",
                    arguments: [survivor]) ?? 0
                return n
            }
            self.notifyChanged()
            return newCount
        } catch {
            reportError("Merge failed: \(error)")
            return nil
        }
    }

    /// Apply many (target, source) merges in a single transaction.
    /// Resolves merge chains via union-find: if A→B and B→C, A's faces
    /// land on C. Returns the number of source clusters actually merged
    /// (chained-away duplicates count once).
    public func mergePersonsBatch(_ pairs: [(target: Int64, source: Int64)]) -> Int {
        guard !pairs.isEmpty else { return 0 }

        // R5-01: name-state + size for every touched cluster, so the union
        // survivor respects name priority ACROSS CHAINS. Per-pair preferredTarget
        // does NOT compose transitively — a chain of borderline pairs linking a
        // named and an unnamed cluster could otherwise reparent the named root
        // onto an unnamed one and DELETE the user's typed name.
        var touched = Set<Int64>()
        for (t, s) in pairs { touched.insert(t); touched.insert(s) }
        var isNamed: [Int64: Bool] = [:]
        var fileCount: [Int64: Int] = [:]
        if let rq = queue {
            let ids = Array(touched)
            for chunk in stride(from: 0, to: ids.count, by: 500).map({
                Array(ids[$0..<min($0 + 500, ids.count)])
            }) {
                let ph = chunk.map { _ in "?" }.joined(separator: ",")
                let rows = (try? rq.read { db in
                    try Row.fetchAll(db, sql: """
                        SELECT id, name, title, first_name, middle_name,
                               last_name, suffix, is_unknown, file_count
                        FROM persons WHERE id IN (\(ph))
                        """, arguments: StatementArguments(chunk.map { Int($0) }))
                }) ?? []
                for r in rows {
                    let id: Int64 = r["id"] ?? 0
                    fileCount[id] = r["file_count"] ?? 0
                    if (r["is_unknown"] as Int? ?? 0) != 0 {
                        isNamed[id] = false
                    } else {
                        let cols = ["name", "title", "first_name",
                                    "middle_name", "last_name", "suffix"]
                        isNamed[id] = cols.contains {
                            (r[$0] as String?)?.trimmingCharacters(in: .whitespaces).isEmpty == false
                        }
                    }
                }
            }
        }
        // Survivor priority: named > larger file_count > lower id.
        func preferredRoot(_ a: Int64, _ b: Int64) -> Int64 {
            let an = isNamed[a] ?? false, bn = isNamed[b] ?? false
            if an != bn { return an ? a : b }
            let ac = fileCount[a] ?? 0, bc = fileCount[b] ?? 0
            if ac != bc { return ac > bc ? a : b }
            return a < b ? a : b
        }

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
            // R5-01: keep the higher-priority root (named > larger file_count >
            // lower id) so a typed name is never deleted when a chain of
            // borderline pairs links a named and an unnamed cluster — per-pair
            // preferredTarget does not compose transitively, so union order alone
            // would pick the wrong (possibly unnamed) root.
            let keep = preferredRoot(rt, rs)
            parent[keep == rt ? rs : rt] = keep
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
            let q = try writeQueue()
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
            reportError("Batch merge failed: \(error)")
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
        try writeQueue()
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
    /// Item 5: (url, tags) for every file carrying ≥1 keyword tag, so the
    /// "Apply tags" action can write them onto the files as Finder tags (making
    /// Spotlight + Finder search work). Aggregated per file — one FS write each.
    public func filesWithKeywordTags() -> [(url: URL, tags: [String])] {
        guard let q = queue else { return [] }
        do {
            return try q.read { db in
                let rows = try Row.fetchAll(db, sql: """
                    SELECT files.path_text AS path, tags.tag AS tag
                    FROM tags
                    INNER JOIN files ON files.id = tags.file_id
                    WHERE files.failed = 0 AND tags.tag IS NOT NULL AND tags.tag != ''
                    ORDER BY files.id, tags.rowid
                    """)
                var byPath: [String: [String]] = [:]
                var order: [String] = []
                for r in rows {
                    guard let path: String = r["path"], let tag: String = r["tag"] else { continue }
                    if byPath[path] == nil { order.append(path) }
                    byPath[path, default: []].append(tag)
                }
                return order.map { (url: URL(fileURLWithPath: $0), tags: byPath[$0] ?? []) }
            }
        } catch {
            reportError("filesWithKeywordTags failed: \(error)")
            return []
        }
    }

    /// Item 5: the person's name for file tagging — the non-empty
    /// [title, first, middle, last, suffix] joined by single spaces, else the
    /// legacy `name`. Byte-faithful with the Windows `ReadStore.FormatPersonTagName`
    /// so a person is tagged IDENTICALLY on both platforms. (Deliberately NOT
    /// `PersonRow.displayName`, which has a suffix double-space quirk and a
    /// "Person N" fallback that must never become a file tag.)
    static func personTagName(_ p: PersonRow) -> String {
        let parts = [p.title, p.firstName, p.middleName, p.lastName, p.suffix]
            .compactMap { $0?.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        if !parts.isEmpty { return parts.joined(separator: " ") }
        return (p.name ?? "").trimmingCharacters(in: .whitespaces)
    }

    /// Item 5: (url, names) for every file containing ≥1 NAMED person, so the
    /// "Apply people as tags" action can write the person names onto the files.
    /// Skips Unknown / unnamed clusters. Aggregated per file.
    public func filesWithPersonTags() -> [(url: URL, names: [String])] {
        let named = persons(includeUnknown: false).filter { $0.hasAnyName && !$0.isUnknown }
        guard !named.isEmpty else { return [] }
        var byPath: [String: [String]] = [:]
        var order: [String] = []
        for person in named {
            let name = Self.personTagName(person)
            guard !name.isEmpty else { continue }
            for file in files(forPersonID: person.id, limit: 1_000_000) {
                let path = file.pathText
                if byPath[path] == nil { order.append(path) }
                if !(byPath[path]?.contains(name) ?? false) {
                    byPath[path, default: []].append(name)
                }
            }
        }
        return order.map { (url: URL(fileURLWithPath: $0), names: byPath[$0] ?? []) }
    }

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
            reportError("Proposed-name query failed: \(error)")
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
            queue = try writeQueue()
        } catch {
            reportError("DB open for rename failed: \(error)")
            return BulkRenameResult(renamed: [], failed: files.count,
                                    firstError: lastErrorSnapshot())
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
                if firstError == nil { firstError = lastErrorSnapshot() }
            }
        }
        // R5-03: one observation invalidation + counter refresh for the whole
        // batch (the per-file notifyChanged was removed from applyProposedName).
        if !renamed.isEmpty { self.notifyChanged() }
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
                    let q = try writeQueue()
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
            // R5-03: the private overload no longer fires notifyChanged per call;
            // fire it once here for the single-file path.
            let result = applyProposedName(file: file, on: try writeQueue())
            if result != nil { self.notifyChanged() }
            return result
        } catch {
            reportError("DB open for rename failed: \(error)")
            return nil
        }
    }

    // Every app write connection sets a busy timeout. The engine is the
    // single writer; a People edit / Cleanup delete / rename that lands
    // during a brief engine WAL write would otherwise hit SQLITE_BUSY and
    // throw — silently no-opping the edit, or stranding a trashed file as
    // a ghost DB row. The timeout makes the contended write retry instead.
    private func writeQueue() throws -> DatabaseQueue {
        var config = Configuration()
        config.busyMode = .timeout(5)
        return try DatabaseQueue(path: dbURL.path, configuration: config)
    }

    /// True when both URLs resolve to the same on-disk file. Uses the file
    /// resource identifier (inode), not a string compare, so a case-only rename
    /// on a case-insensitive volume is recognized as the file's own slot while
    /// two genuinely distinct files on a case-sensitive volume are not. Only
    /// consulted when fileExists(target) is already true (oldURL always exists as
    /// the move source), so resourceValues won't be missing. (R5-08)
    private func sameFileOnDisk(_ a: URL, _ b: URL) -> Bool {
        if a == b { return true }
        guard
            let ia = (try? a.resourceValues(forKeys: [.fileResourceIdentifierKey]))?.fileResourceIdentifier,
            let ib = (try? b.resourceValues(forKeys: [.fileResourceIdentifierKey]))?.fileResourceIdentifier
        else { return false }
        return ia.isEqual(ib)
    }

    private func applyProposedName(file: FileRow, on queue: DatabaseQueue) -> URL? {
        guard let proposed = file.vlmProposedName, !proposed.isEmpty else { return nil }
        let oldURL = file.url
        let dir = oldURL.deletingLastPathComponent()
        let ext = oldURL.pathExtension
        let baseName = ext.isEmpty ? proposed : "\(proposed).\(ext)"
        var target = dir.appendingPathComponent(baseName)
        var bump = 2
        // R5-08: identity (inode) check, NOT `target != oldURL` — on a
        // case-insensitive volume a case-only rename (the engine sanitizer always
        // lowercases) makes fileExists(target) true while the case-sensitive
        // `target != oldURL` is also true, so the loop wrongly bumped the file's
        // own slot to `_2`. A genuine two-distinct-files collision on a
        // case-sensitive volume still bumps (different inode). The line-1370 guard
        // below stays a case-sensitive compare so the case-only rename proceeds.
        while FileManager.default.fileExists(atPath: target.path) && !sameFileOnDisk(target, oldURL) {
            let bumped = ext.isEmpty ? "\(proposed)_\(bump)" : "\(proposed)_\(bump).\(ext)"
            target = dir.appendingPathComponent(bumped)
            bump += 1
            if bump > 99 { return nil }
        }
        guard target != oldURL else { return oldURL }
        do {
            try FileManager.default.moveItem(at: oldURL, to: target)
        } catch {
            reportError("Rename failed: \(error.localizedDescription)")
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
            // R5-03: notifyChanged() is fired ONCE by the caller (per batch in
            // applyProposedNamesBulk, or in the single-file public wrapper) — not
            // per file, which flooded the MainActor with N invalidations.
            return target
        } catch {
            // DB update failed after the on-disk move — roll the file back so
            // disk and DB stay consistent and the rename remains undoable.
            reportError("DB update after rename failed: \(error)")
            try? FileManager.default.moveItem(at: target, to: oldURL)
            return nil
        }
    }
}

/// Bounded top-K selector for cosine ranking. Retains at most `capacity`
/// scored ids in a min-heap keyed by rank (score desc, row-order asc): the
/// root is the lowest-ranked kept item, so an incoming item only displaces
/// it when it ranks strictly higher. This caps retained state at O(K)
/// regardless of table size while reproducing exactly the output of a stable
/// sort-by-score-descending truncated to K — row order breaks score ties, the
/// same tie-break a stable sort gives. `order` is unique, so the rank is a
/// strict total order and selection is deterministic.
private struct TopKByCosine {
    struct Item { let id: Int64; let score: Float; let order: Int }
    private var items: [Item] = []
    private let capacity: Int

    init(capacity: Int) {
        self.capacity = max(0, capacity)
        items.reserveCapacity(self.capacity)
    }

    private func ranksAbove(_ a: Item, _ b: Item) -> Bool {
        a.score != b.score ? a.score > b.score : a.order < b.order
    }

    mutating func offer(id: Int64, score: Float, order: Int) {
        guard capacity > 0 else { return }
        let item = Item(id: id, score: score, order: order)
        if items.count < capacity {
            items.append(item)
            siftUp(from: items.count - 1)
        } else if ranksAbove(item, items[0]) {
            // New item outranks the worst kept — replace the root.
            items[0] = item
            siftDown(from: 0)
        }
    }

    // Min-heap on rank: a parent never ranks above its children, so the
    // worst-ranked kept item sits at the root for O(1) comparison in `offer`.
    private mutating func siftUp(from start: Int) {
        var i = start
        while i > 0 {
            let parent = (i - 1) / 2
            guard ranksAbove(items[parent], items[i]) else { break }
            items.swapAt(i, parent)
            i = parent
        }
    }

    private mutating func siftDown(from start: Int) {
        var i = start
        let n = items.count
        while true {
            let l = 2 * i + 1, r = 2 * i + 2
            var worst = i
            if l < n, ranksAbove(items[worst], items[l]) { worst = l }
            if r < n, ranksAbove(items[worst], items[r]) { worst = r }
            if worst == i { break }
            items.swapAt(i, worst)
            i = worst
        }
    }

    /// The kept items, highest rank first — matching `prefix(limit)` of the
    /// prior stable descending sort.
    func sortedDescending() -> [Item] {
        items.sorted { ranksAbove($0, $1) }
    }
}
