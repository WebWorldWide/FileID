import Foundation

package enum DocumentKeywords {
    private static let maxTags = 8
    private static let minPhraseBytes = 3
    private static let maxPhraseBytes = 60
    private static let maxPhraseWords = 4
    private static let phraseJoiners: Set<Character> = [" ", "-", "'", "\u{2019}", "_"]
    private static let stopwords: Set<String> = [
        "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "has", "have", "he",
        "her", "his", "i", "if", "in", "into", "is", "it", "its", "just", "of", "on", "or", "our",
        "she", "so", "than", "that", "the", "their", "them", "then", "there", "these", "they", "this",
        "those", "to", "was", "we", "were", "what", "when", "where", "which", "who", "why", "will",
        "with", "you", "your", "about", "after", "all", "also", "any", "because", "been", "before",
        "between", "both", "can", "could", "do", "does", "done", "each", "few", "had", "having", "how",
        "more", "most", "no", "nor", "not", "now", "off", "once", "only", "other", "out", "over",
        "own", "same", "should", "some", "such", "through", "too", "under", "until", "up", "very",
        "while", "would",
    ]

    package static func extract(_ text: String) -> [(label: String, score: Double)] {
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return [] }
        let phrases = splitIntoPhrases(text)
        guard !phrases.isEmpty else { return [] }

        var frequency: [String: Int] = [:]
        var degree: [String: Int] = [:]
        for phrase in phrases {
            for word in phrase {
                frequency[word, default: 0] += 1
                degree[word, default: 0] += phrase.count
            }
        }

        var scores: [String: Double] = [:]
        for phrase in phrases {
            let label = phrase.joined(separator: " ")
            guard (minPhraseBytes...maxPhraseBytes).contains(label.utf8.count) else { continue }
            let score = phrase.reduce(0.0) { partial, word in
                partial + Double(degree[word, default: 1]) / Double(frequency[word, default: 1])
            }
            scores[label] = max(scores[label] ?? 0, score)
        }

        return scores
            .map { (label: $0.key, score: $0.value) }
            .sorted {
                if $0.score != $1.score { return $0.score > $1.score }
                return $0.label < $1.label
            }
            .prefix(maxTags)
            .map { $0 }
    }

    static func groundedFilename(from text: String) -> String? {
        var words: [String] = []
        for keyword in extract(text) {
            for word in keyword.label.split(separator: " ").map(String.init) where !words.contains(word) {
                words.append(word)
                if words.count == 5 { break }
            }
            if words.count >= 3 { break }
        }
        if words.count < 3 {
            for phrase in splitIntoPhrases(text) {
                for word in phrase where !words.contains(word) {
                    words.append(word)
                    if words.count == 5 { break }
                }
                if words.count >= 3 { break }
            }
        }
        // The filename sanitizer keeps ASCII only and would split "Zürich" into "z-rich": fold accents first and drop any word that is still not plain ASCII.
        let plain = words.compactMap { word -> String? in
            let folded = word.folding(options: .diacriticInsensitive, locale: nil)
            return folded.unicodeScalars.allSatisfy { $0.isASCII } ? folded : nil
        }
        guard plain.count >= 3 else { return nil }
        return DeepAnalyze.sanitize(filename: plain.prefix(5).joined(separator: "-"))
    }

    private static func splitIntoPhrases(_ text: String) -> [[String]] {
        var phrases: [[String]] = []
        var current: [String] = []
        func flush() {
            if current.count > maxPhraseWords {
                // A long unpunctuated run is a sentence fragment, not a noun phrase.
                phrases.append(contentsOf: current.map { [$0] })
            } else if !current.isEmpty {
                phrases.append(current)
            }
            current.removeAll(keepingCapacity: true)
        }

        var run = ""
        func finishWord() {
            guard !run.isEmpty else { return }
            let word = asciiLowercased(Substring(run))
            run = ""
            let startsWithASCIIDigit = word.first.map { $0.isASCII && $0.isNumber } ?? false
            if stopwords.contains(word) || startsWithASCIIDigit || word.unicodeScalars.count == 1 {
                flush()
            } else {
                current.append(word)
            }
        }
        for character in text + "\n" {
            if character.isLetter || character.isNumber {
                run.append(character)
                continue
            }
            finishWord()
            // Punctuation, a line break or a tab ends the phrase; spaces, hyphens,
            // apostrophes and underscores stay inside it.
            if !phraseJoiners.contains(character) { flush() }
        }
        flush()
        return phrases
    }

    private static func asciiLowercased(_ text: Substring) -> String {
        String(text.unicodeScalars.map { scalar in
            if (65...90).contains(scalar.value), let lowered = UnicodeScalar(scalar.value + 32) {
                return Character(lowered)
            }
            return Character(scalar)
        })
    }
}
