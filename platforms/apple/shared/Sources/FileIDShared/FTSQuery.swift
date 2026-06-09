// FTS5 MATCH parses its argument as a QUERY EXPRESSION, so binding a
// raw search string makes any input containing FTS operators or
// metacharacters (" * : ( ) - AND OR NEAR …) throw a syntax error —
// which callers typically catch into ZERO results for the whole
// search (M3). Quote each whitespace token as a literal phrase
// (doubling embedded quotes) and AND them, so metacharacters are
// matched literally.
import Foundation

public enum FTSQuery {
    public static func quoted(_ raw: String) -> String {
        raw.split(whereSeparator: { $0.isWhitespace })
            .map { "\"" + $0.replacingOccurrences(of: "\"", with: "\"\"") + "\"" }
            .joined(separator: " ")
    }
}
