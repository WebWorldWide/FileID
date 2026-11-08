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
    /// "Kyle and Maya": files with every named person, not any of them.
    var allPeople = false
    /// "without Maya": people whose files are left out.
    var excludedPersonIDs: [Int64] = []
    var excludedPersonNames: [String] = []
    var eventIDs: [String] = []
    var eventNames: [String] = []
    var timeSeconds: Double?

    static func resolve(
        _ text: String,
        previous: ChatSearchPlan? = nil,
        knownPeople: [KnownPerson] = [],
        knownEvents: [KnownEvent] = [],
        followUp: Bool = false
    ) -> ChatSearchPlan {
        let words = text.split(whereSeparator: \.isWhitespace).map(String.init)
        var normalized = words.map(normalizeToken)
        let negations: Set<String> = ["not", "except", "without", "excluding"]
        let excluded = matchExclusions(in: normalized, negations: negations, knownPeople: knownPeople)
        for index in excluded.indices { normalized[index] = "" }
        let negative = normalized.contains { negations.contains($0) }
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
            "these", "those", "instead", "also", "and", "what", "whats", "which", "who", "is", "are", "was", "were",
            "do", "does", "did", "have", "has", "there", "any", "about", "that", "i", "mentioning", "mention", "mentions",
            "named", "called", "titled", "regarding", "ones", "them", "they", "it", "its", "taken",
            "he", "she", "him", "her", "his", "their"
        ]
        // "how many of them show Alex?" and "what is he wearing?" point back at the last search.
        let refinement = followUp || normalized.contains {
            ["only", "just", "now", "these", "those", "instead", "also", "and",
             "them", "they", "ones", "he", "she", "him", "her", "his", "their"].contains($0)
        }
        let kinds = negative ? [] : Array(Set(normalized.flatMap { aliases[$0] ?? [] })).sorted()
        var query = zip(words.indices, zip(words, normalized))
            .filter {
                !matchedPeople.indices.contains($0.0)
                    && !matchedEvents.indices.contains($0.0)
                    && !matchedTime.indices.contains($0.0)
                    && (negative || aliases[$0.1.1] == nil)
                    && !fillers.contains($0.1.1)
                    && !excluded.indices.contains($0.0)
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
        var allPeople = personIDs.count > 1 && !normalized.contains("or")
            && normalized.contains { ["and", "with", "both", "together", "plus"].contains($0) }
        if personIDs.isEmpty, refinement, let previous {
            personIDs = Set(previous.personIDs)
            personNames = Set(previous.personNames)
            allPeople = previous.allPeople
        } else if !personIDs.isEmpty, normalized.contains("also"), let previous {
            personIDs.formUnion(previous.personIDs)
            personNames.formUnion(previous.personNames)
            allPeople = false
        }

        var excludedIDs = excluded.ids
        var excludedNames = excluded.names
        if excludedIDs.isEmpty, refinement, let previous {
            excludedIDs = Set(previous.excludedPersonIDs)
            excludedNames = Set(previous.excludedPersonNames)
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
            allPeople: allPeople,
            excludedPersonIDs: excludedIDs.subtracting(personIDs).sorted(),
            excludedPersonNames: excludedNames.subtracting(personNames).sorted(),
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

    /// People named right after "without"/"except"/"not" (optionally "of"/"the",
    /// joined by "or"/"and"/","), with the words they used, negation included.
    private static func matchExclusions(
        in words: [String], negations: Set<String>, knownPeople: [KnownPerson]
    ) -> (ids: Set<Int64>, names: Set<String>, indices: Set<Int>) {
        var ids = Set<Int64>(), names = Set<String>(), used = Set<Int>()
        for index in words.indices where negations.contains(words[index]) {
            var at = index + 1
            var consumed: Set<Int> = [index]
            var found = false
            while at < words.count {
                while at < words.count, ["of", "the", "any", "or", "and", "nor", ""].contains(words[at]) { consumed.insert(at); at += 1 }
                guard at < words.count else { break }
                let window = Array(words[at..<min(at + 4, words.count)])
                let match = matchPeople(in: window, knownPeople: knownPeople)
                guard match.indices.contains(0) else { break }
                let length = (0..<window.count).prefix { match.indices.contains($0) }.count
                let single = matchPeople(in: Array(window.prefix(length)), knownPeople: knownPeople)
                ids.formUnion(single.ids); names.formUnion(single.names)
                consumed.formUnion(at..<(at + length))
                at += length
                found = true
            }
            if found { used.formUnion(consumed.filter { $0 < at }) }
        }
        return (ids, names, used)
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
