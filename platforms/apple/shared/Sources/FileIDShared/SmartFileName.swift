import Foundation

public enum SmartFileName {
    public enum Style: String, Codable, Sendable { case readable, slug }

    public static func stem(_ raw: String, confirmedSubjects: [String] = [], originalStem: String? = nil, style: Style = .readable) -> String? {
        var words = raw.split { $0.isWhitespace || $0 == "-" || $0 == "_" }.map(String.init)
        for prefix in [["a", "photo", "of"], ["photo", "of"], ["an", "image", "of"], ["image", "of"], ["a", "video", "of"], ["video", "of"]] {
            if words.prefix(prefix.count).map({ $0.lowercased() }) == prefix { words.removeFirst(prefix.count); break }
        }
        let subjects = Array(Set(confirmedSubjects.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty })).sorted()
        let subjectPrefixes: [[String]] = subjects.flatMap { subject -> [[String]] in
            let full = subject.split { $0.isWhitespace || $0 == "-" || $0 == "_" }.map { $0.lowercased() }
            guard let first = full.first else { return [] }
            return [full, [first]]
        }
        func subjectPrefixLength(_ candidate: ArraySlice<String>) -> Int? {
            subjectPrefixes.filter { prefix in
                candidate.prefix(prefix.count).map { $0.lowercased() } == prefix
            }.map(\.count).max()
        }
        while let count = subjectPrefixLength(words[...]) {
            words.removeFirst(count)
            if let first = words.first, ["and", "&"].contains(first.lowercased()),
               subjectPrefixLength(words.dropFirst()) != nil {
                words.removeFirst()
            }
        }
        let generic = Set(["untitled", "filename", "file", "photo", "picture", "image", "video", "document"])
        guard !words.isEmpty, words.contains(where: { !generic.contains($0.lowercased()) }) else { return nil }
        let event = words.map { word in word.prefix(1).uppercased() + word.dropFirst().lowercased() }.joined(separator: " ")
        let prefix = subjects.count <= 2 ? subjects.joined(separator: " & ") : ""
        var result = prefix.isEmpty ? event : prefix + " - " + event
        if style == .slug {
            result = result.lowercased().split { !$0.isLetter && !$0.isNumber }.joined(separator: "-")
        }
        result = FilesystemNameSafe.componentSafe(result, maxLength: 60)
        while result.utf8.count > 200 { result.removeLast() }
        result = result.trimmingCharacters(in: CharacterSet(charactersIn: " -_"))
        guard !result.isEmpty, result != "_" else { return nil }
        if let originalStem, normalized(result) == normalized(originalStem) { return nil }
        return result
    }

    private static func normalized(_ value: String) -> String {
        value.lowercased().filter { $0.isLetter || $0.isNumber }
    }
}
