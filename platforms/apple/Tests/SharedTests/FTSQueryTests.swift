// Regression tests for FTSQuery.quoted — the M3 fix. FTS5 MATCH
// parses its argument as a query expression, so unquoted user input
// containing " * : ( ) - AND OR NEAR … threw a syntax error that the
// caller's catch turned into zero results. SharedTests has no GRDB
// dependency, so instead of a live FTS5 table these tests verify the
// pure transformation exhaustively against the FTS5 string grammar:
// every output must be a space-joined sequence of quoted phrases with
// embedded quotes doubled — the one shape FTS5 always parses as
// literal text.
import Testing
@testable import FileIDShared

@Suite("FTSQuery — quoted")
struct FTSQueryTests {

    // Parses output against the FTS5 quoted-phrase grammar:
    // query := phrase (" " phrase)* | "" ; phrase := '"' (char | '""')* '"'
    private static func isLiteralPhraseQuery(_ q: String) -> Bool {
        var rest = Substring(q)
        while !rest.isEmpty {
            guard rest.first == "\"" else { return false }
            rest = rest.dropFirst()
            var closed = false
            while let c = rest.first {
                rest = rest.dropFirst()
                if c == "\"" {
                    if rest.first == "\"" {
                        rest = rest.dropFirst()
                    } else {
                        closed = true
                        break
                    }
                }
            }
            guard closed else { return false }
            if rest.isEmpty { return true }
            guard rest.first == " " else { return false }
            rest = rest.dropFirst()
            guard !rest.isEmpty else { return false }
        }
        return true
    }

    @Test("Single token wrapped as a phrase")
    func singleToken() {
        #expect(FTSQuery.quoted("invoice") == "\"invoice\"")
    }

    @Test("Tokens are split on whitespace and AND-joined")
    func multipleTokens() {
        #expect(FTSQuery.quoted("tax invoice 2024") == "\"tax\" \"invoice\" \"2024\"")
        #expect(FTSQuery.quoted("a\tb\n c   d") == "\"a\" \"b\" \"c\" \"d\"")
    }

    @Test("Embedded double quotes are doubled")
    func embeddedQuotes() {
        #expect(FTSQuery.quoted("say\"hi") == "\"say\"\"hi\"")
        #expect(FTSQuery.quoted("\"") == "\"\"\"\"")
        #expect(FTSQuery.quoted("\"phrase\"") == "\"\"\"phrase\"\"\"")
        #expect(FTSQuery.quoted("\"unterminated phrase") == "\"\"\"unterminated\" \"phrase\"")
    }

    @Test("FTS metacharacters and operators become literal phrases",
          arguments: [
              "*", ":", "(", ")", "-", "^", "+", ",", ".", "{", "}",
              "pre*", "col:val", "-negated", "(group)", "a-b",
              "NEAR", "NEAR(a,b)", "AND", "OR", "NOT", "near/2",
          ])
    func metacharactersQuoted(input: String) {
        #expect(FTSQuery.quoted(input) == "\"\(input)\"")
    }

    @Test("Empty and whitespace-only input produce an empty query")
    func emptyInput() {
        #expect(FTSQuery.quoted("") == "")
        #expect(FTSQuery.quoted("   \t\n") == "")
    }

    @Test("Unicode passes through unchanged")
    func unicodePreserved() {
        #expect(FTSQuery.quoted("café 写真") == "\"café\" \"写真\"")
        #expect(FTSQuery.quoted("naïve🙂") == "\"naïve🙂\"")
    }

    @Test("Hostile inputs always satisfy the FTS5 literal-phrase grammar",
          arguments: [
              "report \"final\" v2", "file*name", "tag:beach OR tag:sea",
              "NEAR(\"a\" \"b\", 5)", "((((", "))))", "\"\"\"", "- - -",
              "AND OR NOT NEAR", "100%_discount", "C:\\Users\\me",
              "a\"b\"c\"d", "*:^+,-(){}", "emoji 🙂 \" mix *",
              " leading and trailing ", "\t\"\t*\t", "ümlaut–dash—test",
          ])
    func hostileInputsAreLiteral(input: String) {
        let out = FTSQuery.quoted(input)
        #expect(Self.isLiteralPhraseQuery(out), "input \(input) → \(out)")
        #expect(!out.isEmpty)
    }
}
