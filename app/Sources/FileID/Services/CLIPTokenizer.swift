// OpenAI CLIP byte-pair encoding tokenizer — pure Swift port.
//
// The tokenizer's only job: take a search query like "sunset at the
// beach" and produce the integer token IDs CLIP's text transformer
// expects. We then feed those IDs to CLIPTextEncoder which produces a
// 512-d embedding aligned with the image-side embeddings stored in
// `clip_embeddings`. Cosine on those two vectors = "find images
// matching this text query."
//
// Reference: github.com/openai/CLIP/blob/main/clip/simple_tokenizer.py
//
// The vocabulary + merges files come from OpenAI's CLIP release. Run
// `scripts/fetch_clip_vocab.sh` once to download them. They land at
// ~/Library/Application Support/FileID/Models/clip_text/{vocab.json,
// merges.txt}.
import Foundation

public final class CLIPTokenizer: @unchecked Sendable {

    // MARK: - State
    //
    // Mutating state (encoder + bpeRanks + bpeCache) is guarded by
    // `lock`. `pattern` and `byteEncoder` are immutable, no lock
    // needed. The class is @unchecked Sendable because we manage
    // synchronization ourselves rather than via an actor (encode()
    // is called from a sync UI path; an actor hop would block
    // SwiftUI rendering).
    private let lock = NSLock()

    /// token string → token id
    private var encoder: [String: Int32] = [:]
    /// (a, b) → merge priority (lower = higher priority)
    private var bpeRanks: [Pair: Int] = [:]
    private let pattern: NSRegularExpression
    private let byteEncoder: [UInt8: String]

    /// CLIP encodes inputs to a fixed length of 77 (start + 75 + end).
    public let contextLength: Int = 77

    /// Stable POSIX-locale lowercaser used to normalize input. Avoids
    /// the surprise that Turkish locale lowercases "I" to "ı" instead
    /// of "i", which would tokenize differently than CLIP expects.
    private let posixLocale = Locale(identifier: "en_US_POSIX")

    public static let shared = CLIPTokenizer()

    // MARK: - Init

    private init() {
        // OpenAI's pre-tokenization regex — matches contractions,
        // letters, numbers, and other punctuation runs.
        let p = "<\\|startoftext\\|>|<\\|endoftext\\|>|'s|'t|'re|'ve|'m|'ll|'d|[\\p{L}]+|[\\p{N}]|[^\\s\\p{L}\\p{N}]+"
        // swiftlint:disable:next force_try
        pattern = try! NSRegularExpression(pattern: p, options: [.caseInsensitive])
        byteEncoder = Self.buildByteToUnicode()
    }

    /// True iff the vocab + merges files have been loaded successfully.
    public var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return !encoder.isEmpty
    }

    /// Load OpenAI's vocab.json + merges.txt from the model directory.
    /// Idempotent — re-loads silently if files were updated.
    public func loadVocabulary(modelDirectory: URL) -> Bool {
        let vocabURL = modelDirectory.appendingPathComponent("vocab.json")
        let mergesURL = modelDirectory.appendingPathComponent("merges.txt")
        // Size-cap before reading. The legitimate files are ~1 MB
        // (vocab.json) and ~500 KB (merges.txt). A corrupted or
        // tampered file with millions of entries would OOM the
        // decoder. 8 MB / 4 MB ceilings give 8x headroom for legit
        // variants while still bounding the worst-case allocation.
        let fm = FileManager.default
        let vocabCap: Int64 = 8 * 1024 * 1024
        let mergesCap: Int64 = 4 * 1024 * 1024
        let vocabSize = (try? fm.attributesOfItem(atPath: vocabURL.path)[.size] as? Int64) ?? 0
        let mergesSize = (try? fm.attributesOfItem(atPath: mergesURL.path)[.size] as? Int64) ?? 0
        guard vocabSize > 0, vocabSize <= vocabCap,
              mergesSize > 0, mergesSize <= mergesCap else {
            return false
        }
        guard let vocabData = try? Data(contentsOf: vocabURL),
              let mergesText = try? String(contentsOf: mergesURL, encoding: .utf8) else {
            return false
        }
        // vocab.json is { "tokenString": tokenID, ... }
        guard let vocab = try? JSONSerialization.jsonObject(with: vocabData) as? [String: Int]
        else { return false }
        var enc: [String: Int32] = [:]
        enc.reserveCapacity(vocab.count)
        for (k, v) in vocab { enc[k] = Int32(v) }
        // merges.txt: first line is a header, then "a b" per line
        let lines = mergesText.split(separator: "\n").dropFirst()
        var ranks: [Pair: Int] = [:]
        ranks.reserveCapacity(lines.count)
        for (i, line) in lines.enumerated() {
            let parts = line.split(separator: " ")
            guard parts.count == 2 else { continue }
            ranks[Pair(String(parts[0]), String(parts[1]))] = i
        }
        // Verify special tokens are present BEFORE flipping state.
        guard enc["<|startoftext|>"] != nil && enc["<|endoftext|>"] != nil else {
            return false
        }
        lock.lock()
        encoder = enc
        bpeRanks = ranks
        bpeCache.removeAll(keepingCapacity: false)
        lock.unlock()
        return true
    }

    // MARK: - Encode

    /// Tokenize a query into the fixed-length [Int32] CLIP expects.
    /// Returns nil if the vocabulary isn't loaded.
    public func encode(_ text: String) -> [Int32]? {
        // Snapshot vocab refs once under the lock so the encode hot
        // path doesn't re-acquire per dictionary lookup.
        lock.lock()
        let encoderSnapshot = encoder
        lock.unlock()
        guard !encoderSnapshot.isEmpty,
              let sot = encoderSnapshot["<|startoftext|>"],
              let eot = encoderSnapshot["<|endoftext|>"] else { return nil }

        let cleaned = text
            .lowercased(with: posixLocale)
            .trimmingCharacters(in: .whitespaces)
            .replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)

        var pieces: [String] = []
        let nsText = cleaned as NSString
        let matches = pattern.matches(in: cleaned, options: [],
                                       range: NSRange(location: 0, length: nsText.length))
        for m in matches {
            pieces.append(nsText.substring(with: m.range))
        }

        var tokens: [Int32] = [sot]
        for piece in pieces {
            let bytes = Array(piece.utf8)
            let chars = bytes.map { byteEncoder[$0] ?? "" }.joined()
            for token in bpe(token: chars).split(separator: " ") {
                if let id = encoderSnapshot[String(token)] {
                    tokens.append(id)
                }
            }
        }
        tokens.append(eot)

        if tokens.count > contextLength {
            tokens = Array(tokens.prefix(contextLength - 1)) + [eot]
        } else {
            tokens.append(contentsOf: Array(repeating: Int32(0),
                                              count: contextLength - tokens.count))
        }
        return tokens
    }

    // MARK: - BPE inner loop

    /// Memoized BPE results. Mutation guarded by `lock`.
    private var bpeCache: [String: String] = [:]

    private func bpe(token: String) -> String {
        // Cache lookup (lock-protected).
        lock.lock()
        if let cached = bpeCache[token] {
            lock.unlock()
            return cached
        }
        let ranks = bpeRanks
        lock.unlock()

        var word = token.map { String($0) }
        // CLIP marks the last char of every word with </w>.
        if let last = word.last {
            word[word.count - 1] = last + "</w>"
        }
        var pairs = pairsFor(word)
        if pairs.isEmpty {
            let result = word.last ?? token
            lock.lock(); bpeCache[token] = result; lock.unlock()
            return result
        }
        // Bound the merge loop. word.count strictly decreases each
        // successful merge, so worst case is len(word) iterations.
        // Keep an explicit cap as a safety net against malformed
        // merges files that could otherwise spin.
        var safety = word.count + 8
        while safety > 0 {
            safety -= 1
            // Find lowest-ranked pair (highest priority merge).
            var best: Pair?
            var bestRank = Int.max
            for p in pairs {
                if let r = ranks[p], r < bestRank {
                    bestRank = r
                    best = p
                }
            }
            guard let toMerge = best else { break }
            // Merge every adjacent occurrence of (toMerge.a, toMerge.b).
            var newWord: [String] = []
            var i = 0
            while i < word.count {
                if i < word.count - 1, word[i] == toMerge.a, word[i + 1] == toMerge.b {
                    newWord.append(toMerge.a + toMerge.b)
                    i += 2
                } else {
                    newWord.append(word[i])
                    i += 1
                }
            }
            // No-progress guard: if the merge produced an identical
            // word (shouldn't happen with valid merges, but be safe),
            // bail out so we never spin.
            if newWord.count == word.count { break }
            word = newWord
            if word.count == 1 { break }
            pairs = pairsFor(word)
        }
        let merged = word.joined(separator: " ")
        lock.lock(); bpeCache[token] = merged; lock.unlock()
        return merged
    }

    private func pairsFor(_ word: [String]) -> Set<Pair> {
        var out: Set<Pair> = []
        for i in 0..<(word.count - 1) {
            out.insert(Pair(word[i], word[i + 1]))
        }
        return out
    }

    // MARK: - Bytes ↔ Unicode

    /// OpenAI's byte-to-unicode table maps every byte 0–255 to a
    /// printable unicode codepoint, so the tokenizer can operate on
    /// strings without losing information about bytes that happen to
    /// look like control codes / spaces.
    private static func buildByteToUnicode() -> [UInt8: String] {
        var bs: [Int] = []
        bs.append(contentsOf: Array(33...126))   // !..~
        bs.append(contentsOf: Array(161...172))  // ¡..¬
        bs.append(contentsOf: Array(174...255))  // ®..ÿ
        var cs = bs
        var n = 0
        for b in 0..<256 where !bs.contains(b) {
            bs.append(b)
            cs.append(256 + n)
            n += 1
        }
        var out: [UInt8: String] = [:]
        for (i, byte) in bs.enumerated() {
            if let scalar = Unicode.Scalar(cs[i]) {
                out[UInt8(byte)] = String(scalar)
            }
        }
        return out
    }

    // MARK: - Helpers

    public struct Pair: Hashable {
        let a: String
        let b: String
        init(_ a: String, _ b: String) { self.a = a; self.b = b }
    }
}
