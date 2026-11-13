// CoreSpotlight bridge — registers FileID's enriched metadata
// (smart names, captions, tags) so a ⌘Space query finds the photos
// from anywhere on macOS. Items are scoped to "com.fileid.photos"
// for clean wipe/reindex.
import Foundation
import CoreSpotlight
import CryptoKit
import UniformTypeIdentifiers
import FileIDShared
import GRDB

struct SpotlightRow: Sendable {
    let id: Int64
    let path: String
    let kind: String
    let smartName: String?
    let description: String?
    let tags: [String]
}

/// What Spotlight was last sent for each file, kept beside the library so a
/// pass re-sends only files whose title, description, tags or path changed.
/// A rescan rewrites every row's scan date, so dates can't tell them apart.
struct SpotlightFingerprints: Equatable, Sendable {
    private static let magic = Data("FIDSPOT1".utf8)

    var byID: [Int64: UInt64] = [:]

    static var defaultURL: URL { AppSupportPath.fileID.appendingPathComponent("spotlight-sent.bin") }

    static func fingerprint(_ row: SpotlightRow) -> UInt64 {
        var hasher = SHA256()
        for field in [row.path, row.kind, row.smartName ?? "", row.description ?? ""] + row.tags.sorted() {
            hasher.update(data: Data(field.utf8))
            hasher.update(data: Data([0]))
        }
        return hasher.finalize().prefix(8).reduce(0) { $0 << 8 | UInt64($1) }
    }

    /// Missing or unreadable means nothing is known to be sent.
    static func load(from url: URL) -> SpotlightFingerprints {
        guard let data = FileManager.default.contents(atPath: url.path), data.starts(with: magic),
              (data.count - magic.count) % 16 == 0 else { return SpotlightFingerprints() }
        var byID: [Int64: UInt64] = [:]
        byID.reserveCapacity((data.count - magic.count) / 16)
        data.dropFirst(magic.count).withUnsafeBytes { raw in
            for offset in stride(from: 0, to: raw.count, by: 16) {
                let id = Int64(littleEndian: raw.loadUnaligned(fromByteOffset: offset, as: Int64.self))
                byID[id] = UInt64(littleEndian: raw.loadUnaligned(fromByteOffset: offset + 8, as: UInt64.self))
            }
        }
        return SpotlightFingerprints(byID: byID)
    }

    func save(to url: URL) throws {
        var data = Self.magic
        data.reserveCapacity(data.count + byID.count * 16)
        for (id, fingerprint) in byID {
            withUnsafeBytes(of: id.littleEndian) { data.append(contentsOf: $0) }
            withUnsafeBytes(of: fingerprint.littleEndian) { data.append(contentsOf: $0) }
        }
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: url, options: .atomic)
    }

    func changed(_ rows: [SpotlightRow]) -> [SpotlightRow] {
        rows.filter { byID[$0.id] != Self.fingerprint($0) }
    }

    func vanished(keeping seen: Set<Int64>) -> [Int64] {
        byID.keys.filter { !seen.contains($0) }
    }
}

private actor SpotlightIndexCoordinator {
    static let shared = SpotlightIndexCoordinator()

    private var draining = false
    private var pendingDBPath: String?
    private var pendingDeindex = Set<Int64>()
    private var pendingWipe = false

    func requestIndex(dbPath: String) {
        pendingDBPath = dbPath
        startDrainingIfNeeded()
    }

    func requestDeindex(ids: [Int64]) {
        pendingDeindex.formUnion(ids)
        startDrainingIfNeeded()
    }

    func requestWipe() {
        pendingWipe = true
        pendingDeindex.removeAll(keepingCapacity: true)
        pendingDBPath = nil
        startDrainingIfNeeded()
    }

    private func startDrainingIfNeeded() {
        guard !draining else { return }
        draining = true
        Task { await drain() }
    }

    private func drain() async {
        while true {
            if pendingWipe {
                pendingWipe = false
                let wiped = await SpotlightIndexer.performWipe()
                if !wiped {
                    pendingWipe = true
                    try? await Task.sleep(for: .seconds(5))
                }
                continue
            }
            if !pendingDeindex.isEmpty {
                let ids = Array(pendingDeindex)
                pendingDeindex.removeAll(keepingCapacity: true)
                let deindexed = await SpotlightIndexer.performDeindex(ids: ids)
                if !deindexed {
                    pendingDeindex.formUnion(ids)
                    try? await Task.sleep(for: .seconds(5))
                }
                continue
            }
            if let dbPath = pendingDBPath {
                pendingDBPath = nil
                let indexed = await SpotlightIndexer.indexPass(dbPath: dbPath)
                if !indexed {
                    pendingDBPath = dbPath
                    try? await Task.sleep(for: .seconds(5))
                }
                continue
            }
            draining = false
            return
        }
    }
}

public enum SpotlightIndexer {

    public static let domainIdentifier = "com.fileid.photos"

    static let batchSize = 500

    /// Coalesce overlapping requests and index in stable-ID pages so database rows
    /// and CoreSpotlight objects never scale in memory with the whole library.
    public static func indexAll(dbPath: String) async {
        await SpotlightIndexCoordinator.shared.requestIndex(dbPath: dbPath)
    }

    /// Reads every row, sends the ones whose indexed content differs from what
    /// was last sent, and removes the ones that are gone.
    fileprivate static func indexPass(dbPath: String) async -> Bool {
        let fingerprintURL = SpotlightFingerprints.defaultURL
        let queue: DatabaseQueue
        do {
            var configuration = Configuration()
            configuration.readonly = true
            queue = try DatabaseQueue(path: dbPath, configuration: configuration)
        } catch {
            let ns = error as NSError
            NSLog("FileID Spotlight: open failed — \(ns.domain) \(ns.code)")
            return false
        }
        var sent = await Task.detached(priority: .background) { SpotlightFingerprints.load(from: fingerprintURL) }.value
        var seen = Set<Int64>()
        var dirty = false
        // Keeps what was sent so far when a later step fails, so the retry
        // doesn't send it again.
        func persist() async {
            guard dirty else { return }
            let snapshot = sent
            await Task.detached(priority: .background) { try? snapshot.save(to: fingerprintURL) }.value
        }
        var afterID: Int64 = -1
        while true {
            let pageStart = afterID
            var rows: [SpotlightRow]?
            for attempt in 0..<2 {
                let result = await Task.detached(priority: .background) {
                    Result {
                        try readRows(queue, afterID: pageStart, limit: batchSize)
                    }
                }.value
                switch result {
                case .success(let page):
                    rows = page
                case .failure(let error):
                    if attempt == 0 {
                        try? await Task.sleep(for: .milliseconds(250))
                    } else {
                        let ns = error as NSError
                        NSLog("FileID Spotlight: page read failed — \(ns.domain) \(ns.code)")
                    }
                }
                if rows != nil { break }
            }
            guard let rows else { await persist(); return false }
            guard !rows.isEmpty else { break }
            seen.formUnion(rows.map(\.id))
            let changed = sent.changed(rows)
            if !changed.isEmpty {
                do {
                    try await CSSearchableIndex.default().indexSearchableItems(changed.map(makeItem))
                } catch {
                    let ns = error as NSError
                    NSLog("FileID Spotlight: batch index failed — \(ns.domain) \(ns.code)")
                    await persist()
                    return false
                }
                for row in changed { sent.byID[row.id] = SpotlightFingerprints.fingerprint(row) }
                dirty = true
            }
            afterID = rows[rows.count - 1].id
        }
        let gone = sent.vanished(keeping: seen)
        for start in stride(from: 0, to: gone.count, by: batchSize) {
            let ids = Array(gone[start..<min(start + batchSize, gone.count)])
            guard await deleteItems(ids: ids) else { await persist(); return false }
            for id in ids { sent.byID.removeValue(forKey: id) }
            dirty = true
        }
        await persist()
        return true
    }

    /// Wipe every FileID-owned item from Spotlight. Called by the
    /// wipe-library flow (EngineClient.deleteLibraryFiles) so wiped
    /// files' captions/tags/paths leave ⌘Space with the library.
    public static func wipe() {
        Task { await SpotlightIndexCoordinator.shared.requestWipe() }
    }

    fileprivate static func performWipe() async -> Bool {
        for attempt in 0..<3 {
            let error: Error? = await withCheckedContinuation { continuation in
                CSSearchableIndex.default().deleteSearchableItems(
                    withDomainIdentifiers: [domainIdentifier]
                ) { continuation.resume(returning: $0) }
            }
            guard let error else {
                // Passes and wipes run one at a time, so no pass can write this back.
                try? FileManager.default.removeItem(at: SpotlightFingerprints.defaultURL)
                return true
            }
            if attempt < 2 {
                try? await Task.sleep(for: .milliseconds(250))
            } else {
                let ns = error as NSError
                NSLog("FileID Spotlight: wipe failed — \(ns.domain) \(ns.code)")
            }
        }
        return false
    }

    /// Drop the Spotlight items for deleted rows — `indexAll` only
    /// upserts, so without this a trashed file's caption/path stays
    /// queryable in ⌘Space indefinitely.
    public static func deindex(ids: [Int64]) {
        guard !ids.isEmpty else { return }
        Task { await SpotlightIndexCoordinator.shared.requestDeindex(ids: ids) }
    }

    /// Also forgets what was sent for these files: one put back from Trash
    /// keeps its id and must be sent again.
    fileprivate static func performDeindex(ids: [Int64]) async -> Bool {
        guard await deleteItems(ids: ids) else { return false }
        let url = SpotlightFingerprints.defaultURL
        await Task.detached(priority: .background) {
            var sent = SpotlightFingerprints.load(from: url)
            let before = sent.byID.count
            for id in ids { sent.byID.removeValue(forKey: id) }
            if sent.byID.count != before { try? sent.save(to: url) }
        }.value
        return true
    }

    private static func deleteItems(ids: [Int64]) async -> Bool {
        for attempt in 0..<3 {
            let error: Error? = await withCheckedContinuation { continuation in
                CSSearchableIndex.default().deleteSearchableItems(
                    withIdentifiers: ids.map { "fileid-\($0)" }
                ) { continuation.resume(returning: $0) }
            }
            guard let error else { return true }
            if attempt < 2 {
                try? await Task.sleep(for: .milliseconds(250))
            } else {
                let ns = error as NSError
                NSLog("FileID Spotlight: deindex failed — \(ns.domain) \(ns.code)")
            }
        }
        return false
    }

    // MARK: - Internals

    private static func readRows(
        _ q: DatabaseQueue, afterID: Int64, limit: Int
    ) throws -> [SpotlightRow] {
        return try q.read { db -> [SpotlightRow] in
            let raw = try Row.fetchAll(db, sql: """
                SELECT files.id, files.path_text, files.kind,
                       files.vlm_proposed_name, files.vlm_description,
                       (SELECT GROUP_CONCAT(tag, '|')
                          FROM tags WHERE tags.file_id = files.id) AS taglist
                FROM files
                WHERE files.failed = 0 AND files.id > ?
                ORDER BY files.id
                LIMIT ?
                """, arguments: [afterID, limit])
            return raw.compactMap { r -> SpotlightRow? in
                guard let id: Int64 = r["id"],
                      let path: String = r["path_text"],
                      let kind: String = r["kind"] else { return nil }
                let tagList: String = r["taglist"] ?? ""
                let tags = tagList.split(separator: "|").map(String.init)
                return SpotlightRow(
                    id: id, path: path, kind: kind,
                    smartName: r["vlm_proposed_name"],
                    description: r["vlm_description"],
                    tags: tags
                )
            }
        }
    }

    private static func makeItem(_ r: SpotlightRow) -> CSSearchableItem {
        let contentType: UTType
        switch r.kind {
        case "image": contentType = .image
        case "video": contentType = .video
        case "pdf":   contentType = .pdf
        case "doc":   contentType = .data
        case "audio": contentType = .audio
        default:      contentType = .item
        }
        let attrs = CSSearchableItemAttributeSet(contentType: contentType)
        // Title: prefer the smart name if present, else basename.
        // Spotlight shows "Mia at Beach" not "IMG_5512.jpg".
        if let smart = r.smartName, !smart.isEmpty {
            attrs.title = smart
            attrs.alternateNames = [URL(fileURLWithPath: r.path).lastPathComponent]
        } else {
            attrs.title = URL(fileURLWithPath: r.path).lastPathComponent
        }
        attrs.contentDescription = r.description
        attrs.keywords = r.tags
        attrs.contentURL = URL(fileURLWithPath: r.path)
        attrs.identifier = "fileid-\(r.id)"
        return CSSearchableItem(
            uniqueIdentifier: "fileid-\(r.id)",
            domainIdentifier: domainIdentifier,
            attributeSet: attrs
        )
    }
}
