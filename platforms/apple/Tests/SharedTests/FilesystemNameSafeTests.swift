// Unit tests for FilesystemNameSafe — the cross-platform filename
// component sanitizer. Verifies safety on Windows NTFS + Linux ext4
// + BSD UFS for the rules we promise: illegal-char replacement,
// reserved-name handling, trailing-dot/space trimming, length cap.
import Testing
@testable import FileIDShared

@Suite("FilesystemNameSafe — componentSafe")
struct FilesystemNameSafeTests {

    @Test("Windows-illegal chars replaced with _")
    func illegalCharsReplaced() {
        let cases: [(input: String, expected: String)] = [
            ("John<Jr>",     "John_Jr_"),
            ("a:b",          "a_b"),
            ("a/b",          "a_b"),
            ("a\\b",         "a_b"),
            ("a|b",          "a_b"),
            ("a?b",          "a_b"),
            ("a*b",          "a_b"),
            ("\"quote\"",    "_quote_"),
        ]
        for c in cases {
            #expect(FilesystemNameSafe.componentSafe(c.input) == c.expected,
                    "input \(c.input)")
        }
    }

    @Test("ASCII control bytes replaced")
    func controlBytesReplaced() {
        let raw = "a\u{00}b\u{01}c\u{1F}d"
        #expect(FilesystemNameSafe.componentSafe(raw) == "a_b_c_d")
    }

    @Test("Trailing dots and spaces trimmed")
    func trailingTrimmed() {
        #expect(FilesystemNameSafe.componentSafe("Photo.jpg.") == "Photo.jpg")
        #expect(FilesystemNameSafe.componentSafe("Photo  ")     == "Photo")
        #expect(FilesystemNameSafe.componentSafe("Photo. . . ") == "Photo")
    }

    @Test("Windows reserved basenames get _ prefix")
    func reservedNames() {
        #expect(FilesystemNameSafe.componentSafe("CON")     == "_CON")
        #expect(FilesystemNameSafe.componentSafe("con")     == "_con")
        #expect(FilesystemNameSafe.componentSafe("Aux.txt") == "_Aux.txt")
        #expect(FilesystemNameSafe.componentSafe("LPT1.log") == "_LPT1.log")
        #expect(FilesystemNameSafe.componentSafe("nul")     == "_nul")
        // "console" starts with "con" but isn't reserved.
        #expect(FilesystemNameSafe.componentSafe("console") == "console")
    }

    @Test("Empty input returns _")
    func emptyInput() {
        #expect(FilesystemNameSafe.componentSafe("") == "_")
        #expect(FilesystemNameSafe.componentSafe("   ") == "_")
        #expect(FilesystemNameSafe.componentSafe("...") == "_")
    }

    @Test("Length capped at maxLength")
    func lengthCap() {
        let long = String(repeating: "a", count: 300)
        #expect(FilesystemNameSafe.componentSafe(long).count == 200)
        #expect(FilesystemNameSafe.componentSafe(long, maxLength: 16).count == 16)
    }

    @Test("Unicode passes through unchanged")
    func unicodePreserved() {
        #expect(FilesystemNameSafe.componentSafe("Café") == "Café")
        #expect(FilesystemNameSafe.componentSafe("写真") == "写真")
        #expect(FilesystemNameSafe.componentSafe("naïve") == "naïve")
    }

    @Test("Combined: bad input becomes safe")
    func combined() {
        let input = "  John<Jr>:Test/\\file.   "
        let out = FilesystemNameSafe.componentSafe(input)
        #expect(out == "  John_Jr__Test__file")
        // Verify result has no illegal chars.
        for c in out {
            #expect(!"<>:\"/\\|?*".contains(c))
        }
    }
}
