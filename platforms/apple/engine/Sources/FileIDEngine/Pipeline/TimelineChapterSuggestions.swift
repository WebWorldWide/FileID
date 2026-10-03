import Foundation
import FileIDShared

enum TimelineChapterSuggestions {
    struct Caption: Sendable {
        let seconds: Double
        let text: String
    }

    static func propose(
        fileID: Int64,
        sourceRevision: String,
        frameModelVersion: String,
        duration: Double,
        captions: [Caption]
    ) -> [CatalogChapter] {
        let ordered = captions
            .filter { $0.seconds.isFinite && $0.seconds >= 0 && !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
            .sorted { $0.seconds < $1.seconds }
        guard ordered.count >= 3, duration.isFinite, duration > 0 else { return [] }

        let boundaries = (1..<(ordered.count - 1)).filter { index in
            similarity(ordered[index - 1].text, ordered[index].text) < 0.12
                && similarity(ordered[index].text, ordered[index + 1].text) >= 0.35
        }
        guard !boundaries.isEmpty else { return [] }

        let starts = [0] + boundaries
        let modelVersion = "timeline-chapter-suggestion-v1/" + frameModelVersion
        return starts.enumerated().compactMap { offset, captionIndex in
            let caption = ordered[captionIndex]
            let start = min(caption.seconds, duration)
            let end = offset + 1 < starts.count
                ? max(start, ordered[starts[offset + 1]].seconds - 0.001)
                : duration
            let summary = String(caption.text.prefix(4_000)).trimmingCharacters(in: .whitespacesAndNewlines)
            let title = conciseTitle(summary)
            guard !title.isEmpty else { return nil }
            let id = "timeline-suggestion:\(fileID):\(Int(start * 1_000)):\(sourceRevision.prefix(32))"
            return CatalogChapter(
                id: id,
                fileID: fileID,
                startSeconds: start,
                endSeconds: max(start, end),
                title: title,
                summary: summary,
                sourceRevision: sourceRevision,
                modelVersion: modelVersion,
                confidence: 0.2,
                userEdited: false,
                stale: false
            )
        }
    }

    private static func similarity(_ lhs: String, _ rhs: String) -> Double {
        let stopWords: Set<String> = [
            "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "into", "is", "it",
            "of", "on", "or", "the", "to", "with", "this", "that", "there", "their", "they", "while"
        ]
        func words(_ text: String) -> Set<String> {
            Set(text.lowercased().split { !$0.isLetter && !$0.isNumber }.map(String.init).filter { $0.count > 1 && !stopWords.contains($0) })
        }
        let left = words(lhs)
        let right = words(rhs)
        let union = left.union(right)
        guard !union.isEmpty else { return 0 }
        return Double(left.intersection(right).count) / Double(union.count)
    }

    private static func conciseTitle(_ text: String) -> String {
        let firstSentence = text.split(whereSeparator: { ".!?\n".contains($0) }).first.map(String.init) ?? text
        let trimmed = firstSentence.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count > 88 else { return trimmed }
        let shortened = trimmed.prefix(88)
        if let boundary = shortened.lastIndex(where: \.isWhitespace) {
            return String(shortened[..<boundary]) + "…"
        }
        return String(shortened) + "…"
    }
}
