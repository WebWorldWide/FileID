import FileIDShared

enum FileKindLabel {
    /// Display name for a catalog `files.kind` value ("pdf" reads as "PDF", not "Pdf").
    static func title(_ kind: String) -> String {
        kind.lowercased() == "pdf" ? "PDF" : kind.capitalized
    }
}

enum TagLabel {
    /// Chip text for a catalog tag: the last segment of an engine label ("Year_2026" → "2026",
    /// "Camera_Sony" → "Sony"), dashes as spaces, first letter capitalized. Phrases pass through.
    static func text(_ tag: String) -> String {
        if tag.contains(" ") { return tag }
        let last = (tag.split(separator: "_").last.map(String.init) ?? tag).replacingOccurrences(of: "-", with: " ")
        guard let first = last.first else { return last }
        return first.uppercased() + last.dropFirst()
    }

    /// Distinct chip texts, first occurrence wins (case-insensitive).
    static func unique(_ tags: [String]) -> [String] {
        var seen = Set<String>()
        return tags.map(text).filter { seen.insert($0.lowercased()).inserted }
    }
}

enum PathText {
    /// A path for display: the home folder reads as "~" so the account name doesn't fill
    /// every row (or every screenshot). The full path stays in tooltips and Finder.
    static func abbreviated(_ path: String) -> String {
        let home = UserHome.path
        guard !home.isEmpty, path == home || path.hasPrefix(home + "/") else { return path }
        return "~" + path.dropFirst(home.count)
    }
}
