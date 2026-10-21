import Foundation
import GRDB
import FileIDShared

/// Structured Chat answers from fixed SQL templates. Mirrors Rust
/// `commands::chat_facts`. No model ever writes SQL: every predicate below is
/// a constant with bound parameters.
enum ChatFacts {
    static let analyzable = ["image", "video", "pdf", "doc", "audio", "model"]
    static let shown = 50
    /// Above this many matches a listing walks the date or size index; below
    /// it, SQLite sorts the matches. Same threshold in Rust `chat_facts`.
    static let large: Int64 = 2_000
    private static let taken = "COALESCE(f.created_at,f.modified_at)"

    struct Filter: Sendable {
        var kinds: [String] = []
        var personIDs: [Int64] = []
        var personNames: [String] = []
        var date: ChatIntent.DateFilter?
        /// `named:<folder>` or an absolute path.
        var folder: String?
        var undescribed = false
        var ids: [Int64]?

        var effectiveKinds: [String] { kinds.isEmpty && undescribed ? ChatFacts.analyzable : kinds }

        /// The SQL predicate over `files f`, appending its parameters.
        func clause(_ args: inout [DatabaseValueConvertible?]) -> String { predicate(&args, ordered: false) }

        /// `ordered` hides the kind column from the planner (`+f.kind`) so a
        /// large listing walks the ORDER BY index instead of sorting every match.
        func predicate(_ args: inout [DatabaseValueConvertible?], ordered: Bool) -> String {
            var parts = ["f.failed=0"]
            if !effectiveKinds.isEmpty {
                parts.append("\(ordered ? "+" : "")f.kind IN (SELECT value FROM json_each(?))"); args.append(ChatFacts.json(effectiveKinds))
            }
            if !personIDs.isEmpty {
                parts.append("f.id IN (SELECT file_id FROM face_prints WHERE person_id IN (SELECT value FROM json_each(?)) UNION SELECT file_id FROM catalog_observations WHERE person_id IN (SELECT value FROM json_each(?)) AND stale=0)")
                args.append(ChatFacts.json(personIDs)); args.append(ChatFacts.json(personIDs))
            }
            if let ids {
                parts.append("f.id IN (SELECT value FROM json_each(?))"); args.append(ChatFacts.json(ids))
            }
            if let date {
                if let from = date.from { parts.append("\(ChatFacts.taken)>=\(ChatFacts.modifiers(from, &args))") }
                if let to = date.to { parts.append("\(ChatFacts.taken)<\(ChatFacts.modifiers(to, &args))") }
                if let monthDay = date.monthDay {
                    parts.append("strftime('%m-%d',\(ChatFacts.taken),'unixepoch','localtime')=?"); args.append(monthDay)
                }
                if let month = date.month {
                    parts.append("strftime('%m',\(ChatFacts.taken),'unixepoch','localtime')=?"); args.append(month)
                }
            }
            if let folder {
                parts.append("f.path_text LIKE ? ESCAPE '\\'")
                if folder.hasPrefix("named:") {
                    args.append("%/\(ChatFacts.likeEscape(String(folder.dropFirst(6))))/%")
                } else {
                    var trimmed = Substring(folder)
                    while let last = trimmed.last, last == "/" || last == "\\" { trimmed = trimmed.dropLast() }
                    args.append("\(ChatFacts.likeEscape(String(trimmed)))/%")
                }
            }
            if undescribed { parts.append("(f.vlm_description IS NULL OR trim(f.vlm_description)='')") }
            return parts.joined(separator: " AND ")
        }

        /// " of Kyle in Downloads from 2023 not described yet".
        var qualifiers: String {
            var text = ""
            if !personNames.isEmpty { text += " of \(personNames.joined(separator: " and "))" }
            if let folder { text += " in \(folder.hasPrefix("named:") ? String(folder.dropFirst(6)) : folder)" }
            if let date { text += " \(date.phrase)" }
            if undescribed { text += " not described yet" }
            return text
        }
    }

    static func json<T: Encodable>(_ value: T) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        return (try? String(decoding: encoder.encode(value), as: UTF8.self)) ?? "[]"
    }

    static func likeEscape(_ text: String) -> String {
        text.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "%", with: "\\%").replacingOccurrences(of: "_", with: "\\_")
    }

    private static func modifiers(_ parts: [String], _ args: inout [DatabaseValueConvertible?]) -> String {
        args.append(contentsOf: parts.map { $0 as DatabaseValueConvertible? })
        return "CAST(strftime('%s',\(Array(repeating: "?", count: parts.count).joined(separator: ",")),'utc') AS REAL)"
    }

    static func fileName(_ path: String) -> String {
        let name = (path as NSString).lastPathComponent
        return name.isEmpty || name == "/" ? path : name
    }

    static func capitalized(_ text: String) -> String { (text.first.map { String($0).uppercased() } ?? "") + text.dropFirst() }

    /// Count and total size of the matching files.
    static func totals(_ db: GRDB.Database, _ filter: Filter) throws -> (count: Int64, bytes: Int64) {
        var args: [DatabaseValueConvertible?] = []
        let clause = filter.clause(&args)
        let row = try Row.fetchOne(db, sql: "SELECT COUNT(*),COALESCE(SUM(f.size_bytes),0) FROM files f WHERE \(clause)", arguments: StatementArguments(args))
        return (row?[0] ?? 0, row?[1] ?? 0)
    }

    /// Matching files as citations, in the requested order.
    static func listed(_ db: GRDB.Database, _ filter: Filter, order: String, limit: Int, count: Int64) throws -> [CatalogHit] {
        var args: [DatabaseValueConvertible?] = []
        let clause = filter.predicate(&args, ordered: count > large)
        let ordering = switch order {
        case "largest": "f.size_bytes DESC,f.id DESC"
        case "smallest": "f.size_bytes ASC,f.id ASC"
        case "oldest": "\(taken) IS NULL,\(taken) ASC,f.id ASC"
        default: "\(taken) DESC,f.id DESC"
        }
        args.append(limit)
        return try Row.fetchAll(db, sql: "SELECT f.id,f.path_text,f.kind,f.size_bytes,strftime('%Y-%m-%d',\(taken),'unixepoch','localtime') AS day FROM files f WHERE \(clause) ORDER BY \(ordering) LIMIT ?", arguments: StatementArguments(args)).map { row in
            let size: Int64 = row["size_bytes"]
            let day: String? = row["day"]
            return CatalogHit(fileID: row["id"], path: row["path_text"], kind: row["kind"],
                              text: day.map { "\(ChatIntent.bytesLabel(size)) · \($0)" } ?? ChatIntent.bytesLabel(size))
        }
    }

    struct Copy: Sendable { let id: Int64; let keeper: Int64; let path: String; let keeperPath: String; let size: Int64 }

    /// Extra copies of files that share a stored content hash and size, with
    /// the copy Cleanup would keep: best-looking, largest, oldest, shortest path.
    struct Extras: Sendable { let groups: Int64; let count: Int64; let bytes: Int64; let copies: [Copy] }

    static func extras(_ db: GRDB.Database, _ filter: Filter, limit: Int) throws -> Extras {
        var args: [DatabaseValueConvertible?] = []
        let clause = filter.clause(&args)
        let window = "PARTITION BY f.content_hash,f.size_bytes ORDER BY COALESCE(f.aesthetic,0) DESC,f.size_bytes DESC,COALESCE(f.created_at,1e308) ASC,length(f.path_text) ASC,f.path_text ASC,f.id ASC"
        let base = """
            WITH twins AS (SELECT content_hash AS h,size_bytes AS s FROM files WHERE content_hash IS NOT NULL AND failed=0 GROUP BY content_hash,size_bytes HAVING COUNT(*)>1), \
            ranked AS (SELECT f.id,f.path_text,f.size_bytes,ROW_NUMBER() OVER (\(window)) AS rank,FIRST_VALUE(f.id) OVER (\(window)) AS keeper,FIRST_VALUE(f.path_text) OVER (\(window)) AS keeper_path \
            FROM files f JOIN twins ON f.content_hash=twins.h AND f.size_bytes=twins.s WHERE f.failed=0), \
            chosen AS (SELECT r.* FROM ranked r JOIN files f ON f.id=r.id WHERE r.rank>1 AND \(clause))
            """
        var keepers = Set<Int64>()
        var count: Int64 = 0
        var bytes: Int64 = 0
        var copies: [Copy] = []
        let rows = try Row.fetchCursor(db, sql: base + " SELECT id,keeper,path_text,keeper_path,size_bytes FROM chosen ORDER BY size_bytes DESC,path_text ASC,id ASC", arguments: StatementArguments(args))
        while let row = try rows.next() {
            let keeper: Int64 = row["keeper"]
            let size: Int64 = row["size_bytes"]
            keepers.insert(keeper)
            count += 1
            bytes += size
            if copies.count < limit {
                copies.append(Copy(id: row["id"], keeper: keeper, path: row["path_text"], keeperPath: row["keeper_path"], size: size))
            }
        }
        return Extras(groups: Int64(keepers.count), count: count, bytes: bytes, copies: copies)
    }

    static func extraHits(_ extras: Extras) -> [CatalogHit] {
        extras.copies.prefix(shown).map {
            CatalogHit(fileID: $0.id, path: $0.path, kind: "duplicate", text: "Extra copy of \(fileName($0.keeperPath)) · \(ChatIntent.bytesLabel($0.size))")
        }
    }

    static func duplicatesSentence(_ extras: Extras, _ qualifiers: String) -> String {
        guard extras.count > 0 else { return "No identical copies found\(qualifiers)." }
        return "Found \(ChatIntent.countLabel(extras.count)) \(extras.count == 1 ? "extra copy" : "extra copies") of \(ChatIntent.countLabel(extras.groups)) \(extras.groups == 1 ? "file" : "files")\(qualifiers), using \(ChatIntent.bytesLabel(extras.bytes)). Cleanup checks every copy byte by byte before anything moves."
    }

    /// Answers a question that is fully structured: no subject words remain.
    static func answer(_ db: GRDB.Database, _ question: ChatIntent.Question, _ filter: Filter) throws -> (String, [CatalogHit]) {
        let qualifiers = filter.qualifiers
        if question.duplicates {
            let found = try extras(db, filter, limit: shown)
            let message = question.measure == "size" && found.count > 0
                ? "Extra copies of identical files\(qualifiers) use \(ChatIntent.bytesLabel(found.bytes)) (\(ChatIntent.countLabel(found.count)) \(found.count == 1 ? "copy" : "copies") of \(ChatIntent.countLabel(found.groups)) \(found.groups == 1 ? "file" : "files")). Cleanup checks every copy byte by byte before anything moves."
                : duplicatesSentence(found, qualifiers)
            return (message, extraHits(found))
        }
        let kinds = filter.kinds
        let (count, bytes) = try totals(db, filter)
        let hits = try listed(db, filter, order: question.order, limit: shown, count: count)
        guard count > 0 else { return ("No \(ChatIntent.noun(kinds, 2))\(qualifiers).", hits) }
        let message: String
        switch question.measure {
        case "size":
            message = "\(capitalized(ChatIntent.noun(kinds, 2)))\(qualifiers) use \(ChatIntent.bytesLabel(bytes)) (\(ChatIntent.countLabel(count)) \(ChatIntent.noun(kinds, count)))."
        case "count":
            message = "Found \(ChatIntent.countLabel(count)) \(ChatIntent.noun(kinds, count))\(qualifiers), using \(ChatIntent.bytesLabel(bytes))."
        default:
            let order = question.explicitOrder ? " \(capitalized(question.order)) first." : ""
            message = "Found \(ChatIntent.countLabel(count)) \(ChatIntent.noun(kinds, count))\(qualifiers).\(order)"
        }
        return (message, hits)
    }

    /// Keeps only hits whose files pass the structured filter, preserving order.
    static func restrict(_ db: GRDB.Database, _ hits: [CatalogHit], _ filter: Filter) throws -> [CatalogHit] {
        guard !hits.isEmpty else { return hits }
        var scoped = filter
        scoped.ids = Array(Set(hits.map(\.fileID))).sorted()
        var args: [DatabaseValueConvertible?] = []
        let clause = scoped.clause(&args)
        let keep = Set(try Int64.fetchAll(db, sql: "SELECT f.id FROM files f WHERE \(clause)", arguments: StatementArguments(args)))
        return hits.filter { keep.contains($0.fileID) }
    }
}
