import Foundation

struct ChatSearchPlan: Sendable, Equatable {
    struct KnownPerson: Sendable, Equatable {
        let id: Int64
        let names: [String]
    }

    var query: String
    var kinds: [String]
    var personIDs: [Int64] = []
    var personNames: [String] = []

    static func resolve(
        _ text: String,
        previous: ChatSearchPlan? = nil,
        knownPeople: [KnownPerson] = []
    ) -> ChatSearchPlan {
        let words = text.split(whereSeparator: \.isWhitespace).map(String.init)
        let normalized = words.map { $0.lowercased().trimmingCharacters(in: .alphanumerics.inverted) }
        let negative = normalized.contains { ["not", "except", "without", "excluding"].contains($0) }
        let matchedPeople = negative ? (ids: Set<Int64>(), names: Set<String>(), indices: Set<Int>()) : matchPeople(in: normalized, knownPeople: knownPeople)
        let aliases: [String: [String]] = ["video": ["video"], "videos": ["video"], "clip": ["video"], "clips": ["video"], "photo": ["image"], "photos": ["image"], "picture": ["image"], "pictures": ["image"], "image": ["image"], "images": ["image"], "document": ["doc", "pdf"], "documents": ["doc", "pdf"], "pdf": ["pdf"], "pdfs": ["pdf"], "audio": ["audio"]]
        let fillers: Set<String> = ["find", "show", "me", "please", "search", "for", "the", "a", "an", "where", "of", "my", "files", "file", "with", "in", "all", "only", "just", "now", "these", "those", "instead", "also", "and"]
        let refinement = normalized.contains { ["only", "just", "now", "these", "those", "instead", "also", "and"].contains($0) }
        let kinds = negative ? [] : Array(Set(normalized.flatMap { aliases[$0] ?? [] })).sorted()
        var query = zip(words.indices, zip(words, normalized))
            .filter { !matchedPeople.indices.contains($0.0) && !fillers.contains($0.1.1) && (negative || aliases[$0.1.1] == nil) }
            .map { $0.1.0 }
            .joined(separator: " ")
        if refinement, !normalized.contains("all"), let previous {
            if query.isEmpty { query = previous.query }
            else if normalized.contains("also") || normalized.first == "and" {
                query = previous.query.isEmpty ? query : previous.query + " " + query
            }
        }

        var personIDs = matchedPeople.ids
        var personNames = matchedPeople.names
        if !matchedPeople.ids.isEmpty,
           refinement,
           normalized.contains("also") || normalized.first == "and",
           let previous {
            personIDs.formUnion(previous.personIDs)
            personNames.formUnion(previous.personNames)
        } else if matchedPeople.ids.isEmpty, refinement, let previous {
            personIDs = Set(previous.personIDs)
            personNames = Set(previous.personNames)
        }

        return ChatSearchPlan(
            query: String(query.prefix(2000)),
            kinds: kinds.isEmpty && !negative && refinement && !normalized.contains("all") ? previous?.kinds ?? [] : kinds,
            personIDs: personIDs.sorted(),
            personNames: personNames.sorted()
        )
    }

    private static func matchPeople(
        in words: [String],
        knownPeople: [KnownPerson]
    ) -> (ids: Set<Int64>, names: Set<String>, indices: Set<Int>) {
        let candidates = knownPeople.flatMap { person in
            person.names.compactMap { name -> (Int64, String, [String])? in
                let tokens = name.split(whereSeparator: \.isWhitespace)
                    .map { $0.lowercased().trimmingCharacters(in: .alphanumerics.inverted) }
                    .filter { !$0.isEmpty }
                guard !tokens.isEmpty else { return nil }
                return (person.id, name.trimmingCharacters(in: .whitespacesAndNewlines), tokens)
            }
        }

        var ids = Set<Int64>()
        var names = Set<String>()
        var consumed = Set<Int>()
        var index = 0
        while index < words.count {
            guard !consumed.contains(index) else { index += 1; continue }
            let matches = candidates.filter { _, _, tokens in
                index + tokens.count <= words.count
                    && Array(words[index..<(index + tokens.count)]) == tokens
            }
            guard let length = matches.map({ $0.2.count }).max() else { index += 1; continue }
            for (id, name, tokens) in matches where tokens.count == length {
                ids.insert(id)
                names.insert(name)
            }
            consumed.formUnion(index..<(index + length))
            index += length
        }
        return (ids, names, consumed)
    }
}
