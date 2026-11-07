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
        /// Lowercase file extensions ("raw" covers every RAW type).
        var extensions: [String] = []
        /// Every named person must appear ("Kyle and Maya"), not any of them.
        var allPeople = false
        /// The subject words behind `ids`, for the reply.
        var subject: String?
        /// People the question leaves out ("without Maya").
        var excludedPersonIDs: [Int64] = []
        var excludedPersonNames: [String] = []
        /// Exclusive byte bounds.
        var minBytes: Int64?
        var maxBytes: Int64?
        /// The library location the app is scoped to (`rootID`).
        var root: LibraryRoots.Scope?

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
            if !extensions.isEmpty {
                parts.append("lower(f.extension) IN (SELECT value FROM json_each(?))"); args.append(ChatFacts.json(extensions))
            }
            if allPeople, personIDs.count > 1 {
                for person in Set(personIDs).sorted() {
                    parts.append("f.id IN (SELECT file_id FROM face_prints WHERE person_id=? UNION SELECT file_id FROM catalog_observations WHERE person_id=? AND stale=0)")
                    args.append(person); args.append(person)
                }
            } else if !personIDs.isEmpty {
                parts.append("f.id IN (SELECT file_id FROM face_prints WHERE person_id IN (SELECT value FROM json_each(?)) UNION SELECT file_id FROM catalog_observations WHERE person_id IN (SELECT value FROM json_each(?)) AND stale=0)")
                args.append(ChatFacts.json(personIDs)); args.append(ChatFacts.json(personIDs))
            }
            if !excludedPersonIDs.isEmpty {
                parts.append("f.id NOT IN (SELECT file_id FROM face_prints WHERE person_id IN (SELECT value FROM json_each(?)) UNION SELECT file_id FROM catalog_observations WHERE person_id IN (SELECT value FROM json_each(?)) AND stale=0)")
                args.append(ChatFacts.json(excludedPersonIDs)); args.append(ChatFacts.json(excludedPersonIDs))
            }
            if let minBytes { parts.append("f.size_bytes>?"); args.append(minBytes) }
            if let maxBytes { parts.append("f.size_bytes<?"); args.append(maxBytes) }
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
            if let root { parts.append(LibraryRoots.scopeSQL("f.path_text")); args.append(root.param) }
            return parts.joined(separator: " AND ")
        }

        /// " of Kyle in Downloads from 2023 not described yet".
        var qualifiers: String {
            var text = ""
            if let subject, !subject.isEmpty { text += " matching “\(subject)”" }
            if !extensions.isEmpty { text += " in \(ChatFacts.formatLabel(extensions)) format" }
            if !personNames.isEmpty { text += " of \(personNames.joined(separator: allPeople || personNames.count == 1 ? " and " : " or "))" }
            if !excludedPersonNames.isEmpty { text += " without \(excludedPersonNames.joined(separator: " or "))" }
            if let minBytes { text += " over \(ChatIntent.bytesLabel(minBytes))" }
            if let maxBytes { text += " under \(ChatIntent.bytesLabel(maxBytes))" }
            if let folder { text += " in \(folder.hasPrefix("named:") ? String(folder.dropFirst(6)) : folder)" }
            if let date { text += " \(date.phrase)" }
            if undescribed { text += " not described yet" }
            return text
        }
    }

    /// "RAW" for the RAW family, else the first extension in capitals ("JPG", "PNG").
    static func formatLabel(_ extensions: [String]) -> String {
        extensions.contains("arw") && extensions.contains("cr2") ? "RAW" : (extensions.first ?? "").uppercased()
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

    struct FolderTotal: Sendable { var name: String; let path: String; let count: Int64; let bytes: Int64 }

    /// Folders holding the most files (or bytes), from the stored paths. Both path
    /// separators are folded so a catalog scanned on Windows reads the same.
    static func folders(_ db: GRDB.Database, bySize: Bool, limit: Int, root: LibraryRoots.Scope? = nil) throws -> [FolderTotal] {
        let path = "replace(f.path_text,char(92),'/')"
        let order = bySize ? "b DESC,c DESC" : "c DESC,b DESC"
        var found = try Row.fetchAll(db, sql: "SELECT rtrim(\(path),replace(\(path),'/','')) AS dir,COUNT(*) AS c,COALESCE(SUM(f.size_bytes),0) AS b FROM files f WHERE f.failed=0 AND \(LibraryRoots.scopeSQL("f.path_text")) GROUP BY dir ORDER BY \(order),dir ASC LIMIT ?", arguments: [root?.param, limit]).map { row -> FolderTotal in
            var dir: String = row["dir"]
            while dir.count > 1, dir.hasSuffix("/") { dir.removeLast() }
            return FolderTotal(name: dir.split(separator: "/").last.map(String.init) ?? dir, path: dir, count: row["c"], bytes: row["b"])
        }
        // Widen the label one folder at a time until no two folders share it.
        var depth = 1
        while depth < 8 {
            let repeated = Set(Dictionary(grouping: found, by: \.name).filter { $0.value.count > 1 }.keys)
            if repeated.isEmpty { break }
            depth += 1
            for index in found.indices where repeated.contains(found[index].name) {
                let parts = found[index].path.split(separator: "/")
                if parts.count >= depth { found[index].name = parts.suffix(depth).joined(separator: "/") }
            }
        }
        return found
    }

    static func folderPhrase(_ folder: FolderTotal) -> String {
        "\(folder.name) (\(ChatIntent.countLabel(folder.count)) \(folder.count == 1 ? "file" : "files"), \(ChatIntent.bytesLabel(folder.bytes)))"
    }

    private static func libraryAnswer(_ db: GRDB.Database, _ question: ChatIntent.Question, root: LibraryRoots.Scope?) throws -> (String, [CatalogHit]) {
        let everything = Filter(root: root)
        let scoped: StatementArguments = [root?.param]
        let (count, bytes) = try totals(db, everything)
        let hits = try listed(db, everything, order: "newest", limit: shown, count: count)
        guard count > 0 else { return ("Your library is empty. Scan a folder to get started.", hits) }
        if question.measure == "folders" {
            let bySize = question.order == "largest"
            let top = try folders(db, bySize: bySize, limit: 5, root: root)
            return ("\(bySize ? "Biggest folders" : "Folders with the most files"): \(top.map(folderPhrase).joined(separator: ", ")).", hits)
        }
        if question.measure == "formats" {
            let formats = try Row.fetchAll(db, sql: """
                SELECT upper(COALESCE(NULLIF(f.extension,''),'none')) AS e,COUNT(*) AS c FROM files f WHERE f.failed=0 AND \(LibraryRoots.scopeSQL("f.path_text"))
                GROUP BY lower(COALESCE(NULLIF(f.extension,''),'none')) ORDER BY c DESC,e ASC
                """, arguments: scoped).map { row -> String in
                    let n: Int64 = row["c"]
                    let ext: String = row["e"]
                    return "\(ChatIntent.countLabel(n)) \(ext)"
                }
            let more = formats.count > 10 ? " and \(formats.count - 10) more" : ""
            return ("Your \(ChatIntent.countLabel(count)) \(count == 1 ? "file uses" : "files use") \(formats.count) \(formats.count == 1 ? "format" : "formats"): \(formats.prefix(10).joined(separator: ", "))\(more).", hits)
        }
        let kinds = try Row.fetchAll(db, sql: "SELECT f.kind AS k,COUNT(*) AS c FROM files f WHERE f.failed=0 AND \(LibraryRoots.scopeSQL("f.path_text")) GROUP BY f.kind ORDER BY c DESC,k ASC", arguments: scoped)
            .map { row -> String in
                let n: Int64 = row["c"]
                let kind: String = row["k"]
                return "\(ChatIntent.countLabel(n)) \(ChatIntent.noun([kind], n))"
            }
        var message = "Your library has \(ChatIntent.countLabel(count)) \(count == 1 ? "file" : "files") using \(ChatIntent.bytesLabel(bytes)): \(kinds.joined(separator: ", "))."
        if let range = try Row.fetchOne(db, sql: "SELECT strftime('%Y-%m-%d',MIN(\(taken)),'unixepoch','localtime') AS lo,strftime('%Y-%m-%d',MAX(\(taken)),'unixepoch','localtime') AS hi FROM files f WHERE f.failed=0 AND \(LibraryRoots.scopeSQL("f.path_text"))", arguments: scoped),
           let lo: String = range["lo"], let hi: String = range["hi"] {
            message += lo == hi ? " Taken on \(lo)." : " Dates run from \(lo) to \(hi)."
        }
        let top = try folders(db, bySize: false, limit: 3, root: root)
        if !top.isEmpty { message += " Busiest folders: \(top.map(folderPhrase).joined(separator: ", "))." }
        return (message, hits)
    }

    /// Answers a question that is fully structured: no subject words remain.
    static func answer(_ db: GRDB.Database, _ question: ChatIntent.Question, _ filter: Filter) throws -> (String, [CatalogHit]) {
        if ["overview", "folders", "formats"].contains(question.measure) { return try libraryAnswer(db, question, root: filter.root) }
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
        let hits = try listed(db, filter, order: question.order, limit: min(question.limit ?? shown, shown), count: count)
        guard count > 0 else { return ("No \(ChatIntent.noun(kinds, 2))\(qualifiers).", hits) }
        if let limit = question.limit, question.measure == "list" {
            let names = hits.map { "\(fileName($0.path)) (\($0.text))" }.joined(separator: ", ")
            let top = Int64(min(Int64(limit), count))
            let ordered = question.explicitOrder ? "\(question.order) " : ""
            return ("The \(top == 1 ? "" : "\(ChatIntent.countLabel(top)) ")\(ordered)\(ChatIntent.noun(kinds, top))\(qualifiers): \(names).", hits)
        }
        if let unit = question.breakdown {
            var args: [DatabaseValueConvertible?] = []
            let clause = filter.clause(&args)
            let format = unit == "year" ? "%Y" : "%Y-%m"
            let groups = try Row.fetchAll(db, sql: "SELECT strftime('\(format)',\(taken),'unixepoch','localtime') AS g,COUNT(*) AS c,COALESCE(SUM(f.size_bytes),0) AS b FROM files f WHERE \(clause) GROUP BY g ORDER BY g IS NULL,g DESC", arguments: StatementArguments(args))
            let parts = groups.prefix(24).map { row -> String in
                let n: Int64 = row["c"]
                let label: String = row["g"] ?? "no date"
                return question.measure == "size" ? "\(label): \(ChatIntent.bytesLabel(row["b"]))" : "\(label): \(ChatIntent.countLabel(n))"
            }
            let more = groups.count > 24 ? " and \(groups.count - 24) more" : ""
            return ("\(capitalized(ChatIntent.noun(kinds, 2)))\(qualifiers) by \(unit): \(parts.joined(separator: ", "))\(more).", hits)
        }
        let message: String
        switch question.measure {
        case "size":
            let largest = question.explicitOrder && question.order == "largest" && !hits.isEmpty
                ? " Largest: \(hits.prefix(3).map { "\(fileName($0.path)) (\($0.text.components(separatedBy: " · ").first ?? ""))" }.joined(separator: ", "))."
                : ""
            message = "\(capitalized(ChatIntent.noun(kinds, 2)))\(qualifiers) use \(ChatIntent.bytesLabel(bytes)) (\(ChatIntent.countLabel(count)) \(ChatIntent.noun(kinds, count))).\(largest)"
        case "count":
            message = "Found \(ChatIntent.countLabel(count)) \(ChatIntent.noun(kinds, count))\(qualifiers), using \(ChatIntent.bytesLabel(bytes))."
        default:
            let order = question.explicitOrder ? " \(capitalized(question.order)) first." : ""
            message = "Found \(ChatIntent.countLabel(count)) \(ChatIntent.noun(kinds, count))\(qualifiers).\(order)"
        }
        return (message, hits)
    }

    /// Words that only join a subject to the question ("about taxes", "photos showing a dog").
    static let subjectFillers: Set<String> = [
        "about", "of", "the", "a", "an", "and", "with", "in", "on", "at", "to", "for", "from", "showing", "shows", "show",
        "containing", "contains", "contain", "that", "which", "any", "some", "whats", "what", "is", "are", "there", "where",
        "who", "me", "my", "i", "do", "does", "have", "has", "s", "mentioning", "mention", "mentions", "named", "called",
        "titled", "labeled", "labelled", "regarding", "related", "them", "they", "it", "its", "those", "these", "ones"
    ]
    static let subjectLimit = 200_000

    /// Subject words split into alternatives on "or": "beach or sunset" → [[beach], [sunset]].
    /// Each subject word costs several index lookups, so a pasted paragraph is cut
    /// to the first few alternatives and words (Rust `subject_groups`).
    static let subjectMaxGroups = 6
    static let subjectMaxWords = 8

    static func subjectGroups(_ query: String) -> [[String]] {
        var groups: [[String]] = [[]]
        for word in query.lowercased().split(whereSeparator: { !$0.isLetter && !$0.isNumber }).map(String.init) {
            if word == "or" {
                if !(groups.last?.isEmpty ?? true) {
                    if groups.count == subjectMaxGroups { break }
                    groups.append([])
                }
            } else if !subjectFillers.contains(word), groups[groups.count - 1].count < subjectMaxWords, !groups[groups.count - 1].contains(word) {
                groups[groups.count - 1].append(word)
            }
        }
        return groups.filter { !$0.isEmpty }
    }

    /// Every file the subject describes, so counts, sizes and orders see all
    /// matches rather than the best hundred search keeps. A word matches a file's
    /// name, description, scan tags, document or photo text, or current video
    /// evidence, in its written or singular form; every word must match, and "or"
    /// joins alternatives. Nil when no subject word remains. Mirrors Rust
    /// `chat_facts::subject_ids`.
    static func subjectIDs(_ db: GRDB.Database, _ query: String) throws -> [Int64]? {
        let groups = subjectGroups(query)
        guard !groups.isEmpty else { return nil }
        var cache: [String: Set<Int64>] = [:]
        var result = Set<Int64>()
        for group in groups {
            var matched: Set<Int64>?
            for word in group {
                let found: Set<Int64>
                if let known = cache[word] { found = known } else { found = try filesMatching(db, word); cache[word] = found }
                matched = matched.map { $0.intersection(found) } ?? found
                if matched?.isEmpty == true { break }
            }
            result.formUnion(matched ?? [])
            if result.count >= subjectLimit { break }
        }
        return Array(result).sorted().prefix(subjectLimit).map { $0 }
    }

    private static func filesMatching(_ db: GRDB.Database, _ word: String) throws -> Set<Int64> {
        let forms = CatalogStore.wordForms(word)
        let match = forms.map(FTSQuery.quoted).joined(separator: " OR ")
        var ids = Set<Int64>()
        for table in ["catalog_file_fts", "doc_fts", "ocr_fts"] {
            ids.formUnion(try Int64.fetchAll(db, sql: "SELECT rowid FROM \(table) WHERE \(table) MATCH ? LIMIT ?", arguments: [match, subjectLimit]))
        }
        ids.formUnion(try Int64.fetchAll(db, sql: """
            SELECT DISTINCT CAST(e.file_id AS INTEGER) FROM catalog_evidence_fts e
            LEFT JOIN catalog_chapters c ON c.id=e.evidence_id AND e.kind='chapter'
            LEFT JOIN catalog_passages p ON p.id=e.evidence_id AND e.kind='passage'
            WHERE catalog_evidence_fts MATCH ? AND (c.stale=0 OR p.stale=0) LIMIT ?
            """, arguments: [match, subjectLimit]))
        if word.count >= 3 {
            let likes = forms.map { _ in "(' ' || REPLACE(LOWER(tag),'_',' ')) LIKE ?" }.joined(separator: " OR ")
            let arguments: [DatabaseValueConvertible?] = forms.map { "% \($0)%" } + [subjectLimit]
            ids.formUnion(try Int64.fetchAll(db, sql: "SELECT DISTINCT file_id FROM tags WHERE source IN ('auto','user','vlm') AND tag IN (SELECT tag FROM catalog_tag_vocab WHERE \(likes)) LIMIT ?",
                                             arguments: StatementArguments(arguments)))
        }
        return ids
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
