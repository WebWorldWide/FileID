import Foundation

struct ChatSearchPlan: Sendable, Equatable {
    var query: String
    var kinds: [String]

    static func resolve(_ text: String, previous: ChatSearchPlan? = nil) -> ChatSearchPlan {
        let words = text.split(whereSeparator: \.isWhitespace).map(String.init)
        let normalized = words.map { $0.lowercased().trimmingCharacters(in: .alphanumerics.inverted) }
        let negative = normalized.contains { ["not", "except", "without", "excluding"].contains($0) }
        let aliases: [String: [String]] = ["video": ["video"], "videos": ["video"], "clip": ["video"], "clips": ["video"], "photo": ["image"], "photos": ["image"], "picture": ["image"], "pictures": ["image"], "image": ["image"], "images": ["image"], "document": ["doc", "pdf"], "documents": ["doc", "pdf"], "pdf": ["pdf"], "pdfs": ["pdf"], "audio": ["audio"]]
        let fillers: Set<String> = ["find", "show", "me", "please", "search", "for", "the", "a", "an", "where", "of", "my", "files", "file", "with", "in", "all", "only", "just", "now", "these", "those", "instead", "also", "and"]
        let refinement = normalized.contains { ["only", "just", "now", "these", "those", "instead", "also", "and"].contains($0) }
        let kinds = negative ? [] : Array(Set(normalized.flatMap { aliases[$0] ?? [] })).sorted()
        var query = zip(words, normalized).filter { !fillers.contains($0.1) && (negative || aliases[$0.1] == nil) }.map(\.0).joined(separator: " ")
        if refinement, !normalized.contains("all"), let previous {
            if query.isEmpty { query = previous.query }
            else if normalized.contains("also") || normalized.first == "and" {
                query = previous.query.isEmpty ? query : previous.query + " " + query
            }
        }
        return ChatSearchPlan(query: String(query.prefix(2000)), kinds: kinds.isEmpty && !negative && refinement && !normalized.contains("all") ? previous?.kinds ?? [] : kinds)
    }
}
