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

public enum SpotlightIndexer {

    public static let domainIdentifier = "com.fileid.photos"

    /// Bulk re-index every file currently in the DB. Idempotent —
    /// CSSearchableIndex de-duplicates by uniqueIdentifier, so calling
    /// this repeatedly keeps the index in sync.
    public static func indexAll(dbPath: String) async {
        let rows: [SpotlightRow] = await Task.detached(priority: .background) {
            readRows(dbPath: dbPath)
        }.value
        guard !rows.isEmpty else { return }
        let items = rows.map(makeItem)
        do {
            try await CSSearchableIndex.default().indexSearchableItems(items)
        } catch {
            // Log silently — Spotlight failures shouldn't surface as
            // user errors; the app still works.
            NSLog("FileID Spotlight: bulk index failed — \(error)")
        }
    }

    /// Wipe every FileID-owned item from Spotlight. Exposed for a
    /// future "Delete my data" flow on the Privacy card.
    public static func wipe() {
        CSSearchableIndex.default().deleteSearchableItems(
            withDomainIdentifiers: [domainIdentifier]
        )
    }

    // MARK: - Internals

    private static func readRows(dbPath: String) -> [SpotlightRow] {
        var c = Configuration()
        c.readonly = true
        guard let q = try? DatabaseQueue(path: dbPath, configuration: c) else { return [] }
        return (try? q.read { db -> [SpotlightRow] in
            let raw = try Row.fetchAll(db, sql: """
                SELECT files.id, files.path_text, files.kind,
                       files.vlm_proposed_name, files.vlm_description,
                       (SELECT GROUP_CONCAT(tag, '|')
                          FROM tags WHERE tags.file_id = files.id) AS taglist
                FROM files
                WHERE files.failed = 0
                """)
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
        }) ?? []
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
