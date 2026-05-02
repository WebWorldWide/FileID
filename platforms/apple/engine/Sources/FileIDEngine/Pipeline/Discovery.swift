// Stage A: file discovery.
//
// Walks the directory tree under a given root and sorts the result
// by path. The sort is what gives us I/O locality on NAS volumes —
// consecutive files in the same folder hit the SMB/NFS prefetch
// window together. O(N log N) in memory, negligible at scan scale.
//
// Filters: hidden files, files >500 MB (Vision adds little for
// huge videos / archives and decode can OOM on 16 GB), and
// non-regular files.
import Foundation
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

    /// Walks `root` and returns the discovered file list. `progress` is invoked
    /// roughly every 1024-file batch so the caller can emit XPC progress events.
    /// Caller is expected to have a security-scoped resource lock open on `root`.
    public func walk(
        root: URL,
        skipHidden: Bool = true,
        maxSizeMB: Int = 500,
        cancelCheck: @Sendable () -> Bool = { false },
        progress: @Sendable (Int) -> Void = { _ in }
    ) async -> [DiscoveredFile] {
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
            return []
        }

        var collected: [DiscoveredFile] = []
        collected.reserveCapacity(8_192)
        let maxBytes = Int64(maxSizeMB) * 1024 * 1024
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
            collected.append(DiscoveredFile(
                url: url,
                sizeBytes: size,
                creationDate: values?.creationDate,
                modificationDate: values?.contentModificationDate,
                kind: FileTypes.kind(forExtension: ext)
            ))
            sinceLastProgress += 1
            if sinceLastProgress >= 256 {
                progress(collected.count)
                sinceLastProgress = 0
            }
        }
        // Sort by path for I/O locality on network volumes.
        collected.sort { $0.url.path < $1.url.path }
        return collected
    }
}
