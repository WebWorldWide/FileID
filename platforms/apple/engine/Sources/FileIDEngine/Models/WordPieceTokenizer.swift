// Minimal BERT-family WordPiece tokenizer — faithful port of the Windows engine's
// `models/wordpiece_tokenizer.rs`, so the BGE-small document embeddings match across
// platforms (lockstep). Greedy longest-match-first WordPiece over a HuggingFace-style
// `vocab.txt`, mirroring `BertTokenizer(do_lower_case=True)` for the common English case.
// Unlike CLIPTokenizer (BPE), BERT models need [CLS]/[SEP], an attention mask, and
// token-type ids.
//
// One deliberate nuance vs. the Rust source: this iterates grapheme clusters where Rust
// iterates Unicode scalars (`char`). They are identical for the common case; they can
// differ only on an exotic single "word" of 100+ combining-mark SCALARS (the
// MAX_INPUT_CHARS_PER_WORD cap counts graphemes here, scalars there). The per-platform doc
// text extractors (textutil/PDFKit vs pdfium) already don't guarantee bit-identical input,
// so this is within the "near-identical, not bit-identical" envelope the doc pass accepts.

import Foundation

/// The three parallel input tensors a BERT ONNX graph expects.
struct WordPieceEncoding {
    let ids: [Int64]
    let attentionMask: [Int64]
    let typeIds: [Int64]
}

final class WordPieceTokenizer {
    private let vocab: [String: Int64]
    private let clsID: Int64
    private let sepID: Int64
    private let unkID: Int64
    let padID: Int64
    private let lowerCase: Bool

    private static let cls = "[CLS]"
    private static let sep = "[SEP]"
    private static let unk = "[UNK]"
    private static let pad = "[PAD]"
    /// HF caps a single whitespace token at 100 chars before emitting [UNK].
    private static let maxInputCharsPerWord = 100

    /// Load from a HuggingFace `vocab.txt` (one token per line; line number = token id).
    convenience init?(vocabFile: URL, lowerCase: Bool = true) {
        guard let text = try? String(contentsOf: vocabFile, encoding: .utf8) else { return nil }
        var vocab: [String: Int64] = [:]
        var i: Int64 = 0
        for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
            // A trailing \r from a Windows checkout never belongs to the token.
            let tok = line.hasSuffix("\r") ? String(line.dropLast()) : String(line)
            vocab[tok] = i
            i += 1
        }
        self.init(vocab: vocab, lowerCase: lowerCase)
    }

    init?(vocab: [String: Int64], lowerCase: Bool = true) {
        guard let cls = vocab[Self.cls], let sep = vocab[Self.sep], let unk = vocab[Self.unk] else {
            return nil
        }
        self.vocab = vocab
        self.clsID = cls
        self.sepID = sep
        self.unkID = unk
        self.padID = vocab[Self.pad] ?? 0
        self.lowerCase = lowerCase
    }

    /// Encode one string into `[CLS] … [SEP]`, truncated to `maxLen` (inclusive of the two
    /// special tokens). Returns ids + an all-1 attention mask + all-0 token-type ids.
    func encode(_ text: String, maxLen: Int) -> WordPieceEncoding {
        let maxLen = max(maxLen, 2)
        var ids: [Int64] = [clsID]
        outer: for word in basicTokenize(text, maxWords: maxLen) {
            for piece in wordpiece(word) {
                if ids.count >= maxLen - 1 { break outer }
                ids.append(piece)
            }
        }
        ids.append(sepID)
        return WordPieceEncoding(
            ids: ids,
            attentionMask: Array(repeating: 1, count: ids.count),
            typeIds: Array(repeating: 0, count: ids.count))
    }

    /// BERT "basic tokenizer": whitespace split, optional full-Unicode lowercasing, and
    /// ASCII punctuation broken into its own tokens. Bounded to `maxWords` (the
    /// punctuation branch may overshoot by one) + a 64-char-per-word pre-slice so a huge
    /// document can't materialize an unbounded Vec of tiny strings before truncation.
    private func basicTokenize(_ text: String, maxWords: Int) -> [String] {
        let sliced: Substring
        let cap = maxWords.multipliedReportingOverflow(by: 64).0
        if let idx = text.index(text.startIndex, offsetBy: cap, limitedBy: text.endIndex) {
            sliced = text[..<idx]
        } else {
            sliced = text[...]
        }
        let src = lowerCase ? sliced.lowercased() : String(sliced)
        var out: [String] = []
        var cur = ""
        for c in src {
            if out.count >= maxWords { return out }
            if c.isWhitespace {
                if !cur.isEmpty { out.append(cur); cur = "" }
            } else if isASCIIPunct(c) {
                if !cur.isEmpty { out.append(cur); cur = "" }
                out.append(String(c))
            } else {
                cur.append(c)
            }
        }
        if !cur.isEmpty { out.append(cur) }
        return out
    }

    /// Greedy longest-match-first WordPiece over a single whitespace token. Returns [UNK]
    /// for a token over the length cap or with any unmatchable prefix (matches HF).
    private func wordpiece(_ word: String) -> [Int64] {
        let chars = Array(word)
        if chars.count > Self.maxInputCharsPerWord { return [unkID] }
        var out: [Int64] = []
        var start = 0
        while start < chars.count {
            var end = chars.count
            var matched: Int64?
            while start < end {
                var sub = String(chars[start..<end])
                if start > 0 { sub = "##" + sub }
                if let id = vocab[sub] { matched = id; break }
                end -= 1
            }
            guard let id = matched else { return [unkID] }
            out.append(id)
            start = end
        }
        return out
    }

    /// `Character.isPunctuation` excludes ASCII symbols like `!`,`#`,`$` that Rust's
    /// `is_ascii_punctuation` (and HF) treat as punctuation; cover the full ASCII set.
    private func isASCIIPunct(_ c: Character) -> Bool {
        guard let a = c.asciiValue else { return false }
        return (33...47).contains(a) || (58...64).contains(a)
            || (91...96).contains(a) || (123...126).contains(a)
    }
}
