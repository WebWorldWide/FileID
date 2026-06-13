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
import GRDB
import FileIDShared

public struct DiscoveredFile: Sendable {
    public let url: URL
    public let sizeBytes: Int64
    public let creationDate: Date?
    public let modificationDate: Date?
    public let kind: Kind

    public enum Kind: String, Sendable {
        case image, video, pdf, doc, audio, other
    }
}

public enum FileTypes {
    // Conservative starting set; expand in M3 as we test more formats.
    public static let images: Set<String> = [
        "jpg", "jpeg", "png", "heic", "heif", "tif", "tiff", "webp", "gif", "bmp",
        "raw", "cr2", "nef", "arw", "dng", "orf", "rw2", "raf"
    ]
    public static let videos: Set<String> = [
        "mp4", "mov", "m4v", "avi", "mkv", "webm", "wmv", "flv", "mpg", "mpeg"
    ]
    public static let pdfs: Set<String> = ["pdf"]
    public static let documents: Set<String> = [
        "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "md", "pages", "numbers", "key"
    ]
    public static let audio: Set<String> = [
        "mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "aiff"
    ]

    public static func kind(forExtension ext: String) -> DiscoveredFile.Kind {
        let e = ext.lowercased()
        if images.contains(e)    { return .image }
        if videos.contains(e)    { return .video }
        if pdfs.contains(e)      { return .pdf }
        if documents.contains(e) { return .doc }
        if audio.contains(e)     { return .audio }
        return .other
    }

    public static func isTaggable(_ ext: String) -> Bool {
        let e = ext.lowercased()
        return images.contains(e) || videos.contains(e) || documents.contains(e) || audio.contains(e)
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
        cancelCheck: @Sendable () -> Bool = { false },
        progress: @Sendable (Int) -> Void = { _ in }
    ) async -> [DiscoveredFile] {
        let skip = await Self.buildSkipSet(
            root: root, database: database, forceReprocess: forceReprocess)
        var collected: [DiscoveredFile] = []
        collected.reserveCapacity(8_192)
        await enumerate(
            root: root, skipHidden: skipHidden, maxSizeMB: maxSizeMB, skip: skip,
            cancelCheck: cancelCheck, progress: progress
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
    public func walkStreaming(
        root: URL,
        database: Database? = nil,
        forceReprocess: Bool = false,
        skipHidden: Bool = true,
        maxSizeMB: Int = 500,
        cancelCheck: @Sendable () -> Bool = { false },
        progress: @Sendable (Int) -> Void = { _ in },
        onFile: (DiscoveredFile) async -> Void
    ) async {
        let skip = await Self.buildSkipSet(
            root: root, database: database, forceReprocess: forceReprocess)
        await enumerate(
            root: root, skipHidden: skipHidden, maxSizeMB: maxSizeMB, skip: skip,
            cancelCheck: cancelCheck, progress: progress, emit: onFile)
    }

    // MARK: - Enumeration core

    private struct SkipEntry: Sendable {
        let scannedAt: Double
        let size: Int64
    }

    /// Shared tree walk used by both `walk` and `walkStreaming`. `emit` receives
    /// each kept file; `progress` is fed the running KEPT count (skipped files
    /// don't count, matching the discovered-count-is-work-to-do contract).
    private func enumerate(
        root: URL,
        skipHidden: Bool,
        maxSizeMB: Int,
        skip: [String: SkipEntry]?,
        cancelCheck: @Sendable () -> Bool,
        progress: @Sendable (Int) -> Void,
        emit: (DiscoveredFile) async -> Void
    ) async {
        let resourceKeys: [URLResourceKey] = [
            .isDirectoryKey, .isRegularFileKey, .isHiddenKey,
            .fileSizeKey, .creationDateKey, .contentModificationDateKey
        ]
        guard let enumerator = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: resourceKeys,
            options: skipHidden ? [.skipsHiddenFiles] : []
        ) else {
            JSONLog.shared.error(ev: "discovery_enumerator_nil", path: redactPathForLog(root.path))
            return
        }

        let maxBytes = Int64(maxSizeMB) * 1024 * 1024
        var kept = 0
        var sinceLastProgress = 0

        // FileManager.DirectoryEnumerator's `for ... in` is unavailable in
        // async contexts (Sendable issues). nextObject() is the sync escape.
        while let next = enumerator.nextObject() {
            guard let url = next as? URL else { continue }
            if cancelCheck() { break }
            let values = try? url.resourceValues(forKeys: Set(resourceKeys))
            // Skip directories (enumerator yields both; we want files).
            if values?.isDirectory == true { continue }
            if values?.isRegularFile != true { continue }
            let ext = url.pathExtension
            guard FileTypes.isTaggable(ext) else { continue }
            let size = Int64(values?.fileSize ?? 0)
            if size > maxBytes {
                JSONLog.shared.info(ev: "skip_large_file", path: redactPathForLog(url.path),
                                    extra: ["sizeMB": AnyCodable(size / 1_048_576)])
                continue
            }
            // F-C6-001 incremental skip: a DB row that succeeded before, still
            // has the same size, and whose `scanned_at` is at/after the file's
            // current on-disk mtime means we already captured this content —
            // skip the whole ANE/Vision/CLIP/OCR + NAS-read pass. The set holds
            // only `failed = 0` rows, so prior failures always reprocess, and a
            // lookup miss fails safe (the file is processed).
            if let skip, let entry = skip[url.path],
               Self.isAlreadyCurrent(
                   dbScannedAt: entry.scannedAt, dbSize: entry.size,
                   currentModified: values?.contentModificationDate?.timeIntervalSince1970,
                   currentSize: size) {
                continue
            }
            await emit(DiscoveredFile(
                url: url,
                sizeBytes: size,
                creationDate: values?.creationDate,
                modificationDate: values?.contentModificationDate,
                kind: FileTypes.kind(forExtension: ext)
            ))
            kept += 1
            sinceLastProgress += 1
            if sinceLastProgress >= 256 {
                progress(kept)
                sinceLastProgress = 0
            }
        }
    }

    /// Pure incremental-skip predicate (testable in isolation). A file is
    /// "already current" when its size is unchanged AND the DB scanned it at or
    /// after the file's current modification time. A `nil` current mtime can't
    /// prove the file is unchanged, so it is never skipped. `forceReprocess` and
    /// the prior-failure exclusion are handled where the skip set is built.
    static func isAlreadyCurrent(
        dbScannedAt: Double, dbSize: Int64,
        currentModified: Double?, currentSize: Int64
    ) -> Bool {
        guard dbSize == currentSize else { return false }
        guard let modified = currentModified else { return false }
        return dbScannedAt >= modified
    }

    /// Build the read-only incremental skip set for `root`. Returns nil (skip
    /// nothing) on a forced rescan, when no DB is supplied, or on a read error
    /// (fail-safe = reprocess). The range predicate `path_text >= prefix AND
    /// path_text < prefixUpper` is sargable on the UNIQUE index on `path_text`
    /// (a `LIKE prefix||'%'` is not) and scopes the load to THIS root's subtree,
    /// mirroring the Windows skip-set query (scan_session.rs) and the macOS
    /// orphan-sweep range. Only `failed = 0` rows are loaded.
    private static func buildSkipSet(
        root: URL, database: Database?, forceReprocess: Bool
    ) async -> [String: SkipEntry]? {
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
        do {
            return try await database.pool.read { db -> [String: SkipEntry] in
                var map: [String: SkipEntry] = [:]
                let rows = try Row.fetchAll(db, sql: """
                    SELECT path_text, size_bytes, scanned_at FROM files
                    WHERE failed = 0 AND path_text >= ? AND path_text < ?
                    """, arguments: [prefix, prefixUpper])
                map.reserveCapacity(rows.count)
                for row in rows {
                    let path: String = row["path_text"]
                    let size: Int64 = row["size_bytes"] ?? -1
                    let scannedAt: Double = row["scanned_at"] ?? 0
                    map[path] = SkipEntry(scannedAt: scannedAt, size: size)
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
