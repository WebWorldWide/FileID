import Foundation

/// Deterministic Chat parsing. Mirrors Rust `commands::chat_intent` line for
/// line; the shared cases live in `shared/test-corpus/chat-intents.json`.
///
/// Only the user's own words are parsed. Retrieved file text never reaches
/// these functions, so instructions inside documents cannot start actions.
struct ChatIntent: Codable, Equatable, Sendable {
    var kind: String
    var folder: String?
    var query: String?
    var name: String?
    var people: [String] = []
    var page: String?
    var job: String?
    var setting: String?
    var value: Bool?
    var scope: String?

    init(_ kind: String) { self.kind = kind }

    private enum CodingKeys: String, CodingKey { case kind, folder, query, name, people, page, job, setting, value, scope }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? ""
        folder = try c.decodeIfPresent(String.self, forKey: .folder)
        query = try c.decodeIfPresent(String.self, forKey: .query)
        name = try c.decodeIfPresent(String.self, forKey: .name)
        people = try c.decodeIfPresent([String].self, forKey: .people) ?? []
        page = try c.decodeIfPresent(String.self, forKey: .page)
        job = try c.decodeIfPresent(String.self, forKey: .job)
        setting = try c.decodeIfPresent(String.self, forKey: .setting)
        value = try c.decodeIfPresent(Bool.self, forKey: .value)
        scope = try c.decodeIfPresent(String.self, forKey: .scope)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(kind, forKey: .kind)
        try c.encodeIfPresent(folder, forKey: .folder)
        try c.encodeIfPresent(query, forKey: .query)
        try c.encodeIfPresent(name, forKey: .name)
        if !people.isEmpty { try c.encode(people, forKey: .people) }
        try c.encodeIfPresent(page, forKey: .page)
        try c.encodeIfPresent(job, forKey: .job)
        try c.encodeIfPresent(setting, forKey: .setting)
        try c.encodeIfPresent(value, forKey: .value)
        try c.encodeIfPresent(scope, forKey: .scope)
    }

    /// A date window over `COALESCE(created_at, modified_at)`. `from`/`to` are
    /// SQLite date-modifier lists, evaluated in local time and converted to UTC.
    struct DateFilter: Codable, Equatable, Sendable {
        var from: [String]?
        var to: [String]?
        var monthDay: String?
        var month: String?
        var phrase: String = ""
    }

    /// The structured part of a question.
    struct Question: Codable, Equatable, Sendable {
        var measure = "list"
        var order = "newest"
        var explicitOrder = false
        var date: DateFilter?
        var folder: String?
        var duplicates = false
        var undescribed = false
        var reduced = ""

        var structured: Bool {
            measure != "list" || explicitOrder || date != nil || folder != nil || duplicates || undescribed
        }
    }

    static let questionWords: Set<String> = ["how", "what", "which", "where", "when", "who", "why", "is", "are", "do", "does", "did", "was", "were", "has", "have"]
    static let knownFolders: Set<String> = ["downloads", "desktop", "documents", "pictures", "movies", "videos", "music", "home", "docs"]
    static let duplicateWords: Set<String> = ["duplicate", "duplicates", "dupes", "dupe", "duplicated", "copies"]

    /// Lowercased word with surrounding punctuation removed; inner punctuation stays.
    static func clean(_ word: String) -> String { original(word).lowercased() }

    static func original(_ word: String) -> String {
        var text = Substring(word.replacingOccurrences(of: "’", with: "'"))
        while let first = text.first, "\"'“”‘(".contains(first) { text = text.dropFirst() }
        while let last = text.last, "\"'“”’).,!?;:".contains(last) { text = text.dropLast() }
        return String(text)
    }

    static func split(_ text: String) -> [String] {
        text.split(whereSeparator: \.isWhitespace).map(String.init)
    }

    private static func starts(_ words: ArraySlice<String>, _ prefix: [String]) -> Bool {
        words.count >= prefix.count && zip(words, prefix).allSatisfy { $0 == $1 }
    }
    private static func starts(_ words: [String], _ prefix: [String]) -> Bool { starts(words[...], prefix) }

    private static func has<S: Sequence>(_ words: S, _ any: Set<String>) -> Bool where S.Element == String {
        words.contains { any.contains($0) }
    }

    private static func hasPrefix<S: Sequence>(_ words: S, _ stems: [String]) -> Bool where S.Element == String {
        words.contains { word in stems.contains { word.hasPrefix($0) } }
    }

    static func isPath(_ word: String) -> Bool {
        if word.hasPrefix("/") || word.hasPrefix("~/") || word == "~" { return true }
        let bytes = Array(word.utf8)
        guard bytes.count >= 3, let first = bytes.first, (65...90).contains(first) || (97...122).contains(first) else { return false }
        return bytes[1] == UInt8(ascii: ":") && bytes[2] == UInt8(ascii: "\\")
    }

    private static func imperativeWords(_ text: String) -> (lower: [String], cased: [String]) {
        let raw = split(text)
        var lower: [String] = []
        var cased: [String] = []
        for word in raw where !clean(word).isEmpty {
            lower.append(clean(word))
            cased.append(original(word))
        }
        let leads: [[String]] = [
            ["please"], ["can", "you"], ["could", "you"], ["would", "you"], ["will", "you"], ["i", "want", "to"],
            ["i'd", "like", "to"], ["i", "would", "like", "to"], ["help", "me"], ["let's"], ["lets"], ["go", "ahead", "and"],
            ["now"], ["ok"], ["okay"], ["hey"], ["fileid"], ["just"]
        ]
        while let lead = leads.first(where: { starts(lower, $0) }) {
            lower.removeFirst(lead.count)
            cased.removeFirst(lead.count)
        }
        while let last = lower.last, ["please", "now", "thanks"].contains(last) || (lower.count >= 2 && Array(lower.suffix(2)) == ["for", "me"]) {
            let count = last == "me" ? 2 : 1
            lower.removeLast(count)
            cased.removeLast(count)
        }
        return (lower, cased)
    }

    /// A folder reference inside an imperative: a path, a known home folder,
    /// `this`, `library` or `named:<folder>`.
    static func folderReference(_ words: [String], _ cased: [String], _ text: String) -> String? {
        if let quoted = quotedPath(text) { return quoted }
        if let path = cased.first(where: isPath) { return path }
        for (index, word) in words.enumerated() {
            if knownFolders.contains(word) { return word == "docs" ? "documents" : word }
            let next = index + 1 < words.count ? words[index + 1] : nil
            if word == "folder" || word == "directory", !["layout", "structure", "plan", "organization"].contains(next ?? "") {
                let before = index > 0 ? words[index - 1] : nil
                if let before, ["this", "current", "that"].contains(before) { return "this" }
                if let before, !["the", "my", "a", "whole", "entire"].contains(before) { return "named:\(cased[index - 1])" }
                if let next, !["in", "to", "for", "and"].contains(next) { return "named:\(cased[index + 1])" }
            }
        }
        if has(words, ["this", "here"]) { return "this" }
        if words.isEmpty || has(words, ["library", "everything", "files", "all"]) { return "library" }
        return nil
    }

    private static func quotedPath(_ text: String) -> String? {
        let parts = text.replacingOccurrences(of: "“", with: "\"").replacingOccurrences(of: "”", with: "\"")
            .split(separator: "\"", omittingEmptySubsequences: false)
        guard parts.count >= 3 else { return nil }
        let inside = parts[1].trimmingCharacters(in: .whitespacesAndNewlines)
        return isPath(inside) ? inside : nil
    }

    private static func personName(_ cased: ArraySlice<String>, requireCapital: Bool) -> String? {
        var tokens = Array(cased)
        if tokens.first?.lowercased() == "as" { tokens.removeFirst() }
        guard !tokens.isEmpty, tokens.count <= 4 else { return nil }
        let valid = tokens.allSatisfy { token in
            (token.first?.isLetter ?? false) && token.allSatisfy { $0.isLetter || "'-.".contains($0) }
        }
        guard valid, !requireCapital || tokens.allSatisfy({ $0.first?.isUppercase ?? false }) else { return nil }
        return tokens.map { token in
            token.contains(where: \.isUppercase) ? token : (token.first.map { String($0).uppercased() } ?? "") + token.dropFirst()
        }.joined(separator: " ")
    }

    private static func page(for words: ArraySlice<String>) -> String? {
        let filler: Set<String> = ["to", "me", "the", "my", "page", "tab", "view", "screen", "section", "window", "up"]
        switch words.filter({ !filler.contains($0) }) {
        case ["library"], ["files"], ["all", "files"]: return "library"
        case ["people"], ["faces"], ["persons"]: return "people"
        case ["duplicates"], ["cleanup"], ["clean"], ["dupes"]: return "cleanup"
        case ["deep", "analyze"], ["describe", "files"], ["descriptions"], ["describe"]: return "deepAnalyze"
        case ["restructure"], ["organize", "folders"], ["organize"], ["organizer"], ["folders"]: return "restructure"
        case ["settings"], ["preferences"], ["options"]: return "settings"
        case ["search"], ["moments"], ["search", "and", "moments"], ["search", "&", "moments"]: return "search"
        case ["tools"], ["file", "tools"], ["export"]: return "tools"
        case ["chat"]: return "chat"
        default: return nil
        }
    }

    private static func restText(_ words: [String], from: Int) -> String? {
        let rest = from < words.count ? words[from...].joined(separator: " ") : ""
        return rest.isEmpty ? nil : rest
    }

    private static func with(_ kind: String, _ edit: (inout ChatIntent) -> Void) -> ChatIntent {
        var intent = ChatIntent(kind)
        edit(&intent)
        return intent
    }

    /// Parses an imperative request. Nil means a question or search; `refuse`
    /// means a request Chat will not turn into an action.
    static func parse(_ text: String) -> ChatIntent? {
        let (words, cased) = imperativeWords(text)
        guard let verb = words.first, !questionWords.contains(verb) else { return nil }
        let rest = words[1...]
        let restCased = cased[1...]
        let duplicates = has(rest, duplicateWords) || has(rest, ["identical"])

        if ["stop", "cancel", "pause", "halt", "abort", "end"].contains(verb) {
            for word in rest {
                let job: String
                if ["scan", "scanning", "scans", "import", "indexing"].contains(word) { job = "scan" }
                else if ["describ", "analy", "caption", "deep"].contains(where: { word.hasPrefix($0) }) { job = "describe" }
                else if ["organi", "restructur", "reorgani", "move", "moving", "layout"].contains(where: { word.hasPrefix($0) }) { job = "organize" }
                else if ["moment", "moments", "timeline"].contains(word) { job = "moments" }
                else if ["everything", "all", "jobs", "job", "work", "it", "that", "tasks"].contains(word) { job = "all" }
                else { continue }
                return with("stopJob") { $0.job = job }
            }
            return nil
        }
        if ["undo", "revert", "reverse"].contains(verb) {
            let movement = rest.isEmpty || hasPrefix(rest, ["move", "organi", "restructur", "reorgani", "layout"])
                || has(rest, ["last", "that", "it", "folder", "folders"])
            return movement ? with("undoLastMove") { $0.folder = "library" } : nil
        }
        let removal = ["delete", "remove", "trash", "erase", "discard"].contains(verb)
            || (["move", "put", "send", "throw"].contains(verb) && has(rest, ["trash", "bin", "recycle", "garbage"]))
            || starts(words, ["get", "rid"])
            || starts(words, ["clear", "out"])
            || (starts(words, ["clean", "up"]) && duplicates)
        if removal {
            return duplicates || has(rest, ["extra"])
                ? with("moveDuplicatesToTrash") { $0.scope = restText(words, from: 1) }
                : ChatIntent("refuse")
        }
        if ["find", "show", "list", "check", "look", "any", "search", "detect"].contains(verb), duplicates {
            return with("findDuplicates") { $0.scope = restText(words, from: 1) }
        }
        if ["open", "go", "switch", "take", "view", "show"].contains(verb), let page = page(for: rest) {
            return with("openPage") { $0.page = page }
        }
        let suggestion: Set<String> = ["suggested", "suggestion", "suggestions", "smart", "proposed", "ai", "better", "descriptive", "recommended", "new"]
        if (verb == "rename" && has(rest, suggestion))
            || (["apply", "use", "accept"].contains(verb) && has(rest, ["names", "name", "filenames"]) && has(rest, suggestion)) {
            let scope = rest.filter { !suggestion.contains($0) && !["names", "name", "filenames", "to", "the", "using", "with"].contains($0) }
            return with("renameToSuggested") { $0.scope = restText(Array(scope), from: 0) }
        }
        let layout: Set<String> = ["layout", "plan", "organization", "structure", "suggestion", "suggestions", "restructure", "reorganization", "folders"]
        if (["apply", "accept"].contains(verb) || starts(words, ["go", "with"]) || verb == "use"), has(rest, layout) {
            let folder = folderReference(Array(rest), Array(restCased), text).flatMap { $0 == "library" ? nil : $0 }
            return with("applyFolderLayout") { $0.folder = folder ?? "library" }
        }
        let organize = ["organize", "organise", "reorganize", "reorganise", "restructure", "tidy", "arrange"].contains(verb)
            || starts(words, ["sort", "out"])
            || (verb == "clean" && rest.first == "up")
            || (["suggest", "plan", "propose"].contains(verb) && has(rest, layout))
        if organize {
            let skip = rest.first == "up" || rest.first == "out" ? 1 : 0
            let folder = folderReference(Array(words[(1 + skip)...]), Array(cased[(1 + skip)...]), text)
            return with("suggestFolderLayout") { $0.folder = folder ?? "library" }
        }
        if ["name", "call", "label"].contains(verb) {
            let targets: [[String]] = [["this", "person"], ["that", "person"], ["this", "face"], ["this", "one"], ["him"], ["her"], ["them"], ["this"], ["it"]]
            if let target = targets.first(where: { starts(rest, $0) }), let name = personName(cased[(1 + target.count)...], requireCapital: false) {
                return with("namePerson") { $0.name = name }
            }
            return nil
        }
        for lead in [["this", "person", "is"], ["this", "is"], ["that", "is"], ["that's"], ["this's"], ["he", "is"], ["she", "is"], ["he's"], ["she's"]] where starts(words, lead) {
            return personName(cased[lead.count...], requireCapital: true).map { name in with("namePerson") { $0.name = name } }
        }
        if ["merge", "combine", "join"].contains(verb) {
            let noise: Set<String> = ["people", "persons", "faces", "the", "these", "those", "two", "together", "person", "groups", "clusters", "as", "one"]
            let into = rest.contains("into")
            var groups: [[String]] = [[]]
            for (index, word) in rest.enumerated() {
                if ["and", "with", "into", "&"].contains(word) { groups.append([]); continue }
                if !noise.contains(word) {
                    var trimmed = Substring(cased[1 + index])
                    while trimmed.last == "," { trimmed = trimmed.dropLast() }
                    groups[groups.count - 1].append(String(trimmed))
                }
            }
            var people = groups.filter { !$0.isEmpty }.map { $0.joined(separator: " ") }
            if into, people.count >= 2, let destination = people.popLast() { people.insert(destination, at: 0) }
            return (people.isEmpty && has(rest, ["these", "those", "them"])) || people.count >= 2
                ? with("mergePeople") { $0.people = people } : nil
        }
        if let same = (0..<max(0, words.count - 2)).first(where: { Array(words[$0..<($0 + 3)]) == ["the", "same", "person"] }),
           same >= 2, ["are", "is"].contains(words[same - 1]) {
            let names = cased[..<(same - 1)]
            let lower = words[..<(same - 1)]
            if let split = lower.firstIndex(of: "and") {
                let people = [names[..<split].joined(separator: " "), names[(split + 1)...].joined(separator: " ")]
                if people.allSatisfy({ !$0.isEmpty }) { return with("mergePeople") { $0.people = people } }
            }
        }
        let libraryWords: Set<String> = ["the", "my", "library", "files", "scan", "index", "catalog", "everything", "again", "all"]
        if ["rescan", "re-scan", "reindex", "re-index", "refresh", "update"].contains(verb), rest.allSatisfy({ libraryWords.contains($0) }) {
            return with("rescan") { $0.folder = "library" }
        }
        if starts(words, ["check", "for", "new", "files"]) { return with("rescan") { $0.folder = "library" } }
        if ["scan", "index", "add", "import"].contains(verb) {
            if rest.allSatisfy({ libraryWords.contains($0) }) {
                return verb == "scan" || verb == "index" ? with("rescan") { $0.folder = "library" } : nil
            }
            guard let folder = folderReference(Array(rest), Array(restCased), text), folder != "library" else { return nil }
            return with("scanFolder") { $0.folder = folder }
        }
        if ["describe", "caption", "analyze", "analyse"].contains(verb) || (verb == "write" && has(rest, ["descriptions", "captions"])) {
            let skip = rest.prefix { ["descriptions", "captions", "for", "of"].contains($0) }.count
            return with("describeFiles") { $0.scope = restText(words, from: 1 + skip) }
        }
        if has(words, ["moment", "moments"]), ["find", "show", "search", "look", "get"].contains(verb) {
            let noise: Set<String> = ["the", "moment", "moments", "when", "where", "of", "with", "in", "my", "for", "that", "a", "an", "videos", "video", "me"]
            let query = zip(restCased, rest).filter { !noise.contains($0.1) }.map(\.0)
            return with("findMoments") { $0.query = query.joined(separator: " ") }
        }
        if ["export", "convert", "resize"].contains(verb) || starts(words, ["make", "copies"]) || starts(words, ["save", "copies"]) {
            let skip = verb == "make" || verb == "save" ? 2 + (words.count > 2 && words[2] == "of" ? 1 : 0) : 1
            return with("exportFiles") { $0.scope = restText(words, from: skip) }
        }
        let toggle = ["turn", "switch"].contains(verb) && has(rest, ["on", "off"])
        if toggle || ["enable", "disable"].contains(verb) || (verb == "set" && has(rest, ["on", "off", "true", "false"])) {
            let value: Bool
            switch verb {
            case "enable": value = true
            case "disable": value = false
            default:
                guard let found = rest.reversed().lazy.compactMap({ (word: String) -> Bool? in
                    word == "on" || word == "true" ? true : word == "off" || word == "false" ? false : nil
                }).first else { return nil }
                value = found
            }
            let joined = rest.joined(separator: " ")
            let setting: String
            if joined.contains("tag"), hasPrefix(rest, ["kept", "keeper", "cleanup", "duplicate", "copies", "copy"]) { setting = "cleanupAutoTag" }
            else if hasPrefix(rest, ["background", "lava", "animation", "motion"]) { setting = "backgroundMotion" }
            else if hasPrefix(rest, ["summar"]) || joined.contains("local ai") || joined.contains("written answers") { setting = "chatSummaries" }
            else { return ChatIntent("refuse") }
            return with("changeSetting") { $0.setting = setting; $0.value = value }
        }
        if verb == "search" {
            let skip = rest.prefix { ["for", "the", "my", "library", "files", "in", "through"].contains($0) }.count
            let query = 1 + skip < cased.count ? cased[(1 + skip)...].joined(separator: " ") : ""
            return query.isEmpty ? nil : with("search") { $0.query = query }
        }
        return nil
    }

    // MARK: - Questions

    /// Parses the structured part of a question (counts, sizes, ordering,
    /// dates, folders, duplicates, descriptions).
    static func question(_ text: String) -> Question {
        let raw = split(text)
        let words = raw.map { clean($0).replacingOccurrences(of: "'", with: "") }
        var question = Question()
        var used = Set<Int>()
        for index in words.indices.dropLast() {
            let pair = (words[index], words[index + 1])
            if pair == ("how", "many") || pair == ("number", "of") {
                question.measure = "count"; used.formUnion([index, index + 1])
            }
            if (pair.0 == "how" && ["much", "big", "large"].contains(pair.1)) || pair == ("total", "size") {
                question.measure = "size"; used.formUnion([index, index + 1])
            }
        }
        if words.first == "count" { question.measure = "count"; used.insert(0) }
        if question.measure == "list", has(words, ["space", "storage", "room"]), has(words, ["use", "uses", "using", "used", "take", "takes", "taking"]) {
            question.measure = "size"
        }
        for (index, word) in words.enumerated() {
            let order: String
            switch word {
            case "largest", "biggest", "heaviest": order = "largest"
            case "smallest", "tiniest": order = "smallest"
            case "newest", "latest", "recent": order = "newest"
            case "oldest", "earliest": order = "oldest"
            default: continue
            }
            question.order = order; question.explicitOrder = true; used.insert(index)
        }
        if has(words, duplicateWords) || has(words, ["identical"]) {
            question.duplicates = true
            for (index, word) in words.enumerated() where duplicateWords.contains(word) || ["identical", "extra"].contains(word) { used.insert(index) }
        }
        let described: Set<String> = ["described", "description", "descriptions", "analyzed", "captioned", "caption", "captions"]
        if has(words, ["undescribed", "unanalyzed"])
            || (has(words, ["not", "no", "without", "missing", "havent", "hasnt", "need", "needs", "lacking"]) && has(words, described)) {
            question.undescribed = true
            let consumed: Set<String> = ["undescribed", "unanalyzed", "not", "no", "without", "missing", "havent", "hasnt", "need", "needs", "lacking", "yet", "been"]
            for (index, word) in words.enumerated() where consumed.contains(word) || described.contains(word) { used.insert(index) }
        }
        if let (filter, range) = dateFilter(words) { question.date = filter; used.formUnion(range) }
        if let (folder, range) = questionFolder(words, raw) { question.folder = folder; used.formUnion(range) }
        if question.structured {
            let filler: Set<String> = [
                "how", "many", "much", "what", "which", "is", "are", "there", "do", "does", "did", "i", "we", "you", "have", "has", "got",
                "total", "number", "count", "use", "uses", "using", "used", "take", "takes", "taking", "up", "space", "storage", "room", "disk",
                "size", "big", "large", "show", "list", "give", "tell", "me", "top", "been", "yet", "were", "was", "taken", "made", "created",
                "shot", "recorded", "added", "any", "that", "those", "these", "by", "per", "most", "least", "first", "sorted", "files", "file"
            ]
            for (index, word) in words.enumerated() where filler.contains(word) { used.insert(index) }
            question.reduced = raw.enumerated().filter { !used.contains($0.offset) }.map(\.element).joined(separator: " ")
        } else {
            question.reduced = text
        }
        return question
    }

    private static func capitalizedFirst(_ text: String) -> String {
        (text.first.map { String($0).uppercased() } ?? "") + text.dropFirst()
    }

    private static func questionFolder(_ words: [String], _ raw: [String]) -> (String, Range<Int>)? {
        for (index, word) in words.enumerated() where ["in", "inside", "under", "from"].contains(word) {
            var at = index + 1
            while at < words.count, ["my", "the"].contains(words[at]) { at += 1 }
            guard at < words.count else { continue }
            let target = words[at]
            let cased = original(raw[at])
            if isPath(cased) { return (cased, index..<(at + 1)) }
            let folderAfter = at + 1 < words.count && (words[at + 1] == "folder" || words[at + 1] == "directory")
            if ["downloads", "desktop", "pictures", "movies", "music"].contains(target) || (folderAfter && target != "folder") {
                return ("named:\(capitalizedFirst(cased))", index..<(folderAfter ? at + 2 : at + 1))
            }
            if target == "folder" || target == "directory", at + 1 < raw.count {
                return ("named:\(original(raw[at + 1]))", index..<(at + 2))
            }
        }
        return nil
    }

    private static let months: [String: Int] = [
        "january": 1, "jan": 1, "february": 2, "feb": 2, "march": 3, "mar": 3, "april": 4, "apr": 4, "may": 5,
        "june": 6, "jun": 6, "july": 7, "jul": 7, "august": 8, "aug": 8, "september": 9, "sept": 9, "sep": 9,
        "october": 10, "oct": 10, "november": 11, "nov": 11, "december": 12, "dec": 12
    ]
    private static let monthNames = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]

    private static func month(_ word: String) -> Int? { months[word] }

    private static func isDigits(_ text: Substring) -> Bool { !text.isEmpty && text.allSatisfy { $0.isASCII && $0.isNumber } }

    private static func day(_ word: String) -> Int? {
        var digits = Substring(word)
        while let last = digits.last, last.isASCII, last.isLetter { digits = digits.dropLast() }
        let suffix = word.dropFirst(digits.count)
        guard isDigits(digits), digits.count <= 2, ["", "st", "nd", "rd", "th"].contains(suffix), let value = Int(digits), (1...31).contains(value) else { return nil }
        return value
    }

    private static func year(_ word: Substring) -> Int? {
        guard word.count == 4, isDigits(word), let value = Int(word), (1900...2100).contains(value) else { return nil }
        return value
    }
    private static func year(_ word: String) -> Int? { year(Substring(word)) }

    private struct Span { var filter: DateFilter; var length: Int }

    private static func exact(_ from: [String], _ to: [String], _ label: String) -> DateFilter {
        DateFilter(from: from, to: to, phrase: label)
    }

    private static func calendar(_ y: Int, _ m: Int, _ d: Int) -> String? {
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
        let days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        guard d <= days[m - 1] else { return nil }
        return String(format: "%04d-%02d-%02d", y, m, d)
    }

    private static func single(_ y: Int, _ m: Int, _ d: Int, length: Int) -> Span? {
        guard let date = calendar(y, m, d) else { return nil }
        return Span(filter: exact([date], [date, "+1 day"], "\(monthNames[m - 1]) \(d), \(y)"), length: length)
    }

    private static func dateAt(_ words: [String], _ index: Int) -> Span? {
        let word = words[index]
        let next = index + 1 < words.count ? words[index + 1] : ""
        switch word {
        case "today": return Span(filter: exact(["now", "localtime", "start of day"], ["now", "localtime", "start of day", "+1 day"], "today"), length: 1)
        case "yesterday": return Span(filter: exact(["now", "localtime", "start of day", "-1 day"], ["now", "localtime", "start of day"], "yesterday"), length: 1)
        case "this", "last", "past":
            guard ["week", "month", "year"].contains(next) else { break }
            let start: [String] = switch next {
            case "week": ["now", "localtime", "start of day", "-6 days", "weekday 1"]
            case "month": ["now", "localtime", "start of month"]
            default: ["now", "localtime", "start of year"]
            }
            let step = next == "week" ? "+7 days" : "+1 \(next)"
            let back = next == "week" ? "-7 days" : "-1 \(next)"
            let label = "\(word) \(next)"
            let filter: DateFilter = switch word {
            case "this": exact(start, start + [step], label)
            case "last": exact(start + [back], start, label)
            default: DateFilter(from: ["now", back], phrase: "the past \(next)")
            }
            return Span(filter: filter, length: 2)
        default: break
        }
        let dashes = word.split(separator: "-", omittingEmptySubsequences: false)
        if dashes.count == 3, dashes[0].count == 4, let y = year(dashes[0]), let m = Int(dashes[1]), let d = Int(dashes[2]),
           dashes[1].count == 2, dashes[2].count == 2, (1...12).contains(m) {
            return single(y, m, d, length: 1)
        }
        if dashes.count == 2, dashes[0].count == 4, dashes[1].count == 2, let y = year(dashes[0]), let m = Int(dashes[1]), (1...12).contains(m) {
            return monthYear(y, m, length: 1)
        }
        let slashes = word.split(separator: "/", omittingEmptySubsequences: false)
        if (2...3).contains(slashes.count), slashes.allSatisfy(isDigits) {
            guard let m = Int(slashes[0]), let d = Int(slashes[1]) else { return nil }
            if (1...12).contains(m), (1...31).contains(d), slashes[0].count <= 2, slashes[1].count <= 2 {
                if slashes.count == 3 {
                    let y: Int
                    if slashes[2].count == 2 { guard let short = Int(slashes[2]) else { return nil }; y = 2000 + short }
                    else { guard let full = year(slashes[2]) else { return nil }; y = full }
                    return single(y, m, d, length: 1)
                }
                return monthDay(m, d, length: 1)
            }
        }
        if let m = month(word) {
            if let d = day(next) {
                if index + 2 < words.count, let y = year(words[index + 2]) { return single(y, m, d, length: 3) }
                return monthDay(m, d, length: 2)
            }
            if let y = year(next) { return monthYear(y, m, length: 2) }
            return Span(filter: DateFilter(month: String(format: "%02d", m), phrase: "\(monthNames[m - 1]) (any year)"), length: 1)
        }
        if let d = day(word) {
            let of = next == "of" ? 1 : 0
            if index + 1 + of < words.count, let m = month(words[index + 1 + of]) {
                if index + 2 + of < words.count, let y = year(words[index + 2 + of]) { return single(y, m, d, length: 3 + of) }
                return monthDay(m, d, length: 2 + of)
            }
        }
        if let y = year(word) {
            let start = String(format: "%04d-01-01", y)
            return Span(filter: exact([start], [start, "+1 year"], "\(y)"), length: 1)
        }
        return nil
    }

    private static func monthYear(_ y: Int, _ m: Int, length: Int) -> Span {
        let start = String(format: "%04d-%02d-01", y, m)
        return Span(filter: exact([start], [start, "+1 month"], "\(monthNames[m - 1]) \(y)"), length: length)
    }

    private static func monthDay(_ m: Int, _ d: Int, length: Int) -> Span {
        Span(filter: DateFilter(monthDay: String(format: "%02d-%02d", m, d), phrase: "\(monthNames[m - 1]) \(d) (any year)"), length: length)
    }

    /// The first date expression and the words it consumed, including its
    /// preposition. A bare year or month name needs a preposition before it.
    private static func dateFilter(_ words: [String]) -> (DateFilter, Range<Int>)? {
        let prepositions: Set<String> = ["from", "in", "on", "during", "since", "before", "after", "of", "between", "taken", "around"]
        for index in words.indices {
            guard let span = dateAt(words, index) else { continue }
            let previous = index > 0 ? words[index - 1] : ""
            let bare = (month(words[index]) != nil && span.length == 1) || (year(words[index]) != nil && span.length == 1)
            if bare && !prepositions.contains(previous) { continue }
            let start = prepositions.contains(previous) ? index - 1 : index
            let end = index + span.length
            let absolute = span.filter.monthDay == nil && span.filter.month == nil
            var filter = span.filter
            if previous == "since", absolute {
                filter = DateFilter(from: span.filter.from, phrase: "since \(span.filter.phrase)")
            } else if previous == "after", absolute {
                filter = DateFilter(from: span.filter.to, phrase: "after \(span.filter.phrase)")
            } else if previous == "before", absolute {
                filter = DateFilter(to: span.filter.from, phrase: "before \(span.filter.phrase)")
            } else if previous == "between", absolute, end < words.count, words[end] == "and" {
                if end + 1 < words.count, let second = dateAt(words, end + 1), second.filter.monthDay == nil, second.filter.month == nil {
                    let range = DateFilter(from: span.filter.from, to: second.filter.to, phrase: "between \(span.filter.phrase) and \(second.filter.phrase)")
                    return (range, start..<(end + 1 + second.length))
                }
                filter.phrase = "from \(span.filter.phrase)"
            } else {
                filter.phrase = "from \(span.filter.phrase)"
            }
            return (filter, start..<end)
        }
        return nil
    }

    // MARK: - Labels

    /// "1.2 GB" with decimal units and round-half-up tenths, identical in both engines.
    static func bytesLabel(_ value: Int64) -> String {
        let bytes = max(0, value)
        if bytes < 1000 { return bytes == 1 ? "1 byte" : "\(bytes) bytes" }
        let units = ["KB", "MB", "GB", "TB", "PB"]
        var unit: Int64 = 1000
        var index = 0
        while index + 1 < units.count && bytes >= unit * 1000 { unit *= 1000; index += 1 }
        var step = unit / 10
        var tenths = (bytes + step / 2) / step
        if tenths >= 10_000 && index + 1 < units.count {
            index += 1
            step *= 1000
            tenths = (bytes + step / 2) / step
        }
        return "\(tenths / 10).\(tenths % 10) \(units[index])"
    }

    /// "15,021".
    static func countLabel(_ value: Int64) -> String {
        let digits = String(value.magnitude)
        var grouped = ""
        for (index, digit) in digits.enumerated() {
            if index > 0 && (digits.count - index) % 3 == 0 { grouped.append(",") }
            grouped.append(digit)
        }
        return value < 0 ? "-" + grouped : grouped
    }
    static func countLabel(_ value: Int) -> String { countLabel(Int64(value)) }

    /// Plural or singular noun for a set of kinds.
    static func noun(_ kinds: [String], _ count: Int64) -> String {
        let one = count == 1
        func label(_ kind: String) -> String {
            switch kind {
            case "image": one ? "photo" : "photos"
            case "video": one ? "video" : "videos"
            case "doc": one ? "document" : "documents"
            case "pdf": one ? "PDF" : "PDFs"
            case "audio": one ? "audio file" : "audio files"
            case "model": one ? "3D model" : "3D models"
            case "other": one ? "other file" : "other files"
            default: one ? "file" : "files"
            }
        }
        let unique = Array(Set(kinds)).sorted()
        if unique == ["doc", "pdf"] { return one ? "document" : "documents" }
        let labels = unique.map(label)
        switch labels.count {
        case 0: return label("")
        case 1: return labels[0]
        default: return labels.dropLast().joined(separator: ", ") + " and " + labels[labels.count - 1]
        }
    }
}
