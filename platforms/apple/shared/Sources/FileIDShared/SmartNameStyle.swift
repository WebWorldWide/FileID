import Foundation

/// Saved smart-name styles built only from stored catalog evidence. Lockstep
/// with Rust `util/smart_name_style.rs`; both run
/// `shared/test-corpus/naming/style-cases.json`. Text is compared per Unicode
/// scalar, never by Swift's canonical equivalence, so both engines produce
/// byte-identical names.
public enum SmartNameStyle {
    public enum Style: String, Sendable { case short, descriptive, dateFirst
        var coreLimit: Int { self == .descriptive ? 6 : 3 }
    }

    public struct Options: Sendable {
        public var style: Style
        public var maxLength: Int
        public var people: Bool
        public var event: Bool
        public var date: Bool
        public init(style: Style, maxLength: Int, people: Bool = false, event: Bool = false, date: Bool = false) {
            self.style = style; self.maxLength = maxLength; self.people = people; self.event = event; self.date = date
        }
    }

    public typealias Key = [Unicode.Scalar]

    public struct Evidence: Sendable {
        public var originalStem: String
        public var chapter: String?
        public var storedName: String?
        public var description: String?
        public var tags: [String]
        public var personWords: Set<Key>
        public var people: [String]
        public var event: String?
        public var date: String?
        public init(originalStem: String, chapter: String? = nil, storedName: String? = nil, description: String? = nil, tags: [String] = [],
                    personWords: Set<Key> = [], people: [String] = [], event: String? = nil, date: String? = nil) {
            self.originalStem = originalStem; self.chapter = chapter; self.storedName = storedName; self.description = description
            self.tags = tags; self.personWords = personWords; self.people = people; self.event = event; self.date = date
        }
    }

    public struct Outcome: Sendable, Equatable {
        public let stem: String?
        public let reason: String
        public var source: String
        public init(stem: String?, reason: String, source: String) { self.stem = stem; self.reason = reason; self.source = source }
    }

    public static let minLength = 4
    public static let maxLength = 120
    static let maxStemBytes = 200
    static let maxEventWords = 4
    static let maxTags = 3

    static let stop: Set<String> = [
        "a", "an", "the", "of", "in", "on", "at", "to", "for", "with", "and", "or", "but", "from", "by",
        "into", "onto", "over", "under", "near", "while", "during", "after", "before", "about", "above",
        "below", "between", "through", "across", "around", "along", "against", "its", "his", "her",
        "their", "our", "my", "your", "this", "that", "these", "those", "it", "as", "some", "there",
        "here", "who", "which", "what", "whose", "is", "are", "was", "were", "be", "being", "been",
        "has", "have", "had", "does", "do", "did", "can", "will", "very", "also", "just", "photo",
        "photograph", "photos", "picture", "pictures", "image", "images", "video", "videos", "clip",
        "file", "files", "document", "documents", "untitled", "filename", "screenshot", "shows",
        "show", "showing", "shown", "depicts", "depicting", "depicted", "displays", "displaying",
        "displayed", "features", "featuring", "captured", "taken", "scene", "view", "close",
        "closeup", "up", "front", "background", "foreground", "side", "top", "bottom", "center",
        "middle", "person", "people", "someone", "something",
    ]
    static let labelGeneric: Set<String> = ["untitled", "chapter", "moment", "marker", "event", "file", "photo", "image", "video", "document"]
    static let reserved: Set<String> = Set(["con", "prn", "aux", "nul"] + (0...9).flatMap { ["com\($0)", "lpt\($0)"] })

    public static func key(_ text: String) -> Key { Array(text.unicodeScalars) }

    public static func lower(_ text: String) -> String {
        var view = String.UnicodeScalarView()
        for scalar in text.unicodeScalars { view.append(contentsOf: scalar.properties.lowercaseMapping.unicodeScalars) }
        return String(view)
    }

    static func isNumeric(_ s: Unicode.Scalar) -> Bool {
        switch s.properties.generalCategory {
        case .decimalNumber, .letterNumber, .otherNumber: return true
        default: return false
        }
    }

    static func isMark(_ s: Unicode.Scalar) -> Bool {
        switch s.value {
        case 0x0300...0x036F, 0x1AB0...0x1AFF, 0x1DC0...0x1DFF, 0x20D0...0x20FF, 0xFE20...0xFE2F: return true
        default: return false
        }
    }

    static func isWordScalar(_ s: Unicode.Scalar) -> Bool { s.properties.isAlphabetic || isNumeric(s) || isMark(s) }

    static func split(_ text: String, where separator: (Unicode.Scalar) -> Bool) -> [[Unicode.Scalar]] {
        var parts: [[Unicode.Scalar]] = [[]]
        for scalar in text.unicodeScalars {
            if separator(scalar) { parts.append([]) } else { parts[parts.count - 1].append(scalar) }
        }
        return parts
    }

    static func trimmed(_ text: String) -> String {
        var scalars = Array(text.unicodeScalars)
        while let first = scalars.first, first.properties.isWhitespace { scalars.removeFirst() }
        while let last = scalars.last, last.properties.isWhitespace { scalars.removeLast() }
        return string(scalars)
    }

    static func string(_ scalars: [Unicode.Scalar]) -> String {
        var view = String.UnicodeScalarView()
        view.append(contentsOf: scalars)
        return String(view)
    }

    public static func rawWords(_ text: String) -> [String] {
        let apostrophe: Unicode.Scalar = "'"
        let lowered = lower(text).unicodeScalars.map { $0 == "\u{2019}" ? apostrophe : $0 }
        return split(string(lowered)) { !(isWordScalar($0) || $0 == apostrophe) }.compactMap { token -> String? in
            var token = token
            while token.first == apostrophe { token.removeFirst() }
            while token.last == apostrophe { token.removeLast() }
            if token.count >= 2, token[token.count - 2] == apostrophe, token[token.count - 1] == "s" { token.removeLast(2) }
            token.removeAll { $0 == apostrophe }
            return token.isEmpty ? nil : string(token)
        }
    }

    public static func words(_ text: String, personWords: Set<Key>) -> [String] {
        var seen = Set<Key>()
        return rawWords(text).filter { word in
            let scalars = key(word)
            let singleASCII = scalars.count == 1 && scalars[0].isASCII
            return !singleASCII && !stop.contains(word) && !scalars.allSatisfy(isNumeric)
                && !personWords.contains(scalars) && seen.insert(scalars).inserted
        }
    }

    public static func addPersonWords(_ target: inout Set<Key>, _ part: String) {
        for word in rawWords(part) where word.unicodeScalars.count >= 2 { target.insert(key(word)) }
    }

    public static func personDisplay(title: String?, first: String?, legacy: String?) -> String {
        let title = trimmed(title ?? ""), first = trimmed(first ?? "")
        if !title.isEmpty && !first.isEmpty { return title + " " + first }
        if !first.isEmpty { return first }
        if !title.isEmpty { return title }
        return trimmed(legacy ?? "")
    }

    public static func isUserNamed(_ display: String) -> Bool {
        let name = key(lower(trimmed(display)))
        if name.isEmpty { return false }
        var rest: ArraySlice<Unicode.Scalar>
        if name.starts(with: "person".unicodeScalars) { rest = name.dropFirst(6) }
        else if name.starts(with: "unknown".unicodeScalars) { rest = name.dropFirst(7) }
        else { return true }
        if rest.isEmpty { return false }
        while let first = rest.first, first == " " || first == "_" || first == "-" { rest.removeFirst() }
        return rest.isEmpty || !rest.allSatisfy { $0.isASCII && ("0"..."9").contains($0) }
    }

    public static func personToken(first: String?, legacy: String?) -> String? {
        guard let source = [first, legacy].compactMap({ $0.map(trimmed) }).first(where: { !$0.isEmpty }),
              let word = split(source, where: { $0.properties.isWhitespace }).first(where: { !$0.isEmpty }) else { return nil }
        let token = lower(string(word)).unicodeScalars.filter(isWordScalar)
        return token.count >= 2 ? string(Array(token)) : nil
    }

    public static func singleLabel(_ titles: [String]) -> String? {
        var found: (Key, String)?
        for title in titles {
            let text = split(title) { $0.properties.isWhitespace }.filter { !$0.isEmpty }.map(string).joined(separator: " ")
            let tokens = rawWords(text)
            let usable = !text.isEmpty && text.unicodeScalars.count <= 60 && text.utf8.count <= 200 && tokens.count <= 8
                && tokens.contains { $0.unicodeScalars.contains { $0.properties.isAlphabetic } && !labelGeneric.contains($0) }
            guard usable else { continue }
            let k = key(lower(text))
            if let found { if found.0 != k { return nil } } else { found = (k, text) }
        }
        return found?.1
    }

    public static func splitName(_ name: String) -> (stem: String, ext: String) {
        let scalars = Array(name.unicodeScalars)
        if let dot = scalars.lastIndex(of: "."), dot > 0, dot + 1 < scalars.count {
            return (string(Array(scalars[..<dot])), string(Array(scalars[(dot + 1)...])))
        }
        return (name, "")
    }

    static func firstSentence(_ text: String) -> String {
        string(split(text) { $0 == "." || $0 == "!" || $0 == "?" || $0 == "\n" }[0])
    }

    struct Pick { var people: [Bool]; var core: Int; var date = false; var event = false }

    public static func propose(_ evidence: Evidence, _ options: Options) -> Outcome {
        let pw = evidence.personWords
        let chapter = evidence.chapter.map { words($0, personWords: pw) } ?? []
        let stored = evidence.storedName.map { words($0, personWords: pw) } ?? []
        let description = evidence.description.map { words(firstSentence($0), personWords: pw) } ?? []
        let tags = words(evidence.tags.prefix(maxTags).joined(separator: " "), personWords: pw)
        var sources: [([String], String)] = [(chapter, "chapter")]
        sources += options.style == .descriptive
            ? [(description, "description"), (stored, "proposedName")]
            : [(stored, "proposedName"), (description, "description")]
        sources.append((tags, "tags"))
        var event = options.event ? Array((evidence.event.map { words($0, personWords: pw) } ?? []).prefix(maxEventWords)) : []
        var (core, reason) = sources.first { !$0.0.isEmpty } ?? ([], "noEvidence")
        if core.isEmpty {
            guard !event.isEmpty else { return Outcome(stem: nil, reason: "noEvidence", source: "noEvidence") }
            core = event; event = []; reason = "event"
        }
        core = Array(core.prefix(options.style.coreLimit))
        var people: [String] = []
        if options.people {
            for token in evidence.people.sorted(by: { $0.unicodeScalars.lexicographicallyPrecedes($1.unicodeScalars) })
            where people.last.map(key) != key(token) { people.append(token) }
        }
        if people.count > 2 { people = [] }
        let date = options.style == .dateFirst || options.date ? evidence.date : nil

        func render(_ pick: Pick) -> String {
            var seen = Set<Key>()
            let chosen = zip(people, pick.people).filter(\.1).map(\.0) + (pick.event ? event : []) + core.prefix(pick.core)
            let body = chosen.filter { seen.insert(key($0)).inserted }.joined(separator: "-")
            guard let date, pick.date else { return body }
            return options.style == .dateFirst ? date + " " + body : body + "-" + date
        }
        func fits(_ pick: Pick) -> Bool {
            let text = render(pick)
            return text.unicodeScalars.count <= options.maxLength && text.utf8.count <= maxStemBytes
        }
        var pick = Pick(people: Array(repeating: false, count: people.count), core: 1)
        guard fits(pick) else { return Outcome(stem: nil, reason: "budget", source: reason) }
        func attempt(_ change: (inout Pick) -> Void) -> Bool {
            var next = pick
            change(&next)
            guard fits(next) else { return false }
            pick = next
            return true
        }
        for index in people.indices { _ = attempt { $0.people[index] = true } }
        var coreOpen = core.count < 2 || attempt { $0.core = 2 }
        if date != nil { _ = attempt { $0.date = true } }
        if !event.isEmpty { _ = attempt { $0.event = true } }
        while coreOpen && pick.core < core.count {
            let next = pick.core + 1
            coreOpen = attempt { $0.core = next }
        }
        let stem = render(pick)
        if reserved.contains(stem) { return Outcome(stem: nil, reason: "reserved", source: reason) }
        if key(lower(stem)) == key(lower(evidence.originalStem)) { return Outcome(stem: nil, reason: "unchanged", source: reason) }
        return Outcome(stem: stem, reason: reason, source: reason)
    }
}
