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
    @ObservationIgnored private var _queue: (any DatabaseReader)?
    private var queue: (any DatabaseReader)? {
        get { connLock.lock(); defer { connLock.unlock() }; return _queue }
        set { connLock.lock(); defer { connLock.unlock() }; _queue = newValue }
    }
    // Coalesce + throttle state for the off-main counters refresh. The
    // duplicate-group window query is O(N log N) and must not run on the
    // main thread on every scan tick — see `refreshCounters`.
    @ObservationIgnored private let countersLock = NSLock()
    @ObservationIgnored private var countersDirty = false
    @ObservationIgnored private var countersRunning = false
    /// Last duplicate totals and when they were measured. That GROUP BY walks
    /// every file, so it runs at most once per `duplicateInterval`.
    @ObservationIgnored private var duplicateTotals: (groups: Int, bytes: Int64, at: Date)?
    @ObservationIgnored private var duplicateTotalsStale = false
    private static let duplicateInterval: TimeInterval = 5
    @ObservationIgnored private var retryTask: Task<Void, Never>?
    @ObservationIgnored private var retryAttempts = 0
    private let dbURL: URL
    public private(set) var version: Int = 0
    /// Bumps each time the read connection opens. A launch before the engine has created the catalog's -wal/-shm files cannot open it read-only, so the first listing reads nothing; views reload on this.
    public private(set) var readerEpoch: Int = 0
    public private(set) var totalFiles: Int = 0
    /// False until the first counters read lands, and again while the reader cannot open or the counters read fails;
    /// `totalFiles` is then a placeholder, not a count of an empty library.
    public private(set) var countersAvailable = false
    /// `totalFiles` once it is a real count; nil while it is unknown.
    var catalogedFileCount: Int? { countersAvailable ? totalFiles : nil }
    public private(set) var totalImages: Int = 0
    /// Files the scan could not decode or read; the Library hides them unless
    /// the Unreadable filter is chosen.
    public private(set) var unreadableFiles: Int = 0
    /// The part of `unreadableFiles` that failed only because the machine was too busy; a later scan can still read them.
    public private(set) var unreadableTimedOutFiles: Int = 0
    /// Files cataloged by name, size and dates only (archives, installers, unknown types).
    public private(set) var otherFiles: Int = 0
    public private(set) var totalDuplicateGroups: Int = 0
    public private(set) var totalReclaimableBytes: Int64 = 0
    public private(set) var hasFacePrints = false
    public private(set) var hasNamedPerson = false
    public private(set) var hasCaptions = false
    public private(set) var lastError: String?
    /// The Library's counters inside the chosen location (`scope`); the whole catalog under
    /// "All locations".
    public private(set) var scopedTotalFiles: Int = 0
    public private(set) var scopedUnreadableFiles: Int = 0
    public private(set) var scopedOtherFiles: Int = 0
    /// Bumps when the scope changes so the Library reloads.
    public private(set) var scopeEpoch: Int = 0
    @ObservationIgnored private let scopeLock = NSLock()
    @ObservationIgnored private var _scope: LibraryScope?
    /// The location the Library lists; nil for "All locations".
    var scope: LibraryScope? {
        scopeLock.lock(); defer { scopeLock.unlock() }
        return _scope
    }

    @MainActor
    func setScope(_ next: LibraryScope?) {
        let changed = scopeLock.withLock { () -> Bool in
            guard _scope != next else { return false }
            _scope = next
            return true
        }
        guard changed else { return }
        scopeEpoch &+= 1
        refreshCounters()
    }

    // `lastError` is read by SwiftUI on the main actor but reported from
    // off-main reads, so every write hops to the main actor.
    private func reportError(_ message: String?) {
        Task { @MainActor in self.lastError = message }
    }

    public init(dbURL: URL = ReadStore.defaultDBURL) {
        self.dbURL = dbURL
    }

    public static var defaultDBURL: URL {
        if let path = ProcessInfo.processInfo.environment["FILEID_DATABASE_PATH"], !path.isEmpty {
            return URL(fileURLWithPath: path)
        }
        return AppSupportPath.fileID.appendingPathComponent("fileid.sqlite")
    }

    private static let suppressedDisplayTags: Set<String> = [
        "image", "photo", "picture", "photography", "shot", "view",
        "object", "item", "background", "foreground", "indoor", "outdoor"
    ]

    /// Idempotent. Safe to call after engine creates / migrates the DB.
    public func openIfPossible() {
        guard FileManager.default.fileExists(atPath: dbURL.path) else {
            self.queue = nil
            self.countersAvailable = true
            self.totalFiles = 0
            self.totalImages = 0
            self.unreadableFiles = 0
            self.unreadableTimedOutFiles = 0
            self.otherFiles = 0
            self.totalDuplicateGroups = 0
            self.totalReclaimableBytes = 0
            self.hasFacePrints = false
            self.hasNamedPerson = false
            self.hasCaptions = false
            return
        }
        if queue == nil {
            do {
                try ReadOnlyLocations.requireWritable(dbURL)
                self.queue = try Self.openReader(at: dbURL)
                Task { @MainActor in self.readerEpoch &+= 1 }
            } catch {
                reportError("Could not open DB: \(error)")
                self.countersAvailable = false
                scheduleRetry()
                return
            }
        }
        refreshCounters()
    }

    /// A reader that cannot open the catalog, or a counters read that fails while the engine is still creating or
    /// migrating it, is asked again with a growing delay until it works, so a launch never stays on a placeholder 0.
    private func scheduleRetry() {
        countersLock.withLock {
            guard retryTask == nil else { return }
            let delay = min(5.0, 0.5 * pow(2.0, Double(retryAttempts)))
            retryAttempts += 1
            retryTask = Task { [weak self] in
                try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
                guard !Task.isCancelled, let self else { return }
                self.countersLock.withLock { self.retryTask = nil }
                await MainActor.run { self.openIfPossible() }
            }
        }
    }

    private func cancelRetry(resetBackoff: Bool) {
        countersLock.withLock {
            retryTask?.cancel()
            retryTask = nil
            if resetBackoff { retryAttempts = 0 }
        }
    }

    public func notifyChanged() {
        // Callers include detached tasks; `version` is read by SwiftUI on the
        // main actor, so the increment is serialized there.
        Task { @MainActor in self.version &+= 1 }
        refreshCounters()
    }

    /// Explicit teardown. Drops our reference to the read connection
    /// under the lock; ARC keeps the underlying SQLite connection alive
    /// until every in-flight read (each holding a strong `q`) drains, so
    /// closing never frees the connection out from under a reader.
    public func close() {
        cancelRetry(resetBackoff: true)
        queue = nil
    }

    private struct CounterSnapshot {
        let totalFiles: Int
        let totalImages: Int
        let unreadableFiles: Int
        let unreadableTimedOutFiles: Int
        let otherFiles: Int
        let scopedTotalFiles: Int
        let scopedUnreadableFiles: Int
        let scopedOtherFiles: Int
        let totalDuplicateGroups: Int
        let totalReclaimableBytes: Int64
        let hasFacePrints: Bool
        let hasNamedPerson: Bool
        let hasCaptions: Bool
    }

    private enum CounterResult {
        case ok(CounterSnapshot)
        case failure(String)
        case noDatabase
    }

    /// Schedules a counters refresh off the main thread. The duplicate-group window query is O(N log N) and a live scan calls notifyChanged up to once a second, so one background worker coalesces bursts and throttles the heavy query.
    func refreshCounters() {
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
                // `withLock` instead of lock()/unlock(): NSLock's lock() is unavailable in async contexts under Swift 6.
                // nil: stop. Otherwise how long to wait first, which is non-zero
                // only when the sole work left is a deferred duplicate total.
                let wait = self.countersLock.withLock { () -> TimeInterval? in
                    if self.countersDirty {
                        self.countersDirty = false
                        return 0
                    }
                    guard self.duplicateTotalsStale, let last = self.duplicateTotals else {
                        self.countersRunning = false
                        return nil
                    }
                    return max(0.05, Self.duplicateInterval - Date().timeIntervalSince(last.at))
                }
                guard let wait else { return }
                if wait > 0 {
                    try? await Task.sleep(nanoseconds: UInt64(wait * 1_000_000_000))
                    self.countersLock.withLock { self.countersDirty = false }
                }

                switch self.computeCounters() {
                case .ok(let snapshot):
                    self.cancelRetry(resetBackoff: true)
                    await MainActor.run { self.applyCounters(snapshot) }
                case .failure(let message):
                    await MainActor.run {
                        self.lastError = message
                        self.countersAvailable = false
                    }
                    self.scheduleRetry()
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
        let scope = self.scope
        do {
            return try q.read { db -> CounterResult in
                let totalFiles  = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE \(Self.notEngineHiddenSQL)") ?? 0
                let totalImages = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE kind = 'image' AND failed = 0") ?? 0
                let unreadableFiles = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE failed = 1 AND \(Self.unreadableSQL)") ?? 0
                let unreadableTimedOutFiles = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE failed = 1 AND \(Self.unreadableSQL) AND \(Self.timedOutSQL)") ?? 0
                let otherFiles = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE kind = 'other' AND failed = 0") ?? 0
                var scoped = (total: totalFiles, unreadable: unreadableFiles, other: otherFiles)
                if let scope {
                    let within = LibraryScope.sql("path_text")
                    scoped = ((try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE \(Self.notEngineHiddenSQL) AND \(within)", arguments: [scope.param]) ?? 0),
                              try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE failed = 1 AND \(Self.unreadableSQL) AND \(within)", arguments: [scope.param]) ?? 0,
                              try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE kind = 'other' AND failed = 0 AND \(within)", arguments: [scope.param]) ?? 0)
                }

                // Fast stored-hash candidate metrics for Settings only. Cleanup itself
                // performs bounded live full-file verification before display and Trash.
                // Copies in a group share content hash AND size, so the space
                // a cleanup frees is (copies - 1) x size whichever one is kept.
                // Grouping first avoids ranking every file (4.6x faster at 100k).
                let cached = self.countersLock.withLock { self.duplicateTotals }
                let groups: Int
                let reclaimableBytes: Int64
                if let cached, Date().timeIntervalSince(cached.at) < Self.duplicateInterval {
                    groups = cached.groups
                    reclaimableBytes = cached.bytes
                    self.countersLock.withLock { self.duplicateTotalsStale = true }
                } else {
                    let dupRow = try Row.fetchOne(db, sql: """
                        SELECT COUNT(*) AS groups, COALESCE(SUM((n - 1) * size_bytes), 0) AS reclaimable
                        FROM (
                            SELECT size_bytes, COUNT(*) AS n
                            FROM files
                            WHERE content_hash IS NOT NULL AND failed = 0
                            GROUP BY content_hash, size_bytes
                            HAVING COUNT(*) > 1
                        )
                        """)
                    groups = dupRow?["groups"] ?? 0
                    reclaimableBytes = dupRow?["reclaimable"] ?? 0
                    self.countersLock.withLock {
                        self.duplicateTotals = (groups, reclaimableBytes, Date())
                        self.duplicateTotalsStale = false
                    }
                }
                let hasFacePrints = try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM face_prints)") ?? false
                let hasNamedPerson = try Bool.fetchOne(db, sql: """
                    SELECT EXISTS(SELECT 1 FROM persons WHERE IFNULL(is_unknown, 0) = 0 AND \(Self.personHasNameSQL()))
                    """) ?? false
                let hasCaptions = try Bool.fetchOne(db, sql: """
                    SELECT EXISTS(SELECT 1 FROM files WHERE failed = 0 AND vlm_proposed_name IS NOT NULL AND vlm_proposed_name <> '')
                    """) ?? false
                return .ok(CounterSnapshot(
                    totalFiles: totalFiles,
                    totalImages: totalImages,
                    unreadableFiles: unreadableFiles,
                    unreadableTimedOutFiles: unreadableTimedOutFiles,
                    otherFiles: otherFiles,
                    scopedTotalFiles: scoped.total,
                    scopedUnreadableFiles: scoped.unreadable,
                    scopedOtherFiles: scoped.other,
                    totalDuplicateGroups: groups,
                    totalReclaimableBytes: reclaimableBytes,
                    hasFacePrints: hasFacePrints,
                    hasNamedPerson: hasNamedPerson,
                    hasCaptions: hasCaptions
                ))
            }
        } catch {
            return .failure("Counters refresh failed: \(error)")
        }
    }

    @MainActor
    private func applyCounters(_ snapshot: CounterSnapshot) {
        // Observation notifies on every set, changed or not; unchanged counters stay quiet.
        if !self.countersAvailable { self.countersAvailable = true }
        if self.lastError != nil { self.lastError = nil }
        if self.totalFiles != snapshot.totalFiles { self.totalFiles = snapshot.totalFiles }
        if self.totalImages != snapshot.totalImages { self.totalImages = snapshot.totalImages }
        if self.unreadableFiles != snapshot.unreadableFiles { self.unreadableFiles = snapshot.unreadableFiles }
        if self.unreadableTimedOutFiles != snapshot.unreadableTimedOutFiles { self.unreadableTimedOutFiles = snapshot.unreadableTimedOutFiles }
        if self.otherFiles != snapshot.otherFiles { self.otherFiles = snapshot.otherFiles }
        if self.scopedTotalFiles != snapshot.scopedTotalFiles { self.scopedTotalFiles = snapshot.scopedTotalFiles }
        if self.scopedUnreadableFiles != snapshot.scopedUnreadableFiles { self.scopedUnreadableFiles = snapshot.scopedUnreadableFiles }
        if self.scopedOtherFiles != snapshot.scopedOtherFiles { self.scopedOtherFiles = snapshot.scopedOtherFiles }
        if self.totalDuplicateGroups != snapshot.totalDuplicateGroups { self.totalDuplicateGroups = snapshot.totalDuplicateGroups }
        if self.totalReclaimableBytes != snapshot.totalReclaimableBytes { self.totalReclaimableBytes = snapshot.totalReclaimableBytes }
        if self.hasFacePrints != snapshot.hasFacePrints { self.hasFacePrints = snapshot.hasFacePrints }
        if self.hasNamedPerson != snapshot.hasNamedPerson { self.hasNamedPerson = snapshot.hasNamedPerson }
        if self.hasCaptions != snapshot.hasCaptions { self.hasCaptions = snapshot.hasCaptions }
    }

    // MARK: - Library queries

    /// `kindFilter` value that lists the files the scan could not read.
    public static let unreadableFilter = "unreadable"
    /// Every failed row the engine did not hide, including ones the next scan will retry, so the Library agrees with the scan's failed-file count.
    static let unreadableSQL = notEngineHiddenSQL
    /// Stored `error_message` of a row that failed because the machine was too busy (`tagging.rs` `TIMEOUT_MESSAGES`, and the "ran out of time" wording that replaces them).
    nonisolated static let timedOutMessages = ["per-file timeout after 60s", "Vision pass timed out"]
    nonisolated static let timedOutSQL =
        "(COALESCE(error_message, '') IN (\(timedOutMessages.map { "'" + $0 + "'" }.joined(separator: ", "))) OR COALESCE(error_message, '') LIKE '%ran out of time%')"
    nonisolated static func isTimedOut(errorMessage: String?) -> Bool {
        guard let errorMessage else { return false }
        return timedOutMessages.contains(errorMessage) || errorMessage.contains("ran out of time")
    }
    /// The two `failed = 1` reasons the Rust engine writes for a row that is not a broken file: one removed from
    /// its folder (kept 30 days so a returning file re-binds) and an empty one. Same exact strings as
    /// `scan_session.rs` `HIDDEN_ROW_MESSAGE` and `db/zero_byte.rs` `ZERO_BYTE_REASON`.
    nonisolated static let engineHiddenMessages = [
        "File is no longer present under the completed scan root.",
        "File is zero bytes and has no indexable content.",
    ]
    /// SQL fragment that keeps a row out of totals and the Unreadable list when the engine hid it.
    nonisolated static let notEngineHiddenSQL =
        "COALESCE(error_message, '') NOT IN (\(engineHiddenMessages.map { "'" + $0 + "'" }.joined(separator: ", ")))"

    public func files(offset: Int = 0, limit: Int = 200,
                      search: String = "",
                      kindFilter: String? = nil,
                      sort: LibrarySort = .standard) -> [FileRow] {
        guard let q = queue else { return [] }
        let scope = self.scope
        do {
            return try q.read { db in
                try Self.fetchFiles(db, offset: offset, limit: limit, after: nil, search: search, kindFilter: kindFilter, scope: scope, sort: sort).rows
            }
        } catch {
            reportError("Library query failed: \(error)")
            return []
        }
    }

    /// Rows for the Library grid. Search and sort run over ids only, so a top-N sorter never carries whole rows; the page's rows are fetched by id after. `after` continues from the last row of the previous page by its sort key; without it `offset` skips rows. The cursor is nil when the catalog has no `name_sort` key yet and the sort is Name or Kind, so the caller falls back to `offset`.
    private static func fetchFiles(_ db: Database, offset: Int, limit: Int, after: LibraryCursor?, search: String,
                                   kindFilter: String?, scope: LibraryScope?, sort: LibrarySort) throws -> LibraryPage {
        let unreadable = kindFilter == Self.unreadableFilter
        let keyed = try db.columns(in: "files").contains { $0.name == "name_sort" }
        var base = unreadable ? "failed = 1 AND \(Self.unreadableSQL)" : "failed = 0"
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
            let normalized = trimmedSearch.precomposedStringWithCanonicalMapping
            let escapedSearch = normalized
                .replacingOccurrences(of: "\\", with: "\\\\")
                .replacingOccurrences(of: "%", with: "\\%")
                .replacingOccurrences(of: "_", with: "\\_")
            let like = "%\(escapedSearch)%"
            let ftsQuery = FTSQuery.quoted(trimmedSearch)
            // The trigram index (v39) answers a substring of three or more characters from an index; shorter ones and older catalogs scan with LIKE, which matches the same rows.
            let trigram = normalized.count >= 3 ? try db.tableExists("files_search_fts") : false
            let nameMatch: String
            let nameArgs: StatementArguments
            if trigram {
                nameMatch = "id IN (SELECT rowid FROM files_search_fts WHERE files_search_fts MATCH ?)"
                let quote = "\u{22}"
                nameArgs = [quote + normalized.replacingOccurrences(of: quote, with: quote + quote) + quote]
            } else {
                nameMatch = """
                    path_search LIKE ? ESCAPE '\\'
                          OR vlm_proposed_name LIKE ? ESCAPE '\\'
                          OR vlm_description LIKE ? ESCAPE '\\'
                    """
                nameArgs = [like, like, like]
            }
            // The distinct tags (v27, kept by triggers) are scanned for the text, then the tags index finds the files.
            let tagMatch = try db.tableExists("catalog_tag_vocab")
                ? "id IN (SELECT file_id FROM tags WHERE tag IN (SELECT tag FROM catalog_tag_vocab WHERE tag LIKE ? ESCAPE '\\'))"
                : "id IN (SELECT file_id FROM tags WHERE tag LIKE ? ESCAPE '\\')"
            base += """
                 AND (
                      id IN (SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH ?)
                      OR id IN (SELECT rowid FROM doc_fts WHERE doc_fts MATCH ?)
                      OR \(nameMatch)
                      OR \(tagMatch)
                      OR id IN (
                          SELECT face_prints.file_id FROM face_prints
                          INNER JOIN persons ON persons.id = face_prints.person_id
                          WHERE persons.name LIKE ? ESCAPE '\\'
                             OR persons.first_name LIKE ? ESCAPE '\\'
                             OR persons.last_name LIKE ? ESCAPE '\\'
                             OR persons.middle_name LIKE ? ESCAPE '\\'
                             OR persons.title LIKE ? ESCAPE '\\'
                             OR persons.suffix LIKE ? ESCAPE '\\'
                      )
                    )
                """
            args += [ftsQuery, ftsQuery]
            args += nameArgs
            args += [like, like, like, like, like, like, like]
        }
        if let k = kindFilter, !unreadable {
            base += " AND kind = ?"
            args += [k]
        }
        if let scope {
            base += " AND " + LibraryScope.sql("path_text")
            args += [scope.param]
        }
        var ids: [Int64] = []
        var cursor: LibraryCursor?
        if offset > 0, sort.key == .dateTaken {
            var skip = offset
            for segment in sort.segments(after: nil, keyed: keyed) where ids.count < limit {
                let counted = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE \(base) AND \(segment.condition)", arguments: args + StatementArguments(segment.arguments)) ?? 0
                if skip >= counted { skip -= counted; continue }
                let sql = "SELECT id FROM files WHERE \(base) AND \(segment.condition) ORDER BY \(segment.order) LIMIT ? OFFSET ?"
                ids += try Int64.fetchAll(db, sql: sql, arguments: args + StatementArguments(segment.arguments) + [limit - ids.count, skip])
                skip = 0
            }
        } else if offset > 0 {
            let sql = "SELECT id FROM files WHERE \(base) ORDER BY \(sort.orderSQL(keyed: keyed)) LIMIT ? OFFSET ?"
            ids = try Int64.fetchAll(db, sql: sql, arguments: args + [limit, offset])
        } else {
            let columns = sort.cursorColumns(keyed: keyed)
            var last: Row?
            for segment in sort.segments(after: after, keyed: keyed) where ids.count < limit {
                let select = (["id"] + (columns ?? [])).enumerated().map { "\($1) AS c\($0)" }.joined(separator: ", ")
                let sql = "SELECT \(select) FROM files WHERE \(base) AND \(segment.condition) ORDER BY \(segment.order) LIMIT ?"
                let rows = try Row.fetchAll(db, sql: sql, arguments: args + StatementArguments(segment.arguments) + [limit - ids.count])
                ids += rows.map { $0["c0"] as Int64 }
                if let row = rows.last { last = row }
            }
            if let last, columns != nil {
                cursor = sort.cursor(id: last["c0"], values: (1..<(columns!.count + 1)).map { last["c\($0)"] as DatabaseValue })
            }
        }
        guard !ids.isEmpty else { return LibraryPage(rows: [], cursor: nil) }
        let encoded = String(decoding: try JSONEncoder().encode(ids), as: UTF8.self)
        let rows = try Row.fetchAll(db, sql: "SELECT * FROM files WHERE id IN (SELECT value FROM json_each(?))", arguments: [encoded])
        let byID = Dictionary(rows.map { ($0["id"] as Int64, Self.toFileRow($0)) }, uniquingKeysWith: { first, _ in first })
        return LibraryPage(rows: ids.compactMap { byID[$0] }, cursor: cursor)
    }

    /// Off-main twin of `files(...)`: the keyword search is a multi-table FTS + LIKE + face-join query that would stutter the UI on every throttled scan batch.
    public func filesAsync(offset: Int = 0, limit: Int = 200,
                           search: String = "",
                           kindFilter: String? = nil,
                           sort: LibrarySort = .standard) async -> [FileRow] {
        await filesPageAsync(offset: offset, limit: limit, search: search, kindFilter: kindFilter, sort: sort).rows
    }

    /// The page plus the cursor the next page continues from.
    public func filesPageAsync(offset: Int = 0, limit: Int = 200, after: LibraryCursor? = nil,
                               search: String = "",
                               kindFilter: String? = nil,
                               sort: LibrarySort = .standard) async -> LibraryPage {
        guard let q = queue else { return LibraryPage(rows: [], cursor: nil) }
        let scope = self.scope
        do {
            // GRDB's async read interrupts the query when the calling task is
            // cancelled, so a superseded search stops instead of running out.
            return try await q.read { db in
                try Self.fetchFiles(db, offset: offset, limit: limit, after: after, search: search, kindFilter: kindFilter, scope: scope, sort: sort)
            }
        } catch {
            if !(error is CancellationError), !Task.isCancelled { reportError("Library query failed: \(error)") }
            return LibraryPage(rows: [], cursor: nil)
        }
    }

    /// The listing's rows for these ids as they are now; an id that left the listing is missing from the result.
    public func listedRowsAsync(ids: [Int64], kindFilter: String? = nil) async -> [Int64: FileRow] {
        guard !ids.isEmpty, let q = queue else { return [:] }
        let scope = self.scope
        return (try? await q.read { db in
            let unreadable = kindFilter == Self.unreadableFilter
            var sql = "SELECT * FROM files WHERE id IN (SELECT value FROM json_each(?)) AND " + (unreadable ? "failed = 1 AND \(Self.unreadableSQL)" : "failed = 0")
            var args: StatementArguments = [String(decoding: try JSONEncoder().encode(ids), as: UTF8.self)]
            if let kindFilter, !unreadable { sql += " AND kind = ?"; args += [kindFilter] }
            if let scope { sql += " AND " + LibraryScope.sql("path_text"); args += [scope.param] }
            return Dictionary(try Row.fetchAll(db, sql: sql, arguments: args).map { ($0["id"] as Int64, Self.toFileRow($0)) }, uniquingKeysWith: { first, _ in first })
        }) ?? [:]
    }

    /// How many rows the unsearched listing has, nil when the query fails.
    public func listingCountAsync(kindFilter: String? = nil) async -> Int? {
        guard let q = queue else { return nil }
        let scope = self.scope
        return try? await q.read { db in
            let unreadable = kindFilter == Self.unreadableFilter
            var sql = "SELECT COUNT(*) FROM files WHERE " + (unreadable ? "failed = 1 AND \(Self.unreadableSQL)" : "failed = 0")
            var args: StatementArguments = []
            if let kindFilter, !unreadable { sql += " AND kind = ?"; args += [kindFilter] }
            if let scope { sql += " AND " + LibraryScope.sql("path_text"); args += [scope.param] }
            return try Int.fetchOne(db, sql: sql, arguments: args) ?? 0
        }
    }

    public func catalogFilesAsync(ids: [Int64], kindFilter: String? = nil) async -> [FileRow] {
        guard !ids.isEmpty, ids.count <= 100, let q = queue else { return [] }
        let scope = self.scope
        return await Task.detached(priority: .userInitiated) {
            (try? q.read { db in
                let encoded = String(decoding: try JSONEncoder().encode(ids), as: UTF8.self)
                let rows = try Row.fetchAll(db, sql: "SELECT * FROM files WHERE failed=0 AND id IN (SELECT value FROM json_each(?))", arguments: [encoded])
                let byID = Dictionary(uniqueKeysWithValues: rows.map { (($0["id"] as Int64), Self.toFileRow($0)) })
                var seen = Set<Int64>()
                return ids.compactMap { id -> FileRow? in
                    guard seen.insert(id).inserted, let row = byID[id], kindFilter == nil || row.kind == kindFilter,
                          scope?.contains(row.pathText) ?? true else { return nil }
                    return row
                }
            }) ?? []
        }.value
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
        let tags = (try? q.read { db in
            try String.fetchAll(db, sql: """
                SELECT TRIM(tag) FROM tags
                WHERE file_id = ? AND source IN ('auto', 'user', 'vlm')
                  AND TRIM(tag) <> ''
                  AND LOWER(TRIM(tag)) NOT IN ('image', 'photo', 'picture', 'photography', 'shot', 'view', 'object', 'item', 'background', 'foreground', 'indoor', 'outdoor')
                ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, COALESCE(score, 0) DESC, rowid ASC
                """, arguments: [id])
        }) ?? []
        return Self.uniqueDisplayTags(tags)
    }

    public struct TimelineSpan: Sendable, Hashable {
        public let start: Double
        public let end: Double
        public let text: String
    }

    public struct TimelineTracks: Sendable, Equatable {
        public var examined: [TimelineSpan] = []
        public var speech: [TimelineSpan] = []
        public var scenes: [TimelineSpan] = []
        public static let empty = TimelineTracks()
    }

    /// What the timeline analysis has looked at in a video: sampled frame
    /// times (Examined), transcribed speech passages (Speech) and the
    /// described scenes. Stale rows
    /// from an older revision of the file are left out.
    public func timelineTracks(forFileID id: Int64) -> TimelineTracks {
        guard let q = queue else { return .empty }
        return (try? q.read { db -> TimelineTracks in
            let examined = try Row.fetchAll(db, sql: """
                SELECT start_seconds, end_seconds FROM catalog_coverage
                WHERE file_id = ? AND status IN ('sampled', 'verified') AND model_version NOT LIKE 'apple-speech-%'
                ORDER BY start_seconds LIMIT 4000
                """, arguments: [id]).map { TimelineSpan(start: $0["start_seconds"], end: $0["end_seconds"], text: "") }
            let speech = try Row.fetchAll(db, sql: """
                SELECT start_seconds, COALESCE(end_seconds, start_seconds) AS end_seconds, text FROM catalog_passages
                WHERE file_id = ? AND stale = 0 AND start_seconds IS NOT NULL AND id LIKE 'speech:%'
                ORDER BY start_seconds LIMIT 4000
                """, arguments: [id]).map { TimelineSpan(start: $0["start_seconds"], end: $0["end_seconds"], text: $0["text"]) }
            let scenes = try Row.fetchAll(db, sql: """
                SELECT start_seconds, COALESCE(end_seconds, start_seconds) AS end_seconds, text FROM catalog_passages
                WHERE file_id = ? AND stale = 0 AND start_seconds IS NOT NULL AND id LIKE 'scene:%'
                ORDER BY start_seconds LIMIT 200
                """, arguments: [id]).map { TimelineSpan(start: $0["start_seconds"], end: $0["end_seconds"], text: $0["text"]) }
            return TimelineTracks(examined: examined, speech: speech, scenes: scenes)
        }) ?? .empty
    }

    /// Named people whose faces appear in the file; unnamed clusters are skipped.
    public func personNames(forFileID id: Int64) -> [String] {
        guard let q = queue else { return [] }
        let rows = (try? q.read { db in
            try Row.fetchAll(db, sql: """
                SELECT DISTINCT p.id, p.name, p.title, p.first_name, p.middle_name, p.last_name, p.suffix
                FROM face_prints f JOIN persons p ON p.id = f.person_id
                WHERE f.file_id = ? AND COALESCE(p.is_unknown, 0) = 0
                ORDER BY p.id
                """, arguments: [id])
        }) ?? []
        var seen = Set<String>()
        return rows.compactMap { row in
            let name = Self.personTagName(row: row)
            return name.isEmpty || !seen.insert(name.lowercased()).inserted ? nil : name
        }
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
            let rows = try Row.fetchAll(db, sql: """
                SELECT t.file_id, TRIM(t.tag) AS tag
                FROM tags t
                WHERE t.file_id IN (\(placeholders))
                  AND t.source IN ('auto', 'user', 'vlm')
                  AND TRIM(t.tag) <> ''
                  AND LOWER(TRIM(t.tag)) NOT IN ('image', 'photo', 'picture', 'photography', 'shot', 'view', 'object', 'item', 'background', 'foreground', 'indoor', 'outdoor')
                ORDER BY t.file_id ASC,
                         CASE t.source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END,
                         COALESCE(t.score, 0) DESC,
                         t.rowid ASC
                """, arguments: StatementArguments(args))
            var out: [Int64: [String]] = [:]
            var seen: [Int64: Set<String>] = [:]
            out.reserveCapacity(ids.count)
            for r in rows {
                guard let fid: Int64 = r["file_id"],
                      let tag: String = r["tag"] else { continue }
                guard out[fid, default: []].count < limit else { continue }
                let key = tag.lowercased()
                guard !Self.suppressedDisplayTags.contains(key),
                      seen[fid, default: []].insert(key).inserted else { continue }
                out[fid, default: []].append(tag)
            }
            return out
        }) ?? [:]
    }

    private static func uniqueDisplayTags(_ tags: [String]) -> [String] {
        var seen = Set<String>()
        return tags.filter { seen.insert($0.lowercased()).inserted }
    }

    // MARK: - Cleanup queries

    struct ExactCandidateLoad: Sendable {
        let files: [FileRow]
        let candidateCount: Int
        let partial: Bool
        var unshownClasses = 0
        var unshownPotentialBytes: Int64 = 0
    }

    func exactDuplicateCandidates() -> ExactCandidateLoad {
        guard let q = queue else {
            return ExactCandidateLoad(files: [], candidateCount: 0, partial: true)
        }
        do {
            return try q.read { db in
                // Candidate selection is recipe-agnostic: membership is the whole same-size class, since stored hashes are never byte-proof (legacy BLAKE3 and current SHA-256 rows and stale hashes must still meet in one class so verify()'s live read can pair them). The stored hash only ranks classes so near-certain duplicates load first, with same-hash twins ranked adjacently so memberCap can't split a pair; memberCap stops a dominant non-duplicate class from starving real groups out of candidateCap. candidateCount is uncapped so the partial-preview disclosure fires whenever a backstop drops candidates.
                let candidateCount = try Int.fetchOne(db, sql: """
                    WITH candidate_sizes AS (
                        SELECT COUNT(*) AS n FROM files
                        WHERE failed = 0 AND \(ExactDuplicateVerifier.eligibleSQL)
                        GROUP BY size_bytes HAVING n > 1
                    )
                    SELECT COALESCE(SUM(n), 0) FROM candidate_sizes
                    """) ?? 0
                let sizeClassTotals = try Row.fetchOne(db, sql: """
                    WITH classes AS (
                        SELECT size_bytes AS s, COUNT(*) AS n FROM files
                        WHERE failed = 0 AND size_bytes >= 0
                        GROUP BY size_bytes HAVING n > 1
                    )
                    SELECT COUNT(*) AS classes, COALESCE(SUM((n - 1) * s), 0) AS bytes FROM classes
                    """)
                let rows = try Row.fetchAll(db, sql: """
                    WITH per_file AS (
                        -- twins: how many OTHER rows share this row's stored
                        -- (content_hash, size). NULL hashes never count as twins
                        -- (SQLite windows partition NULLs together).
                        SELECT id AS pf_id, size_bytes AS pf_size,
                               CASE WHEN content_hash IS NULL THEN 0
                                    ELSE COUNT(*) OVER (
                                        PARTITION BY content_hash, size_bytes
                                    ) - 1 END AS twins
                        FROM files
                        WHERE failed = 0 AND \(ExactDuplicateVerifier.eligibleSQL)
                          AND size_bytes <= \(ExactDuplicateVerifier.maxFileBytes)
                          AND size_bytes IN (
                              SELECT size_bytes FROM files
                              WHERE failed = 0 AND \(ExactDuplicateVerifier.eligibleSQL)
                              GROUP BY size_bytes HAVING COUNT(*) > 1
                          )
                    ),
                    candidate_sizes AS (
                        SELECT pf_size AS gsize, COUNT(*) AS n,
                               MAX(twins) AS hash_twins
                        FROM per_file
                        GROUP BY pf_size HAVING n > 1
                    ),
                    top_groups AS (
                        SELECT gsize, n, hash_twins FROM candidate_sizes
                        ORDER BY (hash_twins > 0) DESC, hash_twins * gsize DESC,
                                 n DESC, gsize ASC
                        LIMIT ?
                    ),
                    ranked AS (
                        SELECT f.*, tg.n AS group_count,
                               tg.hash_twins AS group_hash_twins,
                               ROW_NUMBER() OVER (
                                   PARTITION BY tg.gsize
                                   ORDER BY (pf.twins > 0) DESC,
                                            f.content_hash ASC,
                                            COALESCE(f.created_at, 1e18) ASC,
                                            LENGTH(f.path_text) ASC,
                                            f.path_text ASC
                               ) AS member_rank
                        FROM files f
                        JOIN per_file pf ON pf.pf_id = f.id
                        JOIN top_groups tg ON tg.gsize = f.size_bytes
                        WHERE f.failed = 0
                    )
                    SELECT * FROM ranked
                    WHERE member_rank <= ?
                    ORDER BY (group_hash_twins > 0) DESC,
                             group_hash_twins * size_bytes DESC,
                             group_count DESC, size_bytes ASC, member_rank ASC
                    LIMIT ?
                    """, arguments: [
                        ExactDuplicateVerifier.groupCap,
                        ExactDuplicateVerifier.memberCap,
                        ExactDuplicateVerifier.candidateCap + 1
                    ])
                var files: [FileRow] = []
                files.reserveCapacity(min(rows.count, ExactDuplicateVerifier.candidateCap))
                var selectedBytes: Int64 = 0
                var partial = candidateCount > ExactDuplicateVerifier.candidateCap
                for row in rows.prefix(ExactDuplicateVerifier.candidateCap) {
                    let file = Self.toFileRow(row)
                    let size = max(0, file.sizeBytes)
                    if size > ExactDuplicateVerifier.readBudgetBytes - selectedBytes {
                        partial = true
                        break
                    }
                    selectedBytes += size
                    files.append(file)
                }
                if files.count < min(candidateCount, ExactDuplicateVerifier.candidateCap) {
                    partial = true
                }
                var shownClasses: [Int64: Int64] = [:]
                for file in files where shownClasses[file.sizeBytes] == nil {
                    shownClasses[file.sizeBytes] = (rows.first { ($0["size_bytes"] as Int64?) == file.sizeBytes }?["group_count"] as Int64? ?? 1) - 1
                }
                let shownBytes = shownClasses.reduce(Int64(0)) { $0 + $1.key * max(0, $1.value) }
                let totalClasses: Int = sizeClassTotals?["classes"] ?? 0
                let totalBytes: Int64 = sizeClassTotals?["bytes"] ?? 0
                return ExactCandidateLoad(
                    files: files, candidateCount: candidateCount, partial: partial,
                    unshownClasses: max(0, totalClasses - shownClasses.count),
                    unshownPotentialBytes: max(0, totalBytes - shownBytes))
            }
        } catch {
            reportError("Exact duplicate candidate query failed: \(error)")
            return ExactCandidateLoad(files: [], candidateCount: 0, partial: true)
        }
    }

    func exactDuplicateSnapshotAsync() async -> ExactDuplicateSnapshot {
        let load = await Task.detached(priority: .userInitiated) { [self] in
            exactDuplicateCandidates()
        }.value
        return await ExactDuplicateVerifier.verify(
            candidates: load.files,
            candidateCount: load.candidateCount,
            inputPartial: load.partial,
            unshownClasses: load.unshownClasses,
            unshownPotentialBytes: load.unshownPotentialBytes)
    }

    struct ExactStoredLoad: Sendable {
        let groups: [DuplicateGroup]
        let totalGroups: Int
        let totalExtraCopies: Int
        let totalBytes: Int64
        var shownBytes: Int64 { groups.reduce(0) { $0 + $1.reclaimableBytes } }
    }

    /// The Cleanup list straight from the stored hashes: one indexed query, no file reads. Groups are ranked by what they could free; `ExactDuplicateVerifier.verifyStored` then re-reads what is shown. Totals cover every group, not just the ones listed.
    func exactStoredGroups(offset: Int = 0, limit: Int = ExactDuplicateVerifier.groupCap) -> ExactStoredLoad {
        guard let q = queue else { return ExactStoredLoad(groups: [], totalGroups: 0, totalExtraCopies: 0, totalBytes: 0) }
        do {
            return try q.read { db in
                let totals = try Row.fetchOne(db, sql: """
                    WITH twins AS (
                        SELECT size_bytes AS s, COUNT(*) AS n FROM files
                        WHERE failed = 0 AND content_hash IS NOT NULL AND \(ExactDuplicateVerifier.eligibleSQL)
                        GROUP BY content_hash, size_bytes HAVING n > 1
                    )
                    SELECT COUNT(*) AS groups, COALESCE(SUM(n - 1), 0) AS extra, COALESCE(SUM((n - 1) * s), 0) AS bytes FROM twins
                    """)
                let top = try Row.fetchAll(db, sql: """
                    SELECT content_hash AS h, size_bytes AS s FROM files
                    WHERE failed = 0 AND content_hash IS NOT NULL AND \(ExactDuplicateVerifier.eligibleSQL)
                    GROUP BY content_hash, size_bytes HAVING COUNT(*) > 1
                    ORDER BY (COUNT(*) - 1) * size_bytes DESC, size_bytes DESC, content_hash
                    LIMIT ? OFFSET ?
                    """, arguments: [limit, offset])
                var groups: [DuplicateGroup] = []
                groups.reserveCapacity(top.count)
                for key in top {
                    let hash: Data = key["h"]
                    let size: Int64 = key["s"]
                    let rows = try Row.fetchAll(db, sql: """
                        SELECT * FROM files
                        WHERE content_hash = ? AND size_bytes = ? AND failed = 0 AND \(ExactDuplicateVerifier.eligibleSQL)
                        """, arguments: [hash, size])
                    let files = rows.map(Self.toFileRow).sorted(by: ExactDuplicateVerifier.keeperComesFirst)
                    guard files.count > 1 else { continue }
                    groups.append(DuplicateGroup(
                        id: ExactDuplicateVerifier.groupID(hash),
                        files: Array(files.prefix(ExactDuplicateVerifier.memberCap)),
                        totalFileCount: files.count, totalBytes: Int64(files.count) * size))
                }
                return ExactStoredLoad(
                    groups: groups, totalGroups: totals?["groups"] ?? 0,
                    totalExtraCopies: totals?["extra"] ?? 0, totalBytes: totals?["bytes"] ?? 0)
            }
        } catch {
            reportError("Exact duplicate stored-hash query failed: \(error)")
            return ExactStoredLoad(groups: [], totalGroups: 0, totalExtraCopies: 0, totalBytes: 0)
        }
    }

    func exactStoredGroupsAsync(offset: Int = 0, limit: Int = ExactDuplicateVerifier.groupCap) async -> ExactStoredLoad {
        await Task.detached(priority: .userInitiated) { [self] in exactStoredGroups(offset: offset, limit: limit) }.value
    }

    // MARK: - Perceptual near-duplicate queries

    /// Default Hamming threshold for "visually similar" grouping. `FILEID_NEARDUP_HAMMING`
    /// overrides; clamped to 0...20. 8 of 64 bits ≈ visually near-identical (resize /
    /// re-encode / crop / light edit) — deliberately tight so distinct photos of the
    /// same subject over time do NOT collapse into one group.
    public static var defaultNearDupHamming: Int {
        guard let raw = ProcessInfo.processInfo.environment["FILEID_NEARDUP_HAMMING"],
              let v = Int(raw) else { return 8 }
        return min(20, max(0, v))
    }

    /// Display caps for similar-image grouping: at most 200 groups, 5,000 members in total, 500 per group.
    private static let cleanupMaxGroups = 200
    private static let cleanupMaxVisibleMembers = 5_000
    private static let cleanupMaxVisibleMembersPerGroup = 500

    /// Perceptual near-duplicate groups: images whose 64-bit dHashes are within `maxHamming` of one another, transitively unioned. Byte-exact groups are dropped because the Exact view shows them. Heavy work runs outside the read transaction; use `similarImageGroupsAsync` to keep it off the MainActor.
    public func similarImageGroups(maxHamming: Int) -> [DuplicateGroup] {
        guard let q = queue else { return [] }
        let rows: [Row]
        do {
            // phash == 0 is the engine's "none / failed" sentinel (see `DBWriter` in the engine),
            // so exclude it alongside NULL — otherwise every blank-hash image would
            // collapse into one giant false group. Only the columns grouping needs
            // are read; full rows are fetched for the clustered members afterwards.
            rows = try q.read { db in
                try Row.fetchAll(db, sql: """
                    SELECT id, phash, content_hash, path_text FROM files
                    WHERE kind = 'image' AND failed = 0
                      AND phash IS NOT NULL AND phash != 0
                    ORDER BY id
                    """)
            }
        } catch {
            reportError("Similar-image query failed: \(error)")
            return []
        }

        guard rows.count > 1 else { return [] }
        var pathByID: [Int64: String] = [:]
        pathByID.reserveCapacity(rows.count)
        var contentHashByID: [Int64: Data] = [:]
        var items: [(id: Int64, phash: Int64)] = []
        items.reserveCapacity(rows.count)
        for r in rows {
            guard let ph: Int64 = r["phash"], let id: Int64 = r["id"] else { continue }
            pathByID[id] = r["path_text"]
            if let ch: Data = r["content_hash"] { contentHashByID[id] = ch }
            items.append((id: id, phash: ph))
        }
        let hashByID = Dictionary(uniqueKeysWithValues: items.map { ($0.id, $0.phash) })

        let clusters = PerceptualGrouping.droppingRawTwins(
            PerceptualGrouping.groupByHamming(items, maxHamming: maxHamming),
            paths: pathByID, hashes: hashByID, maxHamming: maxHamming
        ).filter { ids in
            let hashes = ids.map { contentHashByID[$0] }
            return !(!hashes.contains(nil) && Set(hashes.compactMap { $0 }).count == 1)
        }
        return buildSimilarGroups(clusters: clusters, contentHashByID: contentHashByID)
    }

    /// Loads the clustered members as full rows, ranks each group's copies, and applies the display caps. `contentHashByID` drops a group of byte-identical files; an engine-made cluster has had that done already, so it passes none.
    private func buildSimilarGroups(clusters: [[Int64]], trueSizes: [Int] = [], contentHashByID: [Int64: Data]) -> [DuplicateGroup] {
        guard let q = queue, !clusters.isEmpty else { return [] }
        var fileByID: [Int64: FileRow] = [:]
        do {
            let wanted = clusters.flatMap { $0 }
            for start in stride(from: 0, to: wanted.count, by: 5_000) {
                let chunk = Array(wanted[start..<min(start + 5_000, wanted.count)])
                let encoded = String(decoding: try JSONEncoder().encode(chunk), as: UTF8.self)
                let full = try q.read { db in
                    try Row.fetchAll(db, sql: "SELECT * FROM files WHERE id IN (SELECT value FROM json_each(?))", arguments: [encoded])
                }
                for r in full { let fr = Self.toFileRow(r); fileByID[fr.id] = fr }
            }
        } catch {
            reportError("Similar-image query failed: \(error)")
            return []
        }

        var groups: [DuplicateGroup] = []
        var remaining = Self.cleanupMaxVisibleMembers
        for (index, ids) in clusters.enumerated() {
            var files = ids.compactMap { fileByID[$0] }
            guard files.count > 1 else { continue }
            // Drop pure byte-exact clusters — already shown under "Exact".
            let hashes = files.map { contentHashByID[$0.id] }
            let allByteExact = !hashes.contains(nil) && Set(hashes.compactMap { $0 }).count == 1
            if allByteExact { continue }
            files.sort { a, b in
                if (a.aesthetic ?? 0) != (b.aesthetic ?? 0) {
                    return (a.aesthetic ?? 0) > (b.aesthetic ?? 0)
                }
                if a.sizeBytes != b.sizeBytes { return a.sizeBytes > b.sizeBytes }
                let ad = a.createdAt ?? .distantFuture
                let bd = b.createdAt ?? .distantFuture
                if ad != bd { return ad < bd }
                if a.pathText.count != b.pathText.count { return a.pathText.count < b.pathText.count }
                // Final tiebreaker on the unique id: Swift's sort isn't stable, so without it the
                // displayed keeper of two rows tied on every rank key above would change between runs.
                return a.id < b.id
            }
            // Stable Identifiable id: the smallest member file id — independent of
            // which copy currently ranks as keeper, so SwiftUI identity (and the
            // user's skip state) survives a mid-scan re-rank.
            let gid = files.map(\.id).min() ?? files[0].id
            guard remaining >= 2 else { break }
            let cut = index < trueSizes.count ? max(0, trueSizes[index] - ids.count) : 0
            let totalCount = files.count + cut
            let totalBytes = files.reduce(Int64(0)) { $0 + $1.sizeBytes }
            let visibleCount = min(
                totalCount, Self.cleanupMaxVisibleMembersPerGroup, remaining)
            files = Array(files.prefix(visibleCount))
            groups.append(DuplicateGroup(
                id: gid, files: files, isSimilar: true,
                totalFileCount: totalCount, totalBytes: totalBytes))
            remaining -= files.count
        }
        // Largest clusters first, mirroring the exact view's ORDER BY n DESC.
        groups.sort { $0.files.count > $1.files.count }
        return Array(groups.prefix(Self.cleanupMaxGroups))
    }

    public struct SimilarClusterPage: Sendable, Equatable {
        public var clusters: [[Int64]]
        public var total: Int
        /// The engine's true size of each cluster; it sends at most 2,000 ids of a larger one.
        public var sizes: [Int]
        public init(clusters: [[Int64]], total: Int, sizes: [Int] = []) { self.clusters = clusters; self.total = total; self.sizes = sizes }
    }

    /// One request carries every group the page can show, so the engine groups the library once per load.
    static let similarPageSize = cleanupMaxGroups

    /// Collects the engine's look-alike clusters, largest first, up to the number the Cleanup page can show. Nil when the engine cannot answer, so the caller falls back to grouping locally.
    static func collectSimilarClusters(page: @Sendable (_ offset: Int, _ limit: Int) async -> SimilarClusterPage?) async -> SimilarClusterPage? {
        var collected = SimilarClusterPage(clusters: [], total: 0)
        while collected.clusters.count < cleanupMaxGroups {
            guard let next = await page(collected.clusters.count, similarPageSize) else { return collected.clusters.isEmpty ? nil : collected }
            collected.clusters += next.clusters
            collected.sizes += next.sizes.count == next.clusters.count ? next.sizes : next.clusters.map(\.count)
            collected.total = next.total
            if next.clusters.isEmpty || collected.clusters.count >= next.total { break }
        }
        return collected
    }

    /// Off-main twin of `similarImageGroups`; only the assignment lands on main. With `page` the engine finds the clusters, so the app never loads every photo's hash; without it, or when the engine cannot answer, they are found here.
    public func similarImageGroupsAsync(
        maxHamming: Int = ReadStore.defaultNearDupHamming,
        page: (@Sendable (_ offset: Int, _ limit: Int) async -> SimilarClusterPage?)? = nil
    ) async -> [DuplicateGroup] {
        if let page, let collected = await Self.collectSimilarClusters(page: page) {
            return await Task.detached(priority: .userInitiated) { [self] in
                buildSimilarGroups(clusters: collected.clusters, trueSizes: collected.sizes, contentHashByID: [:])
            }.value
        }
        return await Task.detached(priority: .userInitiated) { [self] in
            similarImageGroups(maxHamming: maxHamming)
        }.value
    }

    // MARK: - Scan sessions

    public struct ScanSessionRow: Sendable, Identifiable {
        public let id: String; public let rootPath: String
        public let startedAt: Date; public let completedAt: Date?
        public let lastFileIndex: Int?; public let totalFiles: Int?
        public let status: String
        /// What the walk left out, as the engine's JSON object of reason to count; nil when it left nothing out or the catalog predates the record.
        public var skippedJSON: String? = nil
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
                    status: r["status"],
                    skippedJSON: r["skipped"]
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
        public let representativeCacheRevision: String?
        public let fileCount: Int
        public let faceCount: Int

        /// The name a file is tagged with: structured parts, else the legacy
        /// `name`; empty when the person has no name.
        public var tagName: String {
            ReadStore.personTagName(title: title, first: firstName, middle: middleName,
                                    last: lastName, suffix: suffix, legacy: name)
        }

        public var displayName: String {
            if isUnknown { return "Unknown" }
            let tag = tagName
            return tag.isEmpty ? "Person \(id)" : tag
        }

        /// True when any name component is set or the person is marked Unknown.
        public var hasAnyName: Bool { isUnknown || !tagName.isEmpty }
    }

    public func persons(includeUnknown: Bool = false) -> [PersonRow] {
        guard let q = queue else { return [] }
        let visible = "EXISTS(SELECT 1 FROM face_prints visible_face WHERE visible_face.person_id=p.id) OR COALESCE(p.is_unknown,0)=1 OR \(Self.personHasNameSQL("p."))"
        let where_ = "WHERE (\(visible))" + (includeUnknown ? "" : " AND IFNULL(p.is_unknown,0)=0")
        do {
            return try q.read { db in
                let rows = try Row.fetchAll(db, sql: """
                    SELECT
                      p.id, p.name, p.title, p.first_name, p.middle_name,
                      p.last_name, p.suffix, p.is_unknown,
                      p.representative_face_id,
                      f.bbox AS rep_bbox, f.file_id AS rep_file_id,
                      files.path_text AS rep_path,
                      COALESCE(f.embedding_model,'') || '|' || COALESCE(f.processing_version,'') || '|' ||
                      COALESCE(f.source_revision,'') || '|' || COALESCE(CAST(files.size_bytes AS TEXT),'') || ':' ||
                      COALESCE(CAST(files.modified_at AS TEXT),'') AS rep_cache_revision,
                      COUNT(fp.id) AS face_count,
                      COUNT(DISTINCT fp.file_id) AS live_file_count
                    FROM persons p
                    LEFT JOIN face_prints f ON f.id = p.representative_face_id
                    LEFT JOIN files ON files.id = f.file_id
                    LEFT JOIN face_prints fp ON fp.person_id = p.id AND COALESCE(fp.excluded, 0) = 0
                    \(where_)
                    GROUP BY p.id
                    ORDER BY p.is_unknown ASC, live_file_count DESC, p.id ASC
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
                        representativeCacheRevision: r["rep_cache_revision"],
                        fileCount: r["live_file_count"] ?? 0,
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
                WHERE IFNULL(is_unknown, 0) = 0 AND \(Self.personHasNameSQL())
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

    /// Files supported by the engine's Deep Analyze runner.
    /// Used by the Restructure tab's hint banner to decide whether to
    /// nudge the user toward running Deep Analyze for sharper proposals.
    public func totalAnalyzableFiles() -> Int {
        guard let q = queue else { return 0 }
        return (try? q.read { db in
            try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM files
                WHERE failed = 0
                  AND kind IN ('image', 'pdf', 'video', 'doc', 'audio', 'model')
            """) ?? 0
        }) ?? 0
    }

    /// `totalAnalyzableFiles` + captioned count in one table pass —
    /// Restructure's regenerate() needs both, and two separate
    /// full-table COUNTs doubled the scan on large libraries. Same
    /// predicates as the individual functions; one shared snapshot.
    public func filesAnalysisStats() -> (analyzable: Int, captioned: Int) {
        guard let q = queue else { return (0, 0) }
        return (try? q.read { db in
            let row = try Row.fetchOne(db, sql: """
                SELECT
                  SUM(CASE WHEN kind IN ('image', 'pdf', 'video', 'doc', 'audio', 'model')
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

    public func personFileCount(id: Int64) -> Int? {
        guard let q = queue else { return nil }
        return try? q.read { db in
            try Int.fetchOne(db, sql: "SELECT file_count FROM persons WHERE id = ?", arguments: [id])
        }
    }

    /// The faces of `personID` on `fileIDs`, each with its photo: what "Move to another person"
    /// hands the engine, one face at a time. Every face of that person on a
    /// chosen photo moves, so a photo never stays half in each group.
    public func faceFiles(personID: Int64, fileIDs: [Int64]) -> [(face: Int64, file: Int64)] {
        guard let q = queue, !fileIDs.isEmpty else { return [] }
        return (try? q.read { db -> [(face: Int64, file: Int64)] in
            var ids: [(face: Int64, file: Int64)] = []
            for start in stride(from: 0, to: fileIDs.count, by: 500) {
                let chunk = fileIDs[start..<min(start + 500, fileIDs.count)]
                let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                var args: [DatabaseValueConvertible] = [personID]
                args.append(contentsOf: chunk.map { Int($0) })
                ids += try Row.fetchAll(db, sql: """
                    SELECT id, file_id FROM face_prints WHERE person_id = ? AND file_id IN (\(placeholders)) ORDER BY id
                    """, arguments: StatementArguments(args)).map { (face: $0["id"], file: $0["file_id"]) }
            }
            return ids
        }) ?? []
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

    public struct FaceCrop: Sendable, Equatable {
        public let path: String
        public let bbox: String
    }

    /// Photo and box of each face, for review previews.
    public func faceCrops(_ ids: [Int64]) -> [Int64: FaceCrop] {
        guard let q = queue, !ids.isEmpty else { return [:] }
        return (try? q.read { db -> [Int64: FaceCrop] in
            let placeholders = ids.prefix(500).map { _ in "?" }.joined(separator: ",")
            var out: [Int64: FaceCrop] = [:]
            for row in try Row.fetchAll(db, sql: """
                SELECT fp.id, f.path_text, fp.bbox FROM face_prints fp JOIN files f ON f.id = fp.file_id WHERE fp.id IN (\(placeholders))
                """, arguments: StatementArguments(ids.prefix(500).map { Int($0) })) {
                out[row["id"]] = FaceCrop(path: row["path_text"], bbox: row["bbox"])
            }
            return out
        }) ?? [:]
    }

    public func totalFacePrints() -> Int {
        guard let q = queue else { return 0 }
        return (try? q.read { db in
            try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM face_prints") ?? 0
        }) ?? 0
    }

    /// Resolves suggested-merge pairs into one survivor per group via
    /// union-find, so A→B and B→C both land on one card. Returns each survivor
    /// with the people it absorbs, for the engine to merge one by one.
    public func mergeBatchPlan(_ pairs: [(target: Int64, source: Int64)]) -> [(target: Int64, sources: [Int64])] {
        guard !pairs.isEmpty else { return [] }

        // name-state + size for every touched cluster, so the union
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
                        isNamed[id] = !Self.personTagName(row: r).isEmpty
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

        var parent: [Int64: Int64] = [:]
        func find(_ x: Int64) -> Int64 {
            var r = x
            while let p = parent[r], p != r { r = p }
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
            // Keep the higher-priority root so a typed name is never deleted by a chain of borderline pairs (see the note above).
            let keep = preferredRoot(rt, rs)
            parent[keep == rt ? rs : rt] = keep
        }
        for (t, s) in pairs where t != s {
            if parent[t] == nil { parent[t] = t }
            if parent[s] == nil { parent[s] = s }
            union(target: t, source: s)
        }

        var byTarget: [Int64: [Int64]] = [:]
        for id in parent.keys {
            let root = find(id)
            if id != root {
                byTarget[root, default: []].append(id)
            }
        }
        return byTarget.map { (target: $0.key, sources: $0.value.sorted()) }.sorted { $0.target < $1.target }
    }

    // MARK: - Helpers

    private static func toFileRow(_ r: Row) -> FileRow {
        let tagsString: String? = r["auto_tags"]
        let rawTags = tagsString?.split(separator: "|").map(String.init)
        let keptTags = rawTags?.filter { !Self.suppressedDisplayTags.contains($0.lowercased()) }
        let finalTags = (keptTags?.isEmpty == false) ? keptTags : nil

        return FileRow(
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
            vlmFullModel: r["vlm_full_model"],
            vlmAnalyzedAt: (r["vlm_analyzed_at"] as Double?).map { Date(timeIntervalSince1970: $0) },
            tags: finalTags,
            vlmInheritedFrom: r["vlm_inherited_from"]
        )
    }

    // MARK: - Deep Analyze queries

    /// `pending`: files the chosen model has not described (a run with "only files not described yet" does these). `undescribed`: files no model has described.
    public func deepAnalyzePending(modelKey: String) -> (total: Int, pending: Int, undescribed: Int) {
        guard let q = queue else { return (0, 0, 0) }
        return (try? q.read { db in
            let describable = "kind IN ('image', 'pdf', 'video', 'doc', 'audio', 'model') AND failed = 0"
            let total = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE \(describable)") ?? 0
            let pending = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE \(describable) AND (vlm_full_model IS NULL OR vlm_full_model != ?)", arguments: [modelKey]) ?? 0
            let undescribed = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE \(describable) AND vlm_full_model IS NULL") ?? 0
            return (total, pending, undescribed)
        }) ?? (0, 0, 0)
    }

    /// (url, tags) for every file carrying at least one keyword tag, so "Apply tags" can write
    /// them as Finder tags. Aggregated per file, one write each.
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

    /// The non-empty [title, first, middle, last, suffix] joined by single
    /// spaces, else the legacy `name`. Byte-faithful with the Windows
    /// `ReadStore.FormatPersonTagName` so a person is tagged identically on both.

    static func personTagName(title: String?, first: String?, middle: String?,
                              last: String?, suffix: String?, legacy: String?) -> String {
        let parts = [title, first, middle, last, suffix]
            .compactMap { $0?.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        if !parts.isEmpty { return parts.joined(separator: " ") }
        return (legacy ?? "").trimmingCharacters(in: .whitespaces)
    }

    /// SQL twin of a non-empty `personTagName`.
    static func personHasNameSQL(_ prefix: String = "") -> String {
        let columns = ["name", "title", "first_name", "middle_name", "last_name", "suffix"]
        return "TRIM(" + columns.map { "COALESCE(\(prefix)\($0),'')" }.joined(separator: " || ") + ") <> ''"
    }

    static func personTagName(row: Row) -> String {
        personTagName(title: row["title"], first: row["first_name"], middle: row["middle_name"],
                      last: row["last_name"], suffix: row["suffix"], legacy: row["name"])
    }

    /// (url, names) for every file containing at least one named person, so "Apply people as tags"
    /// can write the names onto the files. Skips Unknown and unnamed clusters.
    public func filesWithPersonTags() -> [(url: URL, names: [String])] {
        guard let q = queue else { return [] }
        do {
            return try q.read { db in
                let rows = try Row.fetchAll(db, sql: """
                    SELECT files.path_text AS path,
                           p.title, p.first_name, p.middle_name,
                           p.last_name, p.suffix, p.name
                    FROM persons p
                    INNER JOIN face_prints ON face_prints.person_id = p.id
                    INNER JOIN files ON files.id = face_prints.file_id
                    WHERE IFNULL(p.is_unknown, 0) = 0 AND files.failed = 0
                    ORDER BY p.file_count DESC, p.id ASC, files.scanned_at DESC
                    """)
                var byPath: [String: [String]] = [:]
                var order: [String] = []
                for r in rows {
                    guard let path: String = r["path"] else { continue }
                    let name = Self.personTagName(row: r)
                    guard !name.isEmpty else { continue }
                    if byPath[path] == nil { order.append(path) }
                    if !(byPath[path]?.contains(name) ?? false) {
                        byPath[path, default: []].append(name)
                    }
                }
                return order.map { (url: URL(fileURLWithPath: $0), names: byPath[$0] ?? []) }
            }
        } catch {
            reportError("filesWithPersonTags failed: \(error)")
            return []
        }
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

    /// Files in the library under `root` and every folder below it, the set an Organize Folders plan
    /// for that root covers. The range `root/` … `root0` holds exactly the paths below `root`
    /// ('0' sorts right after '/').
    public func countFiles(under root: String) -> Int {
        guard let q = queue else { return 0 }
        var base = root
        while base.count > 1, base.hasSuffix("/") { base.removeLast() }
        let prefix = base == "/" ? "/" : base + "/"
        let upper = String(prefix.dropLast()) + "0"
        return (try? q.read { db in
            try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files WHERE \(Self.notEngineHiddenSQL) AND path_text >= ? AND path_text < ?",
                             arguments: [prefix, upper]) ?? 0
        }) ?? 0
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

    /// One landed rename, persisted so the last batch can be undone.
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

    /// Concurrent read-only connections over the engine's WAL database: the
    /// grid, search, tags, people and counters do not queue behind one
    /// another. Each reader gets a 32 MB page cache and memory-mapped I/O.
    /// A pool of concurrent readers needs WAL (the engine's database always
    /// is). Other journal modes get one connection, since there a reader's
    /// shared lock would block a writer's commit.
    nonisolated static func openReader(at url: URL) throws -> any DatabaseReader {
        let probe = try DatabaseQueue(path: url.path, configuration: readConfiguration())
        let mode = try probe.read { try String.fetchOne($0, sql: "PRAGMA journal_mode") } ?? ""
        guard mode.lowercased() == "wal" else { return probe }
        try probe.close()
        return try DatabasePool(path: url.path, configuration: readConfiguration())
    }

    nonisolated static func readConfiguration() -> Configuration {
        var config = Configuration()
        config.readonly = true
        config.maximumReaderCount = 6
        config.busyMode = .timeout(2)
        config.prepareDatabase { db in
            db.add(collation: DatabaseCollation(LibrarySort.nameCollation) { $0.localizedStandardCompare($1) })
            try db.execute(sql: "PRAGMA cache_size = -32768")
            try db.execute(sql: "PRAGMA mmap_size = 268435456")
            try db.execute(sql: "PRAGMA temp_store = MEMORY")
        }
        return config
    }
}
