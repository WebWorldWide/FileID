// Pathological-input bounds for the CLIP BPE tokenizer (SECURITY.md
// tokenizer-DoS hardening): megabyte inputs must stay bounded, the
// output shape must stay fixed at contextLength, and over-cap vocab /
// merges fixtures must fail closed. Time bounds are deliberately
// loose — CI machines vary; output correctness is the hard assert.
import Foundation
import Testing
@testable import FileIDShared

@Suite("CLIPTokenizer — DoS bounds")
struct CLIPTokenizerTests {

    private static let sot: Int32 = 100
    private static let eot: Int32 = 101

    private func fixtureDir(vocab: [String: Int], mergeLines: [String]) throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("clip-tokenizer-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try JSONSerialization.data(withJSONObject: vocab)
            .write(to: dir.appendingPathComponent("vocab.json"))
        let merges = (["#version: 0.2"] + mergeLines).joined(separator: "\n")
        try Data(merges.utf8).write(to: dir.appendingPathComponent("merges.txt"))
        return dir
    }

    private func readyTokenizer() throws -> CLIPTokenizer {
        let vocab: [String: Int] = [
            "<|startoftext|>": Int(Self.sot),
            "<|endoftext|>": Int(Self.eot),
            "a": 2,
            "a</w>": 3,
        ]
        let dir = try fixtureDir(vocab: vocab, mergeLines: [])
        defer { try? FileManager.default.removeItem(at: dir) }
        let tokenizer = CLIPTokenizer()
        #expect(tokenizer.loadVocabulary(modelDirectory: dir))
        #expect(tokenizer.isReady)
        return tokenizer
    }

    @Test("1 MB unbroken ASCII word is bounded and EOT-truncated")
    func megabyteUnbrokenWord() throws {
        let tokenizer = try readyTokenizer()
        let input = String(repeating: "a", count: 1_048_576)
        var tokens: [Int32]?
        let elapsed = ContinuousClock().measure { tokens = tokenizer.encode(input) }
        #expect(elapsed < .seconds(5))
        let toks = try #require(tokens)
        #expect(toks.count == tokenizer.contextLength)
        #expect(toks.first == Self.sot)
        #expect(toks.last == Self.eot)
        // Truncated, not padded — every slot before EOT is a real token.
        #expect(!toks.contains(0))
    }

    @Test("1 MB of combining-mark graphemes is bounded")
    func megabyteCombiningMarks() throws {
        let tokenizer = try readyTokenizer()
        let input = String(repeating: "e\u{0301}", count: 349_526)
        var tokens: [Int32]?
        let elapsed = ContinuousClock().measure { tokens = tokenizer.encode(input) }
        #expect(elapsed < .seconds(5))
        let toks = try #require(tokens)
        #expect(toks.count == tokenizer.contextLength)
        #expect(toks.first == Self.sot)
        #expect(toks.contains(Self.eot))
    }

    @Test("250K 4-byte emoji exercise char-safe UTF-8 truncation")
    func quarterMillionEmoji() throws {
        let tokenizer = try readyTokenizer()
        let input = String(repeating: "\u{1F600}", count: 250_000)
        var tokens: [Int32]?
        let elapsed = ContinuousClock().measure { tokens = tokenizer.encode(input) }
        #expect(elapsed < .seconds(5))
        let toks = try #require(tokens)
        #expect(toks.count == tokenizer.contextLength)
        #expect(toks.first == Self.sot)
        #expect(toks.contains(Self.eot))
    }

    @Test("Empty and all-punctuation inputs keep the fixed shape")
    func emptyAndPunctuation() throws {
        let tokenizer = try readyTokenizer()

        let empty = try #require(tokenizer.encode(""))
        #expect(empty.count == tokenizer.contextLength)
        #expect(empty[0] == Self.sot)
        #expect(empty[1] == Self.eot)
        #expect(empty[2...].allSatisfy { $0 == 0 })

        let punct = try #require(tokenizer.encode(String(repeating: "!?#@", count: 4_096)))
        #expect(punct.count == tokenizer.contextLength)
        #expect(punct.first == Self.sot)
        #expect(punct.contains(Self.eot))
    }

    @Test("Over-cap merges file fails closed")
    func overCapMerges() throws {
        let vocab: [String: Int] = [
            "<|startoftext|>": Int(Self.sot),
            "<|endoftext|>": Int(Self.eot),
        ]
        let dir = try fixtureDir(vocab: vocab,
                                 mergeLines: Array(repeating: "a b", count: 50_001))
        defer { try? FileManager.default.removeItem(at: dir) }
        let tokenizer = CLIPTokenizer()
        #expect(tokenizer.loadVocabulary(modelDirectory: dir) == false)
        #expect(tokenizer.isReady == false)
        #expect(tokenizer.encode("hello") == nil)
    }

    @Test("Over-cap vocab fails closed")
    func overCapVocab() throws {
        var vocab: [String: Int] = [
            "<|startoftext|>": Int(Self.sot),
            "<|endoftext|>": Int(Self.eot),
        ]
        for i in 0..<65_535 { vocab["t\(i)"] = i + 200 }
        let dir = try fixtureDir(vocab: vocab, mergeLines: [])
        defer { try? FileManager.default.removeItem(at: dir) }
        let tokenizer = CLIPTokenizer()
        #expect(tokenizer.loadVocabulary(modelDirectory: dir) == false)
        #expect(tokenizer.isReady == false)
        #expect(tokenizer.encode("hello") == nil)
    }

    @Test("encode is nil before a vocabulary loads")
    func encodeWithoutVocab() {
        let tokenizer = CLIPTokenizer()
        #expect(tokenizer.isReady == false)
        #expect(tokenizer.encode("hello") == nil)
    }
}
