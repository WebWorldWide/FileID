// FTS5 MATCH parses its argument as a QUERY EXPRESSION, so binding a
// raw search string makes any input containing FTS operators or
// metacharacters (" * : ( ) - AND OR NEAR …) throw a syntax error —
// which callers typically catch into ZERO results for the whole
// search. Quote each whitespace token as a literal phrase
// (doubling embedded quotes) and AND them, so metacharacters are
// matched literally.
import Foundation

public enum FTSQuery {
    public static func quoted(_ raw: String) -> String {
        raw.split(whereSeparator: { $0.isWhitespace })
            .map { "\"" + $0.replacingOccurrences(of: "\"", with: "\"\"") + "\"" }
            .joined(separator: " ")
    }

    /// `quoted` with the last token of every term that holds at least two
    /// letters or digits matched as a prefix (`"img 41" *`), so "IMG_41" finds
    /// "IMG_4181". Terms of one character stay whole words. Empty when no term
    /// qualifies. Mirrors `fts_prefix_query` in the Rust engine.
    public static func prefixed(_ raw: String) -> String {
        var any = false
        let terms = raw.split(whereSeparator: { $0.isWhitespace })
            .filter { $0.contains(where: { $0.isLetter || $0.isNumber }) }
            .map { token -> String in
                let quoted = "\"" + token.replacingOccurrences(of: "\"", with: "\"\"") + "\""
                if token.filter({ $0.isLetter || $0.isNumber }).count >= 2 {
                    any = true
                    return quoted + " *"
                }
                return quoted
            }
        return any ? terms.joined(separator: " ") : ""
    }
}
