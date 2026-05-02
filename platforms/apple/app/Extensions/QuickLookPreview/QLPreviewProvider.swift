// FileID Quick Look Preview Extension.
//
// When the user presses Space on a file in Finder, macOS spawns this
// extension and asks it to produce a preview. We override Apple's
// default image/PDF preview with FileID's enriched view: the photo
// itself + the smart name + the AI caption + every detected face
// (with names) + the tag chips.
//
// Architecture:
//   - Provider opens FileID's read-only SQLite at the standard path
//   - Looks up the file by path_text
//   - Renders a SwiftUI view that overlays the enrichment on top of the
//     image (or PDF first page)
//
// Permissions: needs com.apple.security.files.user-selected.read-only
// + com.apple.security.application-groups (so the extension can read
// FileID's database in App Support). See Info.plist + the parent
// FileID app's entitlements.
import Cocoa
import QuickLookUI
import UniformTypeIdentifiers
import GRDB

class QLPreviewProvider: NSObject, QLPreviewingController {

    func providePreview(for request: QLFilePreviewRequest) async throws -> QLPreviewReply {
        let url = request.fileURL
        // Look up the file's enrichment in FileID's database.
        let info = await Self.lookup(url: url)

        // Use a SwiftUI-rendered preview. QLPreviewReply supports
        // contextual SwiftUI scenes via the `.dataOfContentType` reply
        // that we render to PDF or PNG. For maximum simplicity we ship
        // an HTML reply — Quick Look renders HTML directly.
        let html = Self.renderHTML(url: url, info: info)
        let data = html.data(using: .utf8) ?? Data()
        return QLPreviewReply(
            dataOfContentType: .html,
            contentSize: CGSize(width: 800, height: 800)
        ) { _ in
            return data
        }
    }

    // MARK: - DB lookup

    struct Enrichment {
        let smartName: String?
        let description: String?
        let tags: [String]
        let faceCount: Int
        let people: [String]   // named persons
    }

    private static func lookup(url: URL) async -> Enrichment? {
        let dbURL = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID/fileid.sqlite")
        guard FileManager.default.fileExists(atPath: dbURL.path) else { return nil }

        return await Task.detached(priority: .userInitiated) { () -> Enrichment? in
            var c = Configuration()
            c.readonly = true
            guard let q = try? DatabaseQueue(path: dbURL.path, configuration: c) else { return nil }
            return try? q.read { db -> Enrichment? in
                guard let r = try Row.fetchOne(db, sql: """
                    SELECT id, vlm_proposed_name, vlm_description
                    FROM files WHERE path_text = ? AND failed = 0
                    """, arguments: [url.path]) else { return nil }
                let fileID: Int64 = r["id"] ?? 0
                let smart: String? = r["vlm_proposed_name"]
                let desc: String? = r["vlm_description"]

                let tagRows = try Row.fetchAll(db, sql: """
                    SELECT tag FROM tags WHERE file_id = ? AND source = 'vision'
                    ORDER BY rowid LIMIT 8
                    """, arguments: [fileID])
                let tags = tagRows.compactMap { $0["tag"] as String? }

                let faceCount: Int = (try? Int.fetchOne(db, sql:
                    "SELECT COUNT(*) FROM face_prints WHERE file_id = ? AND excluded = 0",
                    arguments: [fileID])) ?? 0

                let personRows = try Row.fetchAll(db, sql: """
                    SELECT DISTINCT COALESCE(persons.first_name, persons.name) AS name
                    FROM persons
                    INNER JOIN face_prints ON face_prints.person_id = persons.id
                    WHERE face_prints.file_id = ?
                      AND IFNULL(persons.is_unknown, 0) = 0
                      AND COALESCE(persons.first_name, persons.name) IS NOT NULL
                    """, arguments: [fileID])
                let people = personRows.compactMap { $0["name"] as String? }

                return Enrichment(
                    smartName: smart, description: desc,
                    tags: tags, faceCount: faceCount, people: people
                )
            }
        }.value
    }

    // MARK: - Rendering

    private static func renderHTML(url: URL, info: Enrichment?) -> String {
        let imgSrc = "file://" + url.path.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed)!
        let smartLine: String
        if let smart = info?.smartName, !smart.isEmpty {
            smartLine = """
            <div class="smart">\(esc(smart)).\(esc(url.pathExtension))</div>
            <div class="orig">was \(esc(url.lastPathComponent))</div>
            """
        } else {
            smartLine = "<div class=\"orig\">\(esc(url.lastPathComponent))</div>"
        }
        let captionLine: String
        if let desc = info?.description, !desc.isEmpty {
            captionLine = "<p class=\"caption\">\(esc(desc))</p>"
        } else { captionLine = "" }
        let tagChips = (info?.tags ?? []).map {
            "<span class=\"chip\">\(esc(formatTag($0)))</span>"
        }.joined()
        let peopleLine: String
        if let people = info?.people, !people.isEmpty {
            peopleLine = "<p class=\"people\">👤 " +
                people.map(esc).joined(separator: ", ") + "</p>"
        } else if let count = info?.faceCount, count > 0 {
            peopleLine = "<p class=\"people\">\(count) face\(count == 1 ? "" : "s") detected</p>"
        } else {
            peopleLine = ""
        }

        return """
        <!doctype html>
        <html><head><meta charset="utf-8"><style>
        body { font-family: -apple-system, sans-serif; background: #0a0a0a; color: #fafafa; margin: 0; padding: 24px; }
        img { max-width: 100%; max-height: 480px; border-radius: 8px; display: block; margin: 0 auto 16px; }
        .smart { font-size: 24px; font-weight: 600; color: #FFCD3C; margin-top: 8px; }
        .orig { font-size: 12px; color: #888; font-family: ui-monospace, Menlo, monospace; margin-bottom: 12px; }
        .caption { font-size: 16px; line-height: 1.5; margin: 12px 0; }
        .people { font-size: 14px; color: #B19BCE; margin: 8px 0; }
        .chip { display: inline-block; padding: 3px 8px; margin: 2px 4px 2px 0;
                background: rgba(255,205,60,0.12); color: #FFCD3C;
                border-radius: 4px; font-size: 11px; font-weight: 500; }
        .chips { margin-top: 12px; }
        </style></head><body>
        <img src="\(imgSrc)" alt="">
        \(smartLine)
        \(captionLine)
        \(peopleLine)
        <div class="chips">\(tagChips)</div>
        </body></html>
        """
    }

    private static func formatTag(_ raw: String) -> String {
        if raw.contains(" ") { return raw }
        let last = raw.split(separator: "_").last.map(String.init) ?? raw
        guard let first = last.first else { return last }
        return first.uppercased() + last.dropFirst()
    }

    private static func esc(_ s: String) -> String {
        s.replacingOccurrences(of: "&", with: "&amp;")
            .replacingOccurrences(of: "<", with: "&lt;")
            .replacingOccurrences(of: ">", with: "&gt;")
            .replacingOccurrences(of: "\"", with: "&quot;")
    }
}
