// When a file's text deserves a careful read: Vision's accurate tier during the
// scan (macOS) and a VLM transcription during Deep Analyze (every engine).
// Mirrored rule for rule by the Rust engine's `pipeline/text_reading.rs`; the
// shared conformance case pins the search side.
import Foundation

enum TextReading {
    static let minimumReadableCharacters = 24
    static let garbledShareThreshold = 0.3
    static let maxTranscriptionCharacters = 4_000

    /// Scene tags (Vision classifier identifiers and RAM++ tags, lowercased with
    /// `_` read as a space) that suggest handwriting.
    static let handwritingHints: Set<String> = [
        "handwriting", "whiteboard", "blackboard", "chalkboard", "chalk", "sticky note",
        "notebook", "notepad", "note", "manuscript", "calligraphy", "signature",
        "journal", "diary", "letter", "envelope", "postcard", "greeting card",
    ]

    /// Tags that suggest printed or mixed text worth reading. RAM++ puts "writing"
    /// on printed receipts and forms too, so it only gates the read.
    static let paperHints: Set<String> = [
        "document", "printed page", "paper", "receipt", "bill", "form", "contract",
        "certificate", "recipe", "list", "menu", "newspaper", "poster", "screenshot",
        "sign", "diagram", "calendar", "checkbook", "text", "presentation", "writing", "write",
    ]

    static func normalized(_ tag: String) -> String {
        tag.lowercased().replacingOccurrences(of: "_", with: " ")
            .trimmingCharacters(in: .whitespaces)
    }

    static func hasHandwritingHint(_ tags: [String]) -> Bool {
        tags.contains { handwritingHints.contains(normalized($0)) }
    }

    static func isTextBearing(_ tags: [String]) -> Bool {
        tags.contains {
            let tag = normalized($0)
            return handwritingHints.contains(tag) || paperHints.contains(tag)
        }
    }

    /// A scanner-shaped page (US Letter 11:8.5 or A4 √2, either orientation) with
    /// no camera EXIF: worth a text read even when the classifier saw no paper.
    /// Tight bands, because 4:3 (1.333) is every camera-less phone export.
    static func isDocumentShape(width: Int, height: Int, hasCamera: Bool) -> Bool {
        guard !hasCamera, width > 0, height > 0 else { return false }
        let ratio = Double(max(width, height)) / Double(min(width, height))
        return abs(ratio - 11.0 / 8.5) < 0.015 || abs(ratio - 2.0.squareRoot()) < 0.015
    }

    /// Words a character-level reader typically mangles on handwriting: case flips
    /// inside a word ("syMc", "PMitri"), symbols between letters ("denti&t"), or
    /// five consonants in a row ("Swvbda").
    static func isGarbledWord(_ raw: Substring) -> Bool {
        let edge = CharacterSet(charactersIn: ".,:;!?'\"()[]{}")
        let word = raw.trimmingCharacters(in: edge)
        let letters = word.filter(\.isLetter)
        guard letters.count >= 2 else { return false }
        let chars = Array(word)
        var consonantRun = 0
        for index in chars.indices {
            let char = chars[index]
            if index > 0 {
                let previous = chars[index - 1]
                if previous.isLowercase && char.isUppercase { return true }
                if char.isLowercase, index >= 2, previous.isUppercase, chars[index - 2].isUppercase,
                   !(char == "s" && index == chars.count - 1) {
                    return true
                }
                if !char.isLetter, !char.isNumber, !"'-.".contains(char),
                   previous.isLetter, index + 1 < chars.count, chars[index + 1].isLetter {
                    return true
                }
            }
            if char.isLetter, !"aeiouyAEIOUY".contains(char) {
                consonantRun += 1
                if consonantRun >= 5 { return true }
            } else {
                consonantRun = 0
            }
        }
        return false
    }

    static func garbled(_ text: String) -> (count: Int, share: Double) {
        let words = text.split(whereSeparator: \.isWhitespace)
            .filter { $0.filter(\.isLetter).count >= 2 }
        guard !words.isEmpty else { return (0, 0) }
        let count = words.filter(isGarbledWord).count
        return (count, Double(count) / Double(words.count))
    }

    /// After the fast read: does this image deserve Vision's accurate tier?
    static func needsCarefulRead(fastText: String, tags: [String], textBearing: Bool) -> Bool {
        if hasHandwritingHint(tags) { return true }
        let compact = fastText.trimmingCharacters(in: .whitespacesAndNewlines)
        if compact.count < minimumReadableCharacters { return true }
        let quality = garbled(compact)
        return quality.share >= garbledShareThreshold || ((textBearing || isTextBearing(tags)) && quality.count > 0)
    }

    /// Label the careful read as handwriting only on a handwriting tag or a fast read
    /// that mangled most words; a clean printed page that the accurate tier merely
    /// polished stays "Text in image".
    static func isHandwritten(fastText: String, carefulText: String, tags: [String]) -> Bool {
        guard !carefulText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return false }
        return hasHandwritingHint(tags) || garbled(fastText).share >= garbledShareThreshold
            || disagreement(fast: fastText, careful: carefulText) >= garbledShareThreshold
    }

    /// Share of the careful read's words the fast read didn't produce (case and
    /// edge punctuation ignored). The fast tier reads print almost as well as the
    /// accurate one (a receipt: 0.22) but loses most handwritten words (a sticky
    /// note: 0.67; script the fast tier can't see at all: 1.0).
    static func disagreement(fast: String, careful: String) -> Double {
        let edge = CharacterSet.punctuationCharacters.union(.symbols)
        func words(_ text: String) -> [String] {
            text.lowercased().split(whereSeparator: \.isWhitespace)
                .map { $0.trimmingCharacters(in: edge) }
                .filter { $0.contains(where: \.isLetter) }
        }
        let carefulWords = words(careful)
        // On a sign or label one missed word is half the text; too few to judge.
        guard carefulWords.count >= 4 else { return 0 }
        let fastWords = Set(words(fast))
        return Double(carefulWords.filter { !fastWords.contains($0) }.count) / Double(carefulWords.count)
    }

    /// The careful read when it found text, else the fast read.
    static func preferred(fast: String, careful: String?) -> String {
        guard let careful, !careful.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return fast }
        return careful
    }

    /// Deep Analyze: should the VLM transcribe this file's text? Images the scan
    /// flagged as text-bearing whose stored reading is missing, short or mangled, and
    /// PDFs with no text layer and no OCR. Never repeats a VLM transcription.
    static func needsTranscription(kind: String, tags: [String], ocrText: String?, ocrSource: String?, hasDocText: Bool) -> Bool {
        if ocrSource == "vlm" { return false }
        let compact = (ocrText ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        switch kind {
        case "pdf":
            return !hasDocText && compact.count < minimumReadableCharacters
        case "image":
            guard isTextBearing(tags) else { return false }
            return compact.count < minimumReadableCharacters || garbled(compact).share >= garbledShareThreshold
        default:
            return false
        }
    }

    static let transcriptionPrompt = """
        Transcribe every word of handwritten or printed text visible in this image, exactly as written, one line per line of text. \
        Do not describe the image and do not guess at illegible words. If there is no readable text, reply NONE. \
        End with one final line: HANDWRITTEN: yes if most of the text is handwritten, otherwise HANDWRITTEN: no.
        """

    /// Parsed transcription, or nil when the model saw no readable text.
    static func parseTranscription(_ raw: String) -> (text: String, handwritten: Bool)? {
        var handwritten = false
        var lines: [String] = []
        for line in raw.replacingOccurrences(of: "\r", with: "").split(separator: "\n", omittingEmptySubsequences: true) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            let upper = trimmed.uppercased()
            if upper.hasPrefix("HANDWRITTEN:") {
                handwritten = upper.dropFirst("HANDWRITTEN:".count).trimmingCharacters(in: .whitespaces).hasPrefix("YES")
                continue
            }
            if trimmed.hasPrefix("```") { continue }
            lines.append(trimmed)
        }
        let text = String(lines.joined(separator: "\n").prefix(maxTranscriptionCharacters))
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let folded = text.uppercased().trimmingCharacters(in: CharacterSet(charactersIn: ". "))
        if text.isEmpty || folded == "NONE" || folded.hasPrefix("NO READABLE TEXT") || folded.hasPrefix("THE IMAGE") {
            return nil
        }
        return (text, handwritten)
    }
}
