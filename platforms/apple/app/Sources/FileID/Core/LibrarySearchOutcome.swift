import Foundation
import FileIDShared

/// How a Library search with the engine's strength summary ("3 matches (2 strong visual), 1 weaker.") is shown.
/// The engine lists matches first (strong picture scores, then name, tag and text hits), then weaker look-alikes;
/// its summary says how many of each and how many of the matches are strong visual ones.
enum LibrarySearchOutcome {
    /// Files whose own name holds the typed text go first, in their given order: a search for a file name should not bury that file under look-alike pictures.
    static func promotingNameMatches(_ rows: [FileRow], query: String) -> [FileRow] {
        let wanted = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !wanted.isEmpty else { return rows }
        let named = rows.filter { $0.url.lastPathComponent.range(of: wanted, options: [.caseInsensitive, .diacriticInsensitive]) != nil }
        guard !named.isEmpty, named.count < rows.count else { return rows }
        let ids = Set(named.map(\.id))
        return named + rows.filter { !ids.contains($0.id) }
    }

    struct Strength: Equatable {
        let good: Int
        let strong: Int?
        let weaker: Int
        let text: String

        /// The weaker count, with a "+" when the engine's result limit cut the list short.
        var weakerLabel: String { good + weaker >= LibrarySearchOutcome.resultLimit ? "\(weaker)+" : "\(weaker)" }

        /// The engine's sentence, with "+" on a count the request limit cut short.
        var displayText: String {
            guard good + weaker >= LibrarySearchOutcome.resultLimit else { return text }
            let noun = good == 1 ? "match" : "matches"
            let strongPart = strong.map { " (\($0)\(weaker == 0 ? "+" : "") strong visual)" } ?? ""
            switch (good, weaker) {
            case (_, 0): return "\(good)+ \(noun)\(strongPart)."
            case (0, _): return "No good matches, \(weaker)+ weaker."
            default: return "\(good) \(noun)\(strongPart), \(weaker)+ weaker."
            }
        }

        /// Nil unless `message` is exactly one of the engine's summary sentences.
        init?(message: String?) {
            let text = (message ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard text.count <= 80 else { return nil }
            if text == "No matches." { (good, strong, weaker) = (0, 0, 0) }
            else if let weak = Self.number(text, #"^No good matches, (\d{1,9}) weaker\.$"#, group: 1) { (good, strong, weaker) = (0, 0, weak) }
            else if let good = Self.number(text, #"^(\d{1,9}) match(?:es)?(?: \(\d{1,9} strong visual\))?(?:, \d{1,9} weaker)?\.$"#, group: 1) {
                self.good = good
                strong = Self.number(text, #" \((\d{1,9}) strong visual\)"#, group: 1)
                weaker = Self.number(text, #", (\d{1,9}) weaker\.$"#, group: 1) ?? 0
            } else { return nil }
            self.text = text
        }

        private static func number(_ text: String, _ pattern: String, group: Int) -> Int? {
            guard let regex = try? NSRegularExpression(pattern: pattern),
                  let match = regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)),
                  let range = Range(match.range(at: group), in: text) else { return nil }
            return Int(text[range])
        }
    }

    /// Hits the Library asks the engine for; a summary that reaches it counts only the rows that fit.
    static let resultLimit = 60

    enum Source: Equatable { case engine, keyword }

    struct Decision: Equatable {
        /// Which list fills the grid.
        var source: Source
        /// Add name, tag and text matches the engine's list lacks (after the engine's own).
        var addKeywordRows: Bool
        var label: String
        /// Weaker look-alikes kept back behind "Show N weaker matches".
        var hiddenWeaker: Int
    }

    static let keywordOnlyPrefix = "Keyword results only"
    static let keywordOnlyLabel = "Matches by name and text only"
    static let plainLabel = "Matches by name, text and what’s in the picture"

    /// The engine's summary recounted for the rows a kind filter keeps, in the engine's own wording.
    /// `hitIDs` is the engine's list in its order (good matches first). Nil when `message` is not a summary.
    static func narrowed(message: String?, hitIDs: [Int64], keptIDs: Set<Int64>) -> String? {
        guard let strength = Strength(message: message) else { return nil }
        var seen = Set<Int64>()
        let good = hitIDs.prefix(strength.good).filter { keptIDs.contains($0) && seen.insert($0).inserted }.count
        let weaker = hitIDs.dropFirst(strength.good).filter { keptIDs.contains($0) && seen.insert($0).inserted }.count
        if good == 0 { return weaker == 0 ? "No matches." : "No good matches, \(weaker) weaker." }
        return "\(good) \(good == 1 ? "match" : "matches")" + (weaker > 0 ? ", \(weaker) weaker." : ".")
    }

    /// `message` is the engine's reply message, nil when it sent none (older engine, no search model).
    static func decide(message: String?, keywordCount: Int, showWeaker: Bool) -> Decision {
        if message?.hasPrefix(keywordOnlyPrefix) == true {
            return Decision(source: .engine, addKeywordRows: true, label: keywordOnlyLabel, hiddenWeaker: 0)
        }
        guard let strength = Strength(message: message) else {
            return Decision(source: .engine, addKeywordRows: true, label: plainLabel, hiddenWeaker: 0)
        }
        if strength.good > 0 || showWeaker {
            return Decision(source: .engine, addKeywordRows: true, label: strength.displayText, hiddenWeaker: 0)
        }
        let label = strength.weaker > 0 ? (keywordCount > 0 ? "\(keywordCount) by name, tag or text" : "No good matches") : "No matches"
        return Decision(source: .keyword, addKeywordRows: false, label: label, hiddenWeaker: strength.weaker)
    }
}
