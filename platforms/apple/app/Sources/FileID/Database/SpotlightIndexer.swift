// CoreSpotlight bridge — registers FileID's enriched metadata
// (smart names, captions, tags) so a ⌘Space query finds the photos
// from anywhere on macOS. Items are scoped to "com.fileid.photos"
// for clean wipe/reindex.
import Foundation
import CoreSpotlight
import UniformTypeIdentifiers
import FileIDShared
import GRDB

fileprivate struct SpotlightRow: Sendable {
    let id: Int64
    let path: String
    let kind: String
    let smartName: String?
    let description: String?
    let tags: [String]
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

    fileprivate static func indexPass(dbPath: String) async -> Bool {
        var afterID: Int64 = -1
        while true {
            let pageStart = afterID
            var rows: [SpotlightRow]?
            for attempt in 0..<2 {
                let result = await Task.detached(priority: .background) {
                    Result {
                        try readRows(dbPath: dbPath, afterID: pageStart, limit: batchSize)
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
            guard let rows else { return false }
            guard !rows.isEmpty else { return true }
            let items = rows.map(makeItem)
            do {
                try await CSSearchableIndex.default().indexSearchableItems(items)
            } catch {
                let ns = error as NSError
                NSLog("FileID Spotlight: batch index failed — \(ns.domain) \(ns.code)")
                return false
            }
            afterID = rows[rows.count - 1].id
        }
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
            guard let error else { return true }
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

    fileprivate static func performDeindex(ids: [Int64]) async -> Bool {
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
        dbPath: String, afterID: Int64, limit: Int
    ) throws -> [SpotlightRow] {
        var c = Configuration()
        c.readonly = true
        let q = try DatabaseQueue(path: dbPath, configuration: c)
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
