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
//     (pipeline/discovery.rs). (F-C6-005)
//
// On a non-forced (incremental) rescan, both paths consult a read-only skip
// set built once from the DB so a file the DB already holds UNCHANGED never
// reaches the expensive ANE/Vision/CLIP/OCR pass + NAS content read. Mirrors
// the Windows discovery skip set (scan_session.rs / discovery.rs). (F-C6-001)
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
    public let kind: Kind
    /// Volume-local file identity = APFS/HFS inode (st_ino), the macOS analog of
    /// the Windows NTFS MFT file_ref. Propagated to TaggedFile so DBWriter's
    /// rename/move heal can re-bind a moved file's row instead of orphaning it.
    public let fileRef: UInt64?

    public enum Kind: String, Sendable {
        // `model` = 3D models (scanned, not dropped like `other`) so Deep Analyze can
        // name them from their embedded object/material labels. Lockstep with the Rust
        // engine's FileKind::Model ("model"). Wavefront `.obj` only for now.
        case image, video, pdf, doc, audio, model, other
    }
}

public enum FileTypes {
    // Conservative starting set; expand in M3 as we test more formats.
    public static let images: Set<String> = [
        "jpg", "jpeg", "png", "heic", "heif", "tif", "tiff", "webp", "gif", "bmp",
        "raw", "cr2", "nef", "arw", "dng", "orf", "rw2", "raf"
    ]
    public static let videos: Set<String> = [
        "mp4", "mov", "m4v", "avi", "mkv", "webm", "wmv", "flv", "mpg", "mpeg", "mts", "m2ts"
    ]
    public static let pdfs: Set<String> = ["pdf"]
    public static let documents: Set<String> = [
        "pdf", "doc", "docx", "odt", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "md", "pages", "numbers", "key"
    ]
    public static let audio: Set<String> = [
        "mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "aiff"
    ]
    /// Source code + prose markup — read as UTF-8 text and clustered by content (BGE),
    /// classified as `.doc`. Lockstep with the Rust engine's FileKind::from_extension.
    public static let code: Set<String> = [
        "swift", "py", "rb", "js", "jsx", "ts", "tsx", "java", "kt", "c", "h", "cpp",
        "cc", "cxx", "hpp", "hh", "cs", "go", "rs", "php", "sh", "bash", "zsh", "sql",
        "scala", "m", "mm", "r", "jl", "lua", "dart", "vue", "pl", "pm", "ps1",
        "tex", "bib", "rst", "org", "adoc"
    ]
    /// E-books — extracted to text and clustered as `.doc`. EPUB only (zip of XHTML);
    /// MOBI is proprietary (low ROI). Lockstep with the Rust engine.
    public static let ebooks: Set<String> = ["epub"]
    /// 3D models — rendered to a thumbnail and (for `.obj`) clustered by CLIP like images;
    /// every recognized format is grouped under `3D Models/` + named by Deep Analyze.
    /// Lockstep with the Rust engine's FileKind::from_extension.
    public static let models: Set<String> = [
        "obj", "stl", "ply", "glb", "gltf", "fbx", "usdz", "usd", "usda", "usdc",
        "dae", "3mf", "3ds", "off"
    ]

    public static func kind(forExtension ext: String) -> DiscoveredFile.Kind {
        let e = ext.lowercased()
        if images.contains(e)    { return .image }
        if videos.contains(e)    { return .video }
        if pdfs.contains(e)      { return .pdf }
        if documents.contains(e) || code.contains(e) || ebooks.contains(e) { return .doc }
        if audio.contains(e)     { return .audio }
        if models.contains(e)    { return .model }
        return .other
    }

    public static func isTaggable(_ ext: String) -> Bool {
        let e = ext.lowercased()
        return images.contains(e) || videos.contains(e) || documents.contains(e)
            || code.contains(e) || ebooks.contains(e)
            || audio.contains(e) || models.contains(e)
    }
}

public actor Discovery {

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
    /// Honors the same incremental skip set as `walk`. (F-C6-005)
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

    // MARK: - Enumeration core

    private struct Exclusion: Sendable {
        let path: String
        let prefix: String
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
    private struct PathFingerprint: Hashable, Sendable {
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
        stripTrailingSlashes(
            url.resolvingSymlinksInPath()
                .standardizedFileURL
                .path
                .precomposedStringWithCanonicalMapping
                .lowercased()
        )
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
        let normalized = normalizedExclusionPath(URL(fileURLWithPath: path))
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
        let resourceKeys: [URLResourceKey] = [
            .isDirectoryKey, .isRegularFileKey, .isHiddenKey,
            .fileSizeKey, .creationDateKey, .contentModificationDateKey
        ]
        // Without an errorHandler the enumerator SILENTLY drops any entry it
        // can't read (permission denied, a NAS share that drops mid-scan, a file
        // removed underfoot) — the user gets an incomplete library with no warning.
        // Count the failures and return true to keep walking, so one unreadable
        // subtree can't truncate the scan; the running total is surfaced as a
        // non-fatal `discovery_partial` summary below, mirroring the Windows engine
        // (scan_session.rs error_count → "discovery_partial").
        var discoveryErrorCount = 0
        guard let enumerator = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: resourceKeys,
            options: skipHidden ? [.skipsHiddenFiles] : [],
            errorHandler: { url, error in
                discoveryErrorCount += 1
                JSONLog.shared.warn(ev: "discovery_dir_access_failed",
                                    path: redactPathForLog(url.path), error: "\(error)")
                return true
            }
        ) else {
            JSONLog.shared.error(ev: "discovery_enumerator_nil", path: redactPathForLog(root.path))
            return 0
        }

        let maxBytes = Int64(maxSizeMB) * 1024 * 1024
        var kept = 0
        var sinceLastProgress = 0
        // re-audit R-08: paths skipped this scan whose `scanned_at` must still be
        // bumped to "now" so the post-scan orphan sweep doesn't mistake a present-
        // but-skipped file for a deletion. Captured once at the walk's start
        // (always > the upstream scanStart), flushed in bounded batches.
        var skippedTouch: [String] = []
        let touchTime = Date().timeIntervalSince1970

        // FileManager.DirectoryEnumerator's `for ... in` is unavailable in
        // async contexts (Sendable issues). nextObject() is the sync escape.
        while let next = enumerator.nextObject() {
            guard let url = next as? URL else { continue }
            if cancelCheck() { break }
            let values = try? url.resourceValues(forKeys: Set(resourceKeys))
            // Skip directories (enumerator yields both; we want files).
            if values?.isDirectory == true {
                if Self.isExcluded(url.path, by: exclusions) {
                    enumerator.skipDescendants()
                }
                continue
            }
            if Self.isExcluded(url.path, by: exclusions) { continue }
            if values?.isRegularFile != true { continue }
            let ext = url.pathExtension
            guard FileTypes.isTaggable(ext) else { continue }
            let size = Int64(values?.fileSize ?? 0)
            if size > maxBytes {
                JSONLog.shared.info(ev: "skip_large_file", path: redactPathForLog(url.path),
                                    extra: ["sizeMB": AnyCodable(size / 1_048_576)])
                continue
            }
            // F-C6-001 incremental skip: a DB row that succeeded before and still
            // has the same size AND the same mtime as on disk (the DBWriter
            // "unchanged" contract, R-09) means we already captured this content —
            // skip the whole ANE/Vision/CLIP/OCR + NAS-read pass. The set holds
            // only `failed = 0` rows (prior failures always reprocess) and excludes
            // embeddable images still lacking a CLIP row (R-14); a lookup miss
            // fails safe (the file is processed).
            if let skip, let entry = skip[PathFingerprint(url.path)],
               Self.isAlreadyCurrent(
                   dbModifiedAt: entry.modifiedAt, dbSize: entry.size,
                   currentModified: values?.contentModificationDate?.timeIntervalSince1970,
                   currentSize: size) {
                // re-audit R-08: the file is PRESENT (just unchanged) — record it
                // so its row's `scanned_at` is refreshed; otherwise the orphan
                // sweep (scanned_at < scanStart) would treat it as deleted.
                skippedTouch.append(url.path)
                if skippedTouch.count >= 2_000 {
                    await Self.touchScannedAt(skippedTouch, to: touchTime, database: database)
                    skippedTouch.removeAll(keepingCapacity: true)
                }
                continue
            }
            await emit(DiscoveredFile(
                url: url,
                sizeBytes: size,
                creationDate: values?.creationDate,
                modificationDate: values?.contentModificationDate,
                kind: FileTypes.kind(forExtension: ext),
                fileRef: Self.inode(of: url)
            ))
            kept += 1
            sinceLastProgress += 1
            if kept == 1 || sinceLastProgress >= 16 {
                progress(kept)
                sinceLastProgress = 0
            }
        }
        if !skippedTouch.isEmpty {
            await Self.touchScannedAt(skippedTouch, to: touchTime, database: database)
        }
        // Non-fatal partial-discovery summary: some entries under `root` couldn't be
        // read this walk (counted by the enumerator's errorHandler above). Logged so
        // the app (Settings → Logs) can tell the user the library may be incomplete
        // instead of silently dropping them. Mirrors the Windows `discovery_partial`
        // event (scan_session.rs); the scan still completes.
        if discoveryErrorCount > 0 {
            JSONLog.shared.info(ev: "discovery_partial",
                                path: redactPathForLog(root.path),
                                extra: ["skipped": AnyCodable(discoveryErrorCount),
                                        "kept": AnyCodable(kept)])
        }
        return kept
    }

    /// re-audit R-08: bump `scanned_at` for files SKIPPED this scan. A skip never
    /// reaches the DBWriter UPSERT that normally refreshes `scanned_at`, so without
    /// this the post-scan orphan sweep — which prunes rows whose `scanned_at`
    /// predates the scan — would saturate its candidate cap with present-but-skipped
    /// files and fail to delete genuinely-gone ones. A cheap UPDATE-only touch keeps
    /// the skip set and the orphan sweep agreeing on "seen this scan". Bound the
    /// IN-list to stay under SQLite's bound-variable limit. Fail-safe: a touch error
    /// only risks an over-eager sweep candidate next run, never data loss, so it is
    /// logged and swallowed.
    private static func touchScannedAt(
        _ paths: [String], to scannedAt: Double, database: Database?
    ) async {
        guard let database, !paths.isEmpty else { return }
        do {
            try await database.pool.write { db in
                for chunk in stride(from: 0, to: paths.count, by: 500).map({
                    Array(paths[$0..<min($0 + 500, paths.count)])
                }) {
                    let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                    var args: [DatabaseValueConvertible] = [scannedAt]
                    args.append(contentsOf: chunk)
                    try db.execute(
                        sql: "UPDATE files SET scanned_at = ? WHERE path_text IN (\(placeholders))",
                        arguments: StatementArguments(args))
                }
            }
        } catch {
            JSONLog.shared.warn(ev: "discovery_skip_touch_failed",
                                path: redactPathForLog(paths.first ?? ""), error: "\(error)")
        }
    }

    /// Volume-local file identity = APFS/HFS inode (st_ino), the macOS analog of
    /// the Windows NTFS MFT file_ref (platform.rs `file_ref`). Stored as INTEGER
    /// for cross-platform DB byte-parity. `stat` (follows symlinks) so a
    /// symlinked regular file resolves to its target's identity, matching the
    /// Windows CreateFileW path; nil on any error (heal simply won't fire).
    /// Computed only for KEPT files (post incremental-skip), so the extra syscall
    /// is paid once per file already bound for the ANE/Vision/CLIP/OCR pass.
    ///
    /// CAUTION (re-audit R-10): unlike the Windows NTFS file_ref, st_ino carries NO
    /// sequence/generation number — APFS/HFS reuse an inode freely after a file is
    /// deleted. So the DBWriter rename/move heal MUST NOT trust file_ref equality
    /// alone; it must corroborate with a signal that survives a move but differs
    /// across distinct files (size_bytes, and content_hash when available),
    /// otherwise a reused inode can re-bind a deleted file's row onto an unrelated
    /// new file. (Corroboration lives in DBWriter.healMovedRow.)
    static func inode(of url: URL) -> UInt64? {
        var st = stat()
        let ok = url.withUnsafeFileSystemRepresentation { rep -> Bool in
            guard let rep else { return false }
            return stat(rep, &st) == 0
        }
        return ok ? UInt64(st.st_ino) : nil
    }

    /// Pure incremental-skip predicate (testable in isolation). Mirrors DBWriter's
    /// `unchanged` contract (DBWriter.insertOne) EXACTLY: a file is "already
    /// current" only when its size is unchanged AND its current on-disk mtime
    /// EQUALS the stored `modified_at` (within float tolerance). The prior form
    /// (`scanned_at >= mtime`) was LOOSER than DBWriter — it skipped same-size
    /// edits whose mtime moved to a value still <= the last scan time (archive
    /// extract / `rsync -a` / git checkout / Time Machine restore), so the new
    /// content was never re-tagged (re-audit R-09). A nil-vs-present mtime is a
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
    /// `text_embeddings` row) is excluded (R-14) via the shared
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
        // R-14: the CLIP-backfill exclusion keeps embeddingless images IN the
        // pipeline so a post-install rescan backfills them — but only matters
        // when CLIP is actually installed. With no CLIP model on disk, no image
        // can ever get an embedding, so the exclusion would keep EVERY image out
        // of the skip set and re-run the full ANE/Vision pass on every scan
        // forever. Gate it on the model file existing. The BGE doc-embedding
        // exclusion has the exact same shape for docs/pdfs (gated on BGE on disk):
        // BGE is opt-in, so the first scan usually predates it and the install-then-
        // rescan path must keep embeddingless docs in the pipeline to backfill them.
        let clipInstalled = FileManager.default.fileExists(
            atPath: MobileCLIPService.defaultImageModelURL.path)
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
