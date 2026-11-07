import Foundation

/// Local learning of smart-name style corrections. Each sample pairs the name
/// the engine would propose without learning (`base`) with the name the user
/// applied; preferences are what a clear majority of the newest samples show.
/// Lockstep with Rust `util/smart_name_learning.rs`; both run
/// `shared/test-corpus/naming/learning-cases.json`.
public enum SmartNameLearning {
    typealias S = SmartNameStyle

    public static let window = 50
    public static let keep = 200
    public static let keys = ["dropWords", "people", "words", "leadingDate", "trailingDate", "dateFormat", "separator", "case"]
    static let minVotes = 3
    static let minDrops = 2
    static let maxWordCap = 5
    static let separators: [Unicode.Scalar] = [" ", "-", "_"]
    static let trim: Set<Unicode.Scalar> = [" ", "-", "_", "."]

    public struct Learned: Sendable, Equatable {
        public var separator: String?
        public var letterCase: String?
        public var leadingDate: String?
        public var trailingDate: String?
        public var dateFormat: String?
        public var omitPeople = false
        public var maxWords: Int?
        public var dropWords: [String] = []
        public init() {}

        public var activeKeys: [String] { keys.filter { !values($0).isEmpty } }

        public func without(_ key: String) -> Learned {
            var copy = self
            switch key {
            case "dropWords": copy.dropWords = []
            case "people": copy.omitPeople = false
            case "words": copy.maxWords = nil
            case "leadingDate": copy.leadingDate = nil
            case "trailingDate": copy.trailingDate = nil
            case "dateFormat": copy.dateFormat = nil
            case "separator": copy.separator = nil
            case "case": copy.letterCase = nil
            default: break
            }
            return copy
        }

        public func values(_ key: String) -> [String] {
            switch key {
            case "dropWords": return dropWords
            case "people": return omitPeople ? ["omit"] : []
            case "words": return maxWords.map { [String($0)] } ?? []
            case "leadingDate": return leadingDate.map { [$0] } ?? []
            case "trailingDate": return trailingDate.map { [$0] } ?? []
            case "dateFormat": return dateFormat.map { [$0] } ?? []
            case "separator": return separator.map { [$0] } ?? []
            case "case": return letterCase.map { [$0] } ?? []
            default: return []
            }
        }
    }

    public struct Sample: Sendable, Equatable {
        public var base: String
        public var applied: String
        public var people: [String]
        public init(base: String, applied: String, people: [String] = []) {
            self.base = base; self.applied = applied; self.people = people
        }
    }

    struct Observation {
        var leading: String?
        var trailing: String?
        var dateFormat: String?
        var separator: Unicode.Scalar?
        var letterCase: String?
        var peopleKept: Bool?
        var words: [(String, Bool)] = []
        var length: (count: Int, available: Int)?
    }

    static func isDigit(_ s: Unicode.Scalar) -> Bool { ("0"..."9").contains(s) }

    static func digit(_ text: [Unicode.Scalar], _ index: Int) -> Bool { index >= 0 && index < text.count && isDigit(text[index]) }

    static func stripBump(_ stem: [Unicode.Scalar]) -> [Unicode.Scalar] {
        var digits = 0
        while digits < stem.count, isDigit(stem[stem.count - 1 - digits]) { digits += 1 }
        guard (1...2).contains(digits), stem.count > digits + 1, stem[stem.count - digits - 1] == "_",
              let value = Int(S.string(Array(stem.suffix(digits)))), (2...99).contains(value) else { return stem }
        return Array(stem.prefix(stem.count - digits - 1))
    }

    static func find(_ text: [Unicode.Scalar], _ pattern: [Unicode.Scalar], month: Bool) -> Int? {
        guard pattern.count <= text.count else { return nil }
        for start in 0...(text.count - pattern.count) {
            let end = start + pattern.count
            guard Array(text[start..<end]) == pattern, !digit(text, start - 1), !digit(text, end) else { continue }
            if month, end < text.count, ["-", "_", "."].contains(text[end]), digit(text, end + 1) { continue }
            return start
        }
        return nil
    }

    static func isoDate(_ text: [Unicode.Scalar]) -> Int? {
        guard text.count >= 10 else { return nil }
        for start in 0...(text.count - 10) {
            let shaped = (0..<10).allSatisfy { offset in offset == 4 || offset == 7 ? text[start + offset] == "-" : isDigit(text[start + offset]) }
            if shaped, !digit(text, start - 1), !digit(text, start + 10) { return start }
        }
        return nil
    }

    static func body(_ text: [Unicode.Scalar], cut: Range<Int>?) -> String {
        var scalars = text
        if let cut { scalars.removeSubrange(cut) }
        while let first = scalars.first, trim.contains(first) { scalars.removeFirst() }
        while let last = scalars.last, trim.contains(last) { scalars.removeLast() }
        return S.string(scalars)
    }

    static func letterCase(_ text: String) -> String? {
        var found: String?
        for part in S.split(text, where: { !(S.isWordScalar($0) || $0 == "'") }) {
            let token = S.string(part.filter { $0 != "'" })
            let lowered = S.key(S.lower(token)), raised = S.key(S.upper(token)), own = S.key(token)
            if lowered == own && raised == own { continue }
            let scalars = Array(token.unicodeScalars)
            let first = S.string(Array(scalars.prefix(1))), rest = S.string(Array(scalars.dropFirst()))
            let firstUpper = S.key(S.upper(first)) == S.key(first) && S.key(S.lower(first)) != S.key(first)
            let className: String
            if lowered == own { className = "lower" }
            else if firstUpper && S.key(S.lower(rest)) == S.key(rest) { className = "title" }
            else if raised == own { className = "upper" }
            else { return nil }
            if let found, found != className { return nil }
            found = className
        }
        return found
    }

    static func numeric(_ word: String) -> Bool { word.unicodeScalars.allSatisfy(S.isNumeric) }

    static func observe(_ sample: Sample) -> Observation {
        var seen = Observation()
        let base = Array(sample.base.unicodeScalars)
        let applied = stripBump(Array(S.splitName(sample.applied).stem.unicodeScalars))
        let baseDate = isoDate(base).flatMap { $0 == 0 || $0 + 10 == base.count ? $0 : nil }
        var appliedCut: Range<Int>?
        if let start = baseDate {
            let date = Array(base[start..<(start + 10)])
            let year = S.string(Array(date[0..<4])), month = S.string(Array(date[5..<7])), day = S.string(Array(date[8..<10]))
            let formats: [(String, String, Bool)] = [
                ("iso", "\(year)-\(month)-\(day)", false), ("underscore", "\(year)_\(month)_\(day)", false),
                ("dotted", "\(year).\(month).\(day)", false), ("compact", "\(year)\(month)\(day)", false),
                ("month", "\(year)-\(month)", true),
            ]
            var found: (name: String, range: Range<Int>)?
            for (name, pattern, isMonth) in formats {
                let scalars = Array(pattern.unicodeScalars)
                if let at = find(applied, scalars, month: isMonth) { found = (name, at..<(at + scalars.count)); break }
            }
            var position: String?
            if let found {
                position = found.range.lowerBound == 0 ? "first" : found.range.upperBound == applied.count ? "last" : "middle"
                seen.dateFormat = found.name
                appliedCut = found.range
            } else {
                let hasYear = applied.count >= 4 && (0...(applied.count - 4)).contains { at in (at..<(at + 4)).allSatisfy { isDigit(applied[$0]) } }
                position = hasYear ? nil : "none"
            }
            if start == 0 {
                seen.leading = position.map { $0 == "first" ? "keep" : $0 == "last" ? "end" : $0 }
            } else {
                seen.trailing = position.map { $0 == "last" ? "keep" : $0 == "first" ? "start" : $0 }
            }
        }
        let baseBody = body(base, cut: baseDate.map { $0..<($0 + 10) })
        let appliedBody = body(applied, cut: appliedCut)
        let baseWords = S.rawWords(baseBody)
        let appliedWords = S.rawWords(appliedBody)
        let appliedSet = Set(appliedWords.map(S.key))

        if appliedWords.count >= 2 {
            let present = separators.filter { appliedBody.unicodeScalars.contains($0) }
            if present.count == 1 { seen.separator = present[0] }
        }
        seen.letterCase = letterCase(appliedBody)

        let people = Set(sample.people.map(S.key))
        let named = baseWords.filter { people.contains(S.key($0)) }
        if !named.isEmpty {
            let kept = named.filter { appliedSet.contains(S.key($0)) }.count
            if kept == named.count { seen.peopleKept = true } else if kept == 0 { seen.peopleKept = false }
        }

        let basePlain = baseWords.filter { !numeric($0) && !people.contains(S.key($0)) }
        let appliedPlain = appliedWords.filter { !numeric($0) && !people.contains(S.key($0)) }
        var distinct = Set<S.Key>()
        let candidates = basePlain.filter { distinct.insert(S.key($0)).inserted }
        if candidates.contains(where: { appliedSet.contains(S.key($0)) }) {
            seen.words = candidates.map { ($0, !appliedSet.contains(S.key($0))) }
        }
        if basePlain.count >= 2, !appliedPlain.isEmpty, appliedPlain.count <= basePlain.count,
           appliedPlain.map(S.key) == basePlain.prefix(appliedPlain.count).map(S.key) {
            seen.length = (appliedPlain.count, basePlain.count)
        }
        return seen
    }

    static func majority<T: Equatable>(_ votes: [T]) -> T? {
        guard votes.count >= minVotes else { return nil }
        return votes.first { vote in 2 * votes.filter { $0 == vote }.count > votes.count }
    }

    /// Preferences from up to `window` samples, newest first; the order does not change the result.
    public static func derive(_ samples: [Sample]) -> Learned {
        let observations = samples.prefix(window).map(observe)
        var learned = Learned()
        learned.separator = majority(observations.compactMap(\.separator)).flatMap { $0 == "-" ? nil : String($0) }
        learned.letterCase = majority(observations.compactMap(\.letterCase)).flatMap { $0 == "lower" ? nil : $0 }
        learned.leadingDate = majority(observations.compactMap(\.leading)).flatMap { ["end", "none"].contains($0) ? $0 : nil }
        learned.trailingDate = majority(observations.compactMap(\.trailing)).flatMap { ["start", "none"].contains($0) ? $0 : nil }
        learned.dateFormat = majority(observations.compactMap(\.dateFormat)).flatMap { $0 == "iso" ? nil : $0 }
        learned.omitPeople = majority(observations.compactMap(\.peopleKept)) == false
        var counts: [S.Key: (word: String, dropped: Int, kept: Int)] = [:]
        for (word, dropped) in observations.flatMap(\.words) {
            var entry = counts[S.key(word)] ?? (word, 0, 0)
            if dropped { entry.dropped += 1 } else { entry.kept += 1 }
            counts[S.key(word)] = entry
        }
        learned.dropWords = counts.values.filter { $0.dropped >= minDrops && $0.dropped > $0.kept }.map(\.word)
            .sorted { $0.unicodeScalars.lexicographicallyPrecedes($1.unicodeScalars) }
        let lengths = observations.compactMap(\.length)
        learned.maxWords = (1...maxWordCap).first { cap in
            let eligible = lengths.filter { $0.available > cap }
            return eligible.count >= minVotes && 2 * eligible.filter { $0.count <= cap }.count > eligible.count
        }
        return learned
    }

    public static func tagged(_ reason: String, _ learned: [String]) -> String {
        learned.isEmpty ? reason : reason + "|learned:" + learned.joined(separator: ",")
    }

    /// Splits a proposeNames reason into the plain reason and the learned
    /// preferences named after `|learned:`.
    public static func splitReason(_ reason: String) -> (reason: String, learned: [String]) {
        guard let marker = reason.range(of: "|learned:") else { return (reason, []) }
        return (String(reason[..<marker.lowerBound]), reason[marker.upperBound...].split(separator: ",").map(String.init))
    }
}
