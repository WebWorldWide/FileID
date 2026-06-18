import Testing
@testable import FileIDEngine

// Parity fixtures mirror the Windows engine's wordpiece_tokenizer.rs tests so the BGE
// document embeddings tokenize identically across platforms (lockstep).
@Suite("WordPiece tokenizer (BGE — lockstep with Rust)")
struct WordPieceTokenizerTests {
    // id == insertion order, same toy vocab as the Rust tests.
    func toy() -> WordPieceTokenizer {
        let toks = ["[PAD]", "[UNK]", "[CLS]", "[SEP]", "play", "##ing", "##ed", "love", "!"]
        var vocab: [String: Int64] = [:]
        for (i, t) in toks.enumerated() { vocab[t] = Int64(i) }
        return WordPieceTokenizer(vocab: vocab, lowerCase: true)!
    }

    @Test("wraps with [CLS]/[SEP], mask all 1, types all 0")
    func wraps() {
        let e = toy().encode("play", maxLen: 16)
        #expect(e.ids.first == 2)
        #expect(e.ids.last == 3)
        #expect(e.attentionMask.count == e.ids.count)
        #expect(e.attentionMask.allSatisfy { $0 == 1 })
        #expect(e.typeIds.allSatisfy { $0 == 0 })
    }

    @Test("greedy splits into wordpieces: playing → play ##ing")
    func greedy() {
        #expect(toy().encode("playing", maxLen: 16).ids == [2, 4, 5, 3])
    }

    @Test("unknown word becomes [UNK]")
    func unknown() {
        #expect(toy().encode("xyzzy", maxLen: 16).ids == [2, 1, 3])
    }

    @Test("lowercases and splits punctuation: LOVE! → love !")
    func lowerPunct() {
        #expect(toy().encode("LOVE!", maxLen: 16).ids == [2, 7, 8, 3])
    }

    @Test("truncates to maxLen keeping [CLS]/[SEP]")
    func truncate() {
        let e = toy().encode("play play play play", maxLen: 4)
        #expect(e.ids.count == 4)
        #expect(e.ids.first == 2)
        #expect(e.ids.last == 3)
    }

    @Test("missing special tokens fails init")
    func missingSpecials() {
        #expect(WordPieceTokenizer(vocab: ["hello": 0], lowerCase: true) == nil)
    }

    @Test("all-punctuation splits per char")
    func allPunct() {
        #expect(toy().encode("!!!", maxLen: 16).ids == [2, 8, 8, 8, 3])
        #expect(toy().encode("???", maxLen: 16).ids == [2, 1, 1, 1, 3])
    }

    @Test("word past max input chars becomes [UNK]")
    func pastMaxChars() {
        let word = "play" + String(repeating: "ing", count: 32) + "g" // 101 chars
        #expect(toy().encode(word, maxLen: 256).ids == [2, 1, 3])
    }
}
