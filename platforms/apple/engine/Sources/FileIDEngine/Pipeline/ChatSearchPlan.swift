import Foundation

struct ChatSearchPlan: Sendable, Equatable {
    struct KnownPerson: Sendable, Equatable {
        let id: Int64
        let names: [String]
    }

    struct KnownEvent: Sendable, Equatable {
        let id: String
        let names: [String]
    }

    var query: String
    var kinds: [String]
    var personIDs: [Int64] = []
    var personNames: [String] = []
    var eventIDs: [String] = []
    var eventNames: [String] = []
    var timeSeconds: Double?

    static func resolve(
        _ text: String,
        previous: ChatSearchPlan? = nil,
        knownPeople: [KnownPerson] = [],
        knownEvents: [KnownEvent] = []
    ) -> ChatSearchPlan {
        let words = text.split(whereSeparator: \.isWhitespace).map(String.init)
        let normalized = words.map(normalizeToken)
        let negative = normalized.contains { ["not", "except", "without", "excluding"].contains($0) }
        let matchedPeople = negative
            ? (ids: Set<Int64>(), names: Set<String>(), indices: Set<Int>())
            : matchPeople(in: normalized, knownPeople: knownPeople)
        let matchedEvents = negative
            ? (ids: Set<String>(), names: Set<String>(), indices: Set<Int>())
            : matchEvents(in: normalized, knownEvents: knownEvents)
        let matchedTime = negative ? (seconds: nil, indices: Set<Int>()) : matchTime(in: normalized)

        let aliases: [String: [String]] = [
            "video": ["video"], "videos": ["video"], "clip": ["video"], "clips": ["video"],
            "photo": ["image"], "photos": ["image"], "picture": ["image"], "pictures": ["image"],
            "image": ["image"], "images": ["image"], "document": ["doc", "pdf"],
            "documents": ["doc", "pdf"], "pdf": ["pdf"], "pdfs": ["pdf"], "audio": ["audio"]
        ]
        let fillers: Set<String> = [
            "find", "show", "me", "please", "search", "for", "from", "the", "a", "an", "where", "of",
            "my", "files", "file", "with", "in", "at", "around", "near", "all", "only", "just", "now",
            "these", "those", "instead", "also", "and"
        ]
        let refinement = normalized.contains {
            ["only", "just", "now", "these", "those", "instead", "also", "and"].contains($0)
        }
        let kinds = negative ? [] : Array(Set(normalized.flatMap { aliases[$0] ?? [] })).sorted()
        var query = zip(words.indices, zip(words, normalized))
            .filter {
                !matchedPeople.indices.contains($0.0)
                    && !matchedEvents.indices.contains($0.0)
                    && !matchedTime.indices.contains($0.0)
                    && (negative || aliases[$0.1.1] == nil)
                    && !fillers.contains($0.1.1)
            }
            .map { $0.1.0 }
            .joined(separator: " ")
        if let previous, refinement, !normalized.contains("all") {
            if query.isEmpty {
                query = previous.query
            } else if normalized.contains("also") || normalized.contains("and") {
                query = [previous.query, query].filter { !$0.isEmpty }.joined(separator: " ")
            }
        }

        var personIDs = matchedPeople.ids
        var personNames = matchedPeople.names
        if personIDs.isEmpty, refinement, let previous {
            personIDs = Set(previous.personIDs)
            personNames = Set(previous.personNames)
        } else if !personIDs.isEmpty, normalized.contains("also"), let previous {
            personIDs.formUnion(previous.personIDs)
            personNames.formUnion(previous.personNames)
        }

        var eventIDs = matchedEvents.ids
        var eventNames = matchedEvents.names
        if eventIDs.isEmpty, refinement, let previous {
            eventIDs = Set(previous.eventIDs)
            eventNames = Set(previous.eventNames)
        } else if !eventIDs.isEmpty, normalized.contains("also"), let previous {
            eventIDs.formUnion(previous.eventIDs)
            eventNames.formUnion(previous.eventNames)
        }

        return ChatSearchPlan(
            query: String(query.prefix(2000)),
            kinds: kinds.isEmpty && !negative && refinement && !normalized.contains("all") ? previous?.kinds ?? [] : kinds,
            personIDs: personIDs.sorted(),
            personNames: personNames.sorted(),
            eventIDs: eventIDs.sorted(),
            eventNames: eventNames.sorted(),
            timeSeconds: matchedTime.seconds ?? (refinement ? previous?.timeSeconds : nil)
        )
    }

    private static func matchPeople(
        in words: [String],
        knownPeople: [KnownPerson]
    ) -> (ids: Set<Int64>, names: Set<String>, indices: Set<Int>) {
        let candidates = knownPeople.flatMap { person in
            person.names.compactMap { name -> (Int64, String, [String])? in
                let tokens = name.split(whereSeparator: \.isWhitespace)
                    .map { normalizeToken(String($0)) }
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
                index + tokens.count <= words.count && Array(words[index..<(index + tokens.count)]) == tokens
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

    private static func matchEvents(
        in words: [String],
        knownEvents: [KnownEvent]
    ) -> (ids: Set<String>, names: Set<String>, indices: Set<Int>) {
        let candidates = knownEvents.flatMap { event in
            event.names.compactMap { name -> (String, String, [String])? in
                let tokens = name.split(whereSeparator: \.isWhitespace)
                    .map { normalizeToken(String($0)) }
                    .filter { !$0.isEmpty }
                guard !tokens.isEmpty else { return nil }
                return (event.id, name.trimmingCharacters(in: .whitespacesAndNewlines), tokens)
            }
        }
        var ids = Set<String>()
        var names = Set<String>()
        var consumed = Set<Int>()
        var index = 0
        while index < words.count {
            guard !consumed.contains(index) else { index += 1; continue }
            let matches = candidates.filter { _, _, tokens in
                index + tokens.count <= words.count && Array(words[index..<(index + tokens.count)]) == tokens
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

    private static func matchTime(in words: [String]) -> (seconds: Double?, indices: Set<Int>) {
        for index in words.indices where ["at", "around", "near"].contains(words[index]) {
            guard index + 1 < words.count, let seconds = parseTimestamp(words[index + 1]) else { continue }
            return (seconds, [index, index + 1])
        }
        return (nil, [])
    }

    private static func parseTimestamp(_ value: String) -> Double? {
        let parts = value.split(separator: ":", omittingEmptySubsequences: false)
        guard (2...3).contains(parts.count), parts.allSatisfy({ !$0.isEmpty && $0.allSatisfy(\.isNumber) }) else {
            return nil
        }
        var numbers: [Int] = []
        for part in parts {
            guard let number = Int(part) else { return nil }
            numbers.append(number)
        }
        guard numbers[0] <= 1_000_000 else { return nil }

        if numbers.count == 2 {
            guard numbers[1] < 60 else { return nil }
            return Double(numbers[0] * 60 + numbers[1])
        }
        guard numbers[1] < 60, numbers[2] < 60 else { return nil }
        return Double(numbers[0] * 3600 + numbers[1] * 60 + numbers[2])
    }

    private static func normalizeToken(_ value: String) -> String {
        value.lowercased()
            .replacingOccurrences(of: "’s", with: "")
            .replacingOccurrences(of: "'s", with: "")
            .replacingOccurrences(of: "’", with: "")
            .replacingOccurrences(of: "'", with: "")
            .trimmingCharacters(in: .alphanumerics.inverted)
    }
}
