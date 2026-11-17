// Stage A: file discovery.
//
// Walks the directory tree under a given root. Two entry points share one
// enumeration core:
//   - `walk` materializes the full list and sorts it by path. The sort gives
//     I/O locality on NAS volumes — consecutive files in the same folder hit
//     the SMB/NFS prefetch window together. O(N) memory for the scan.
//   - `walkStreaming` yields each file to a callback AS FOUND, so tagging can
//     start before the whole tree has been walked and no O(N) list is held.
//     It preserves the enumerator's depth-first, directory-by-directory
//     traversal (the dominant prefetch win) but drops the cross-directory
//     global sort `walk` adds — matching the Windows jwalk streaming path
//     (pipeline/discovery.rs).
//
// On a non-forced (incremental) rescan, both paths consult a read-only skip
// set built once from the DB so a file the DB already holds UNCHANGED never
// reaches the expensive ANE/Vision/CLIP/OCR pass + NAS content read. Mirrors
// the Windows discovery skip set (scan_session.rs / discovery.rs).
//
// Filters: hidden files, files >500 MB (Vision adds little for
// huge videos / archives and decode can OOM on 16 GB), and
// non-regular files.
import Foundation
import CryptoKit
import GRDB
import FileIDShared

public struct DiscoveredFile: Sendable {
    public let url: URL
    public let sizeBytes: Int64
    public let creationDate: Date?
    public let modificationDate: Date?
    /// The modification time as the catalog stores it (`FileModificationTime`). `modificationDate`
    /// rounds the last bit of times before 2001, so the stored number is never read back from it.
    public var modifiedUnix: Double? = nil
    public let kind: Kind
    /// Volume-local file identity = APFS/HFS inode (st_ino), the macOS analog of
    /// the Windows NTFS MFT file_ref. Propagated to TaggedFile so DBWriter's
    /// rename/move heal can re-bind a moved file's row instead of orphaning it.
    public let fileRef: UInt64?
    /// The library already holds this exact file (same inode, size and date) at a
    /// path that no longer exists: it was moved or renamed, so the row is re-bound
    /// and nothing is decoded or analysed again.
    public var movedFromKnownRow = false

    public typealias Kind = FileKind
}

public actor Discovery {

    /// Plain-language notice for the app, or nil when the walk was complete.
    /// Kind `discovery_partial` mirrors the shared engine's notice.
    public static func shortfallNotice(_ s: Shortfall, maxSizeMB: Int = 500) -> EngineError? {
        var parts: [String] = []
        if s.unreadablePaths > 0 {
            parts.append("\(s.unreadablePaths) \(s.unreadablePaths == 1 ? "item" : "items") couldn't be opened (no permission, or removed during the scan)")
        }
        if s.cloudOnlyFiles > 0 {
            parts.append("\(s.cloudOnlyFiles) \(s.cloudOnlyFiles == 1 ? "file is" : "files are") stored in iCloud and not downloaded to this Mac, so \(s.cloudOnlyFiles == 1 ? "it was" : "they were") left out")
        }
        if s.oversizedFiles > 0 {
            parts.append("\(s.oversizedFiles) \(s.oversizedFiles == 1 ? "file is" : "files are") over \(maxSizeMB) MB and \(s.oversizedFiles == 1 ? "was" : "were") left out")
        }
        guard !parts.isEmpty else { return nil }
        return EngineError(kind: "discovery_partial", message: parts.joined(separator: "; ") + ". Everything else was scanned.")
    }

    /// What the last walk could not include: unreadable paths and files over the size cap.
    public struct Shortfall: Sendable, Equatable {
        public var unreadablePaths = 0
        public var oversizedFiles = 0
        public var cloudOnlyFiles = 0
    }
    public private(set) var lastShortfall = Shortfall()

    /// Files of the last walk that are on disk and need no work: current in the catalog, or
    /// iCloud-only and left out. Their rows are not rewritten; the orphan sweep reads this set
    /// to tell them from rows of files that are gone.
    private(set) var presentUnchanged: Set<PathFingerprint> = []

    /// Plain-language notice (kind `rescan_no_changes`, as the shared engine's) when a walk found
    /// files the catalog already holds and nothing else to do; nil when anything was kept or the
    /// walk fell short (that notice says more).
    public static func rescanNoChangesNotice(kept: Int, shortfall: Shortfall, unchangedFiles: Int, root: String) -> EngineError? {
        guard kept == 0, unchangedFiles > 0, shortfallNotice(shortfall) == nil else { return nil }
        return EngineError(
            kind: "rescan_no_changes",
            message: "Nothing changed in these files since the last scan. FileID is finishing a quick check of the folder.",
            path: root)
    }

    /// Test seam: directories (by enumerator path) whose enumeration is cut short after their first
    /// entry, as the URL enumerator does on exFAT (or, with `keepYielding`, reported as failed while it
    /// still yields the rest of the directory).
    var simulatedFailures: Set<String> = []
    var simulatedFailuresKeepYielding = false
    func simulateEnumeratorFailures(in directories: Set<String>, keepYielding: Bool = false) {
        simulatedFailures = directories
        simulatedFailuresKeepYielding = keepYielding
    }

    public struct Progress: Sendable {
        public let discovered: Int
        public let isComplete: Bool
    }

    /// Walks `root` and returns the discovered file list (sorted by path).
    /// `progress` is invoked roughly every 256-kept-file batch so the caller can
    /// emit XPC progress events. Caller is expected to have a security-scoped
    /// resource lock open on `root`.
    ///
    /// Pass `database` + `forceReprocess: false` to enable the incremental skip
    /// set: files the DB already holds unchanged are dropped here, upstream of
    /// tagging, so a repeat scan pays near-zero on them. Omitting `database`
    /// (the default) reproduces the original "process everything" behavior.
    public func walk(
        root: URL,
        database: Database? = nil,
        forceReprocess: Bool = false,
        skipHidden: Bool = true,
        maxSizeMB: Int = 500,
        excludedPaths: [String]? = nil,
        cancelCheck: @Sendable () -> Bool = { false },
        progress: @Sendable (Int) -> Void = { _ in }
    ) async -> [DiscoveredFile] {
        let exclusions = Self.resolvedExclusions(root: root, rawPaths: excludedPaths)
        let skip = await Self.buildSkipSet(
            root: root, database: database, forceReprocess: forceReprocess)
        var collected: [DiscoveredFile] = []
        collected.reserveCapacity(8_192)
        _ = await enumerate(
            root: root, skipHidden: skipHidden, maxSizeMB: maxSizeMB, skip: skip,
            database: database,
            exclusions: exclusions, cancelCheck: cancelCheck, progress: progress
        ) { file in
            collected.append(file)
        }
        // Sort by path for I/O locality on network volumes.
        collected.sort { $0.url.path < $1.url.path }
        return collected
    }

    /// Streams discovered files to `onFile` AS THEY ARE FOUND — no O(N) list is
    /// materialized and no global sort/dead-air phase precedes tagging. The
    /// enumerator's depth-first traversal already groups same-directory files
    /// (the dominant NAS-prefetch win); the cross-directory alphabetical sort
    /// `walk` adds is intentionally traded away here for the streaming start.
    /// Honors the same incremental skip set as `walk`.
    @discardableResult
    public func walkStreaming(
        root: URL,
        database: Database? = nil,
        forceReprocess: Bool = false,
        skipHidden: Bool = true,
        maxSizeMB: Int = 500,
        excludedPaths: [String]? = nil,
        cancelCheck: @Sendable () -> Bool = { false },
        progress: @Sendable (Int) -> Void = { _ in },
        onFile: (DiscoveredFile) async -> Void
    ) async -> Int {
        let exclusions = Self.resolvedExclusions(root: root, rawPaths: excludedPaths)
        let skip = await Self.buildSkipSet(
            root: root, database: database, forceReprocess: forceReprocess)
        return await enumerate(
            root: root, skipHidden: skipHidden, maxSizeMB: maxSizeMB, skip: skip,
            database: database,
            exclusions: exclusions,
            cancelCheck: cancelCheck, progress: progress, emit: onFile)
    }

    /// FileID's own catalog, journals and logs never become library files.
    static let engineStatePrefix: String = {
        // realpath: the enumerator reports /private/var and /private/tmp, which
        // resolvingSymlinksInPath would strip.
        var path = EngineStateRoot.directory.path
        if let resolved = realpath(path, nil) { path = String(cString: resolved); free(resolved) }
        return path.hasSuffix("/") ? path : path + "/"
    }()

    // MARK: - Enumeration core

    private struct Exclusion: Sendable {
        let path: String
        let prefix: String
    }

    /// What the URL enumerator has handed out, so a directory it fails on can be listed again
    /// without repeating entries it already returned, whether the enumerator then stops
    /// yielding that directory or carries on with it. Keys are `directoryKey` paths, because the
    /// error handler and the yielded URLs need not spell a symlinked prefix the same way.
    final class EnumerationRecord: @unchecked Sendable {
        private let lock = NSLock()
        private var recent: [String: Set<String>] = [:]
        private var lastParent = ""
        private var failedChildren: [String: Set<String>] = [:]
        private var failedOrder: [(key: String, url: URL)] = []

        static func directoryKey(_ path: String) -> String {
            var key = (path as NSString).standardizingPath
            for prefix in ["/private/var/", "/private/tmp/", "/private/etc/"] where key.hasPrefix(prefix) {
                key.removeFirst("/private".count)
                break
            }
            return key
        }

        func noteYield(of url: URL, parent: String) {
            lock.lock()
            defer { lock.unlock() }
            let key = Self.directoryKey(parent)
            if key != lastParent {
                recent = recent.filter { key == $0.key || key.hasPrefix($0.key + "/") }
                lastParent = key
            }
            let name = url.lastPathComponent
            recent[key, default: []].insert(name)
            if failedChildren[key] != nil { failedChildren[key]?.insert(name) }
        }

        func noteFailure(of directory: URL) {
            lock.lock()
            defer { lock.unlock() }
            let key = Self.directoryKey(directory.path)
            guard failedChildren[key] == nil else { return }
            failedChildren[key] = recent[key] ?? []
            failedOrder.append((key, directory))
        }

        func failures() -> [URL] {
            lock.lock()
            defer { lock.unlock() }
            return failedOrder.map(\.url)
        }

        func covered(_ directory: URL) -> Set<String> {
            lock.lock()
            defer { lock.unlock() }
            return failedChildren[Self.directoryKey(directory.path)] ?? []
        }
    }

    private struct SkipEntry: Sendable {
        let modifiedAt: Double?
        let size: Int64
    }

    /// Compact key for the incremental-rescan cache. Storing every full path
    /// duplicated the path string and its heap allocation for the entire scan;
    /// at a million files that can consume hundreds of MiB before tagging even
    /// begins. SHA-256 truncated to 128 bits makes the retained size independent
    /// of path length while keeping collision risk negligible.
    struct PathFingerprint: Hashable, Sendable {
        let high: UInt64
        let low: UInt64

        init(_ path: String) {
            let digest = SHA256.hash(data: Data(path.utf8))
            var high: UInt64 = 0
            var low: UInt64 = 0
            for (index, byte) in digest.prefix(16).enumerated() {
                if index < 8 {
                    high = (high << 8) | UInt64(byte)
                } else {
                    low = (low << 8) | UInt64(byte)
                }
            }
            self.high = high
            self.low = low
        }
    }

    public static func resolvedExclusionPaths(root: URL, rawPaths: [String]?) -> [String] {
        resolvedExclusions(root: root, rawPaths: rawPaths).map(\.path)
    }

    private static func resolvedExclusions(root: URL, rawPaths: [String]?) -> [Exclusion] {
        guard let rawPaths, !rawPaths.isEmpty else { return [] }
        let rootPath = normalizedExclusionPath(root)
        let rootPrefix = rootPath.hasSuffix("/") ? rootPath : rootPath + "/"
        var seen = Set<String>()
        var result: [Exclusion] = []
        for raw in rawPaths {
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { continue }
            let path = normalizedExclusionPath(URL(fileURLWithPath: trimmed))
            guard path != rootPath, path.hasPrefix(rootPrefix), !seen.contains(path) else {
                continue
            }
            seen.insert(path)
            result.append(Exclusion(path: path, prefix: path + "/"))
        }
        return result
    }

    private static func normalizedExclusionPath(_ url: URL) -> String {
        foldedPath(ScanRootGuard.canonicalPath(url.path))
    }

    private static func foldedPath(_ path: String) -> String {
        stripTrailingSlashes(path.precomposedStringWithCanonicalMapping.lowercased())
    }

    private static func stripTrailingSlashes(_ path: String) -> String {
        guard path.count > 1 else { return path }
        var p = path
        while p.count > 1, p.hasSuffix("/") {
            p.removeLast()
        }
        return p
    }

    private static func isExcluded(_ path: String, by exclusions: [Exclusion]) -> Bool {
        guard !exclusions.isEmpty else { return false }
        // The enumerator already reports children in realpath form, so only case
        // and Unicode folding are needed per file — no per-file symlink resolution.
        let normalized = foldedPath(path)
        return exclusions.contains { exclusion in
            normalized == exclusion.path || normalized.hasPrefix(exclusion.prefix)
        }
    }

    /// Shared tree walk used by both `walk` and `walkStreaming`. `emit` receives
    /// each kept file; `progress` is fed the running KEPT count (skipped files
    /// don't count, matching the discovered-count-is-work-to-do contract).
    private func enumerate(
        root: URL,
        skipHidden: Bool,
        maxSizeMB: Int,
        skip: [PathFingerprint: SkipEntry]?,
        database: Database?,
        exclusions: [Exclusion],
        cancelCheck: @Sendable () -> Bool,
        progress: @Sendable (Int) -> Void,
        emit: (DiscoveredFile) async -> Void
    ) async -> Int {
        // `.fileSizeKey` is deliberately not prefetched: on exFAT volumes the batched
        // fetch fails with EINVAL for some directories and the enumerator then drops
        // the rest of that directory. The size comes from the per-file stat instead.
        let resourceKeys: [URLResourceKey] = [.isDirectoryKey, .isRegularFileKey, .isHiddenKey, .creationDateKey]
        // Without an errorHandler the enumerator SILENTLY drops any entry it
        // can't read (permission denied, a NAS share that drops mid-scan, a file
        // removed underfoot) — the user gets an incomplete library with no warning.
        // A directory the enumerator fails on is listed again with contentsOfDirectory
        // once the walk ends, so a quirk of one volume never drops its subtree. Only
        // what that second listing cannot read is counted, and surfaced as a
        // non-fatal `discovery_partial` summary below, mirroring the Windows engine
        // (scan_session.rs error_count → "discovery_partial").
        var discoveryErrorCount = 0
        var oversizedCount = 0
        let record = EnumerationRecord()
        let simulatedFailures = self.simulatedFailures
        let simulatedFailuresKeepYielding = self.simulatedFailuresKeepYielding
        var failedSimulatedParents = Set<String>()
        guard let enumerator = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: resourceKeys,
            options: skipHidden ? [.skipsHiddenFiles] : [],
            errorHandler: { url, error in
                JSONLog.shared.warn(ev: "discovery_enumerator_fallback",
                                    path: redactPathForLog(url.path), error: "\(error)")
                record.noteFailure(of: url)
                return true
            }
        ) else {
            JSONLog.shared.error(ev: "discovery_enumerator_nil", path: redactPathForLog(root.path))
            return 0
        }

        let maxBytes = Int64(maxSizeMB) * 1024 * 1024
        var kept = 0
        var sinceLastProgress = 0
        // Files skipped or left out this walk are present, so the post-scan orphan sweep
        // must not take their untouched rows for deletions; it reads them from here.
        var present = Set<PathFingerprint>()
        var cloudOnlyCount = 0
        var movedCount = 0
        let libraryHasRows = skip?.isEmpty == false

        enum Step { case file, directory, excludedDirectory, other }

        func handle(_ url: URL, isolation: isolated (any Actor)? = #isolation) async -> Step {
            let values = try? url.resourceValues(forKeys: Set(resourceKeys))
            // Skip directories (enumerator yields both; we want files).
            if values?.isDirectory == true {
                return Self.isExcluded(url.path, by: exclusions) ? .excludedDirectory : .directory
            }
            if Self.isExcluded(url.path, by: exclusions) { return .other }
            if values?.isRegularFile != true { return .other }
            if url.path.hasPrefix(Self.engineStatePrefix) { return .other }
            let ext = url.pathExtension
            guard FileTypes.isCataloged(name: url.lastPathComponent) else { return .other }
            // One stat gives the size, the inode, the cloud flag and the modification time as the
            // catalog stores it (seconds plus nanoseconds, not a Date's rounding).
            let identity = Self.identity(of: url)
            let size = identity.size ?? 0
            let fileKind = FileTypes.kind(forExtension: ext)
            // Video, audio and other files are never decoded whole, so the size cap does not apply.
            let streamed = fileKind == .video || fileKind == .audio || fileKind == .other
            if size > maxBytes, !streamed {
                oversizedCount += 1
                JSONLog.shared.info(ev: "skip_large_file", path: redactPathForLog(url.path),
                                    extra: ["sizeMB": AnyCodable(size / 1_048_576)])
                return .other
            }
            // Incremental skip: a DB row that succeeded before and still
            // has the same size AND the same mtime as on disk (the DBWriter
            // "unchanged" contract) means we already captured this content —
            // skip the whole ANE/Vision/CLIP/OCR + NAS-read pass. The set holds
            // only `failed = 0` rows (prior failures always reprocess) and excludes
            // embeddable images still lacking a CLIP row; a lookup miss
            // fails safe (the file is processed).
            let fingerprint = PathFingerprint(url.path)
            if let skip, let entry = skip[fingerprint],
               Self.isAlreadyCurrent(
                   dbModifiedAt: entry.modifiedAt, dbSize: entry.size,
                   currentModified: identity.modified,
                   currentSize: size) {
                present.insert(fingerprint)
                return .other
            }
            // Reading a cloud-only file makes macOS download it. Leave it out
            // (keeping any row it already has) until it is on this Mac.
            if identity.cloudOnly {
                cloudOnlyCount += 1
                present.insert(fingerprint)
                return .other
            }
            var discovered = DiscoveredFile(
                url: url,
                sizeBytes: size,
                creationDate: values?.creationDate,
                modificationDate: identity.modified.map { Date(timeIntervalSince1970: $0) },
                modifiedUnix: identity.modified,
                kind: fileKind,
                fileRef: identity.inode
            )
            if libraryHasRows, let inode = identity.inode, let database,
               await Self.isMoveOfKnownRow(
                   path: url.path, inode: inode, size: size,
                   modified: identity.modified, database: database) {
                discovered.movedFromKnownRow = true
                movedCount += 1
            }
            await emit(discovered)
            kept += 1
            sinceLastProgress += 1
            if kept == 1 || sinceLastProgress >= 16 {
                progress(kept)
                sinceLastProgress = 0
            }
            return .file
        }

        // FileManager.DirectoryEnumerator's `for ... in` is unavailable in
        // async contexts (Sendable issues). nextObject() is the sync escape.
        while let next = enumerator.nextObject() {
            guard let url = next as? URL else { continue }
            if cancelCheck() { break }
            let parent = url.deletingLastPathComponent().path
            if !simulatedFailuresKeepYielding, failedSimulatedParents.contains(parent) {
                if (try? url.resourceValues(forKeys: [.isDirectoryKey]))?.isDirectory == true {
                    enumerator.skipDescendants()
                }
                continue
            }
            record.noteYield(of: url, parent: parent)
            if await handle(url) == .excludedDirectory { enumerator.skipDescendants() }
            if simulatedFailures.contains(parent), failedSimulatedParents.insert(parent).inserted {
                record.noteFailure(of: url.deletingLastPathComponent())
            }
        }
        var fallbackDirectories = record.failures()
        var listed = Set<String>()
        while !fallbackDirectories.isEmpty, !cancelCheck() {
            let directory = fallbackDirectories.removeLast()
            guard listed.insert(EnumerationRecord.directoryKey(directory.path)).inserted else { continue }
            let covered = record.covered(directory)
            let children: [URL]
            do {
                children = try FileManager.default.contentsOfDirectory(
                    at: directory, includingPropertiesForKeys: resourceKeys,
                    options: skipHidden ? [.skipsHiddenFiles] : [])
            } catch {
                discoveryErrorCount += 1
                JSONLog.shared.warn(ev: "discovery_dir_access_failed",
                                    path: redactPathForLog(directory.path), error: "\(error)")
                continue
            }
            for child in children.sorted(by: { $0.path > $1.path }) where !covered.contains(child.lastPathComponent) {
                if cancelCheck() { break }
                if await handle(child) == .directory { fallbackDirectories.append(child) }
            }
        }
        presentUnchanged = present
        // Non-fatal partial-discovery summary: some entries under `root` couldn't be
        // read this walk (counted by the enumerator's errorHandler above). Logged so
        // the app (Settings → Logs) can tell the user the library may be incomplete
        // instead of silently dropping them. Mirrors the Windows `discovery_partial`
        // event (scan_session.rs); the scan still completes.
        lastShortfall = Shortfall(unreadablePaths: discoveryErrorCount, oversizedFiles: oversizedCount,
                                  cloudOnlyFiles: cloudOnlyCount)
        if cloudOnlyCount > 0 || movedCount > 0 {
            JSONLog.shared.info(ev: "discovery_shortcuts",
                                extra: ["cloudOnly": AnyCodable(cloudOnlyCount), "moved": AnyCodable(movedCount)])
        }
        if discoveryErrorCount > 0 {
            JSONLog.shared.info(ev: "discovery_partial",
                                path: redactPathForLog(root.path),
                                extra: ["skipped": AnyCodable(discoveryErrorCount),
                                        "kept": AnyCodable(kept)])
        }
        return kept
    }

    /// Volume-local file identity = APFS/HFS inode (st_ino), the macOS analog of
    /// the Windows NTFS MFT file_ref (platform.rs `file_ref`). Stored as INTEGER
    /// for cross-platform DB byte-parity. `stat` (follows symlinks) so a
    /// symlinked regular file resolves to its target's identity, matching the
    /// Windows CreateFileW path; nil on any error (heal simply won't fire).
    /// Computed once per cataloged file, before the size cap and the incremental
    /// skip, so the walk pays a single stat per file.
    ///
    /// CAUTION: unlike the Windows NTFS file_ref, st_ino carries NO
    /// sequence/generation number — APFS/HFS reuse an inode freely after a file is
    /// deleted. So the DBWriter rename/move heal MUST NOT trust file_ref equality
    /// alone; it must corroborate with a signal that survives a move but differs
    /// across distinct files (size_bytes, and content_hash when available),
    /// otherwise a reused inode can re-bind a deleted file's row onto an unrelated
    /// new file. (Corroboration lives in DBWriter.healMovedRow.)
    static func inode(of url: URL) -> UInt64? {
        identity(of: url).inode
    }

    /// One `stat` for the inode, the size and SF_DATALESS, the flag macOS sets on a
    /// file whose contents live only in iCloud (or another file provider).
    /// `modified` is the Rust engine's value, including 0 for a time before 1970 (its discovery
    /// stores 0 there); it is nil only when the stat fails.
    static func identity(of url: URL) -> (inode: UInt64?, cloudOnly: Bool, modified: Double?, size: Int64?) {
        var st = stat()
        let ok = url.withUnsafeFileSystemRepresentation { rep -> Bool in
            guard let rep else { return false }
            return stat(rep, &st) == 0
        }
        guard ok else { return (nil, false, nil, nil) }
        return (UInt64(st.st_ino), st.st_flags & UInt32(SF_DATALESS) != 0, FileModificationTime.unixSeconds(st) ?? 0, Int64(st.st_size))
    }

    /// True when no row sits at `path` yet and a successfully scanned row with the
    /// same inode, size and modification date sits at a path that is gone from
    /// disk. The same corroboration as DBWriter.healMovedRow, plus the date, so a
    /// reused inode can't pass. Any error means "not a move" (the file is analysed).
    static func isMoveOfKnownRow(
        path: String, inode: UInt64, size: Int64, modified: Double?, database: Database
    ) async -> Bool {
        let oldPaths: [String] = (try? await database.pool.read { db -> [String] in
            if try Bool.fetchOne(db.cachedStatement(sql: "SELECT EXISTS(SELECT 1 FROM files WHERE path_text = ?)"),
                                 arguments: [path]) == true { return [] }
            let rows = try Row.fetchAll(db.cachedStatement(sql: """
                SELECT path_text, modified_at FROM files
                WHERE file_ref = ? AND size_bytes = ? AND failed = 0 AND path_text != ?
                LIMIT 8
                """), arguments: [Int64(bitPattern: inode), size, path])
            return rows.compactMap { row -> String? in
                let stored: Double? = row["modified_at"]
                guard let stored, let modified, abs(stored - modified) < 0.000_001 else { return nil }
                return row["path_text"]
            }
        }) ?? []
        return oldPaths.contains { old in
            var st = stat()
            return URL(fileURLWithPath: old).withUnsafeFileSystemRepresentation { rep in
                guard let rep else { return false }
                return lstat(rep, &st) != 0 && errno == ENOENT
            }
        }
    }

    /// Pure incremental-skip predicate (testable in isolation). Mirrors DBWriter's
    /// `unchanged` contract (DBWriter.insertOne) EXACTLY: a file is "already
    /// current" only when its size is unchanged AND its current on-disk mtime
    /// EQUALS the stored `modified_at` (within float tolerance). The prior form
    /// (`scanned_at >= mtime`) was LOOSER than DBWriter — it skipped same-size
    /// edits whose mtime moved to a value still <= the last scan time (archive
    /// extract / `rsync -a` / git checkout / Time Machine restore), so the new
    /// content was never re-tagged. A nil-vs-present mtime is a
    /// change; both-nil matches DBWriter's both-nil "unchanged". `forceReprocess`
    /// and the prior-failure exclusion are handled where the skip set is built.
    static func isAlreadyCurrent(
        dbModifiedAt: Double?, dbSize: Int64,
        currentModified: Double?, currentSize: Int64
    ) -> Bool {
        guard dbSize == currentSize else { return false }
        switch (dbModifiedAt, currentModified) {
        case let (a?, b?): return abs(a - b) < 0.000_001
        case (nil, nil):   return true
        default:           return false
        }
    }

    /// Build the read-only incremental skip set for `root`. Returns nil (skip
    /// nothing) on a forced rescan, when no DB is supplied, or on a read error
    /// (fail-safe = reprocess). The range predicate `path_text >= prefix AND
    /// path_text < prefixUpper` is sargable on the UNIQUE index on `path_text`
    /// (a `LIKE prefix||'%'` is not) and scopes the load to THIS root's subtree,
    /// mirroring the Windows skip-set query (scan_session.rs) and the macOS
    /// orphan-sweep range. Only `failed = 0` rows are loaded, and an embeddable
    /// image still lacking a `clip_embeddings` row (or a doc/pdf lacking a
    /// `text_embeddings` row) is excluded via the shared
    /// `DBWriter.skipSetClipBackfillExclusionSQL` / `…TextBackfillExclusionSQL` so the
    /// post-install backfill branches in DBWriter.insertOne stay reachable on an
    /// incremental rescan instead of being filtered out here.
    private static func buildSkipSet(
        root: URL, database: Database?, forceReprocess: Bool
    ) async -> [PathFingerprint: SkipEntry]? {
        guard !forceReprocess, let database else { return nil }
        let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        let prefixUpper: String = {
            var s = prefix
            guard let last = s.popLast(),
                  let next = UnicodeScalar(last.unicodeScalars.first!.value + 1) else {
                return prefix  // unreachable for a non-empty "…/" prefix
            }
            return s + String(next)
        }()
        // The CLIP-backfill exclusion keeps embeddingless images IN the
        // pipeline so a post-install rescan backfills them — but only matters
        // when CLIP is actually installed. With no CLIP model on disk, no image
        // can ever get an embedding, so the exclusion would keep EVERY image out
        // of the skip set and re-run the full ANE/Vision pass on every scan
        // forever. Gate it on the model file existing. The BGE doc-embedding
        // exclusion has the exact same shape for docs/pdfs (gated on BGE on disk):
        // BGE is opt-in, so the first scan usually predates it and the install-then-
        // rescan path must keep embeddingless docs in the pipeline to backfill them.
        // "Installed" means the scan can really make an embedding: a model file
        // that fails to load would otherwise send every image back through the
        // full pass on every scan. The scan loads it before the walk starts.
        let clipInstalled = FileManager.default.fileExists(
            atPath: MobileCLIPService.defaultImageModelURL.path)
            && MobileCLIPService.shared.canEmbed
        let clipExclusion = clipInstalled
            ? "AND \(DBWriter.skipSetClipBackfillExclusionSQL)" : ""
        // Same CLIP gate keeps an embeddingless `.obj` 3D model in the pipeline to backfill.
        let modelExclusion = clipInstalled
            ? "AND \(DBWriter.skipSetModelClipBackfillExclusionSQL)" : ""
        let textExclusion = BGETextService.isInstalledOnDisk
            ? "AND \(DBWriter.skipSetTextBackfillExclusionSQL)" : ""
        do {
            return try await database.pool.read { db -> [PathFingerprint: SkipEntry] in
                var map: [PathFingerprint: SkipEntry] = [:]
                let cursor = try Row.fetchCursor(db, sql: """
                    SELECT path_text, size_bytes, modified_at FROM files
                    WHERE failed = 0 AND path_text >= ? AND path_text < ?
                      \(clipExclusion)
                      \(modelExclusion)
                      \(textExclusion)
                    """, arguments: [prefix, prefixUpper])
                while let row = try cursor.next() {
                    let path: String = row["path_text"]
                    let size: Int64 = row["size_bytes"] ?? -1
                    let modifiedAt: Double? = row["modified_at"]
                    map[PathFingerprint(path)] = SkipEntry(modifiedAt: modifiedAt, size: size)
                }
                return map
            }
        } catch {
            JSONLog.shared.warn(ev: "discovery_skipset_failed",
                                path: redactPathForLog(root.path), error: "\(error)")
            return nil
        }
    }
}
