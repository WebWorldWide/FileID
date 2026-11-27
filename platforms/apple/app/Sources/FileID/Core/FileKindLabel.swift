import FileIDShared

enum FileKindLabel {
    /// Display name for a catalog `files.kind` value ("pdf" reads as "PDF", not "Pdf").
    static func title(_ kind: String) -> String {
        kind.lowercased() == "pdf" ? "PDF" : kind.capitalized
    }

    /// Kind and extension for a details row ("Image · JPG"); a PDF is not shown as "PDF · PDF".
    static func detail(kind: String, fileExtension: String) -> String {
        let title = title(kind)
        let ext = fileExtension.uppercased()
        return ext.isEmpty || ext == title.uppercased() ? title : title + " · " + ext
    }
}

enum VideoPreviewNotice {
    /// Shown for a video that has no thumbnail: macOS can't decode its format, and a visual search only finds what FileID could see.
    static let text = "No preview: macOS can’t decode this kind of video, so FileID can’t show a frame from it or find it by what’s in the picture. Its name, date and folder still work in search."
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

    /// The chips under a grid tile: distinct, two at most, without one that only repeats the tile's kind badge.
    static func tileChips(_ tags: [String], kind: String) -> [String] {
        let kindName = FileKindLabel.title(kind).lowercased()
        return Array(unique(tags).filter { $0.lowercased() != kindName }.prefix(2))
    }

    /// Distinct chip texts, first occurrence wins (case-insensitive).
    static func unique(_ tags: [String]) -> [String] {
        var seen = Set<String>()
        return tags.map(text).filter { seen.insert($0.lowercased()).inserted }
    }
}

enum PathText {
    /// The last `parts` folder names of a folder path, the part that tells two copies apart
    /// ("Lisbon 2022/Day 1"), where a middle-truncated full path would cut it off.
    static func tail(_ folder: String, parts: Int = 2) -> String {
        let names = folder.split(separator: "/").map(String.init)
        guard names.count > parts else { return folder == "/" ? "/" : names.joined(separator: "/") }
        return names.suffix(parts).joined(separator: "/")
    }

    /// The ways to show a path in less room, longest first, each cut at a folder boundary:
    /// the whole path, then "…/" plus its last three, two and one folders.
    static func shortenings(_ path: String) -> [String] {
        let names = path.split(separator: "/").map(String.init)
        var options = [path]
        for keep in [3, 2, 1] where names.count > keep {
            options.append("…/" + names.suffix(keep).joined(separator: "/"))
        }
        return options
    }

    /// A path for display: the home folder reads as "~" so the account name doesn't fill
    /// every row (or every screenshot). The full path stays in tooltips and Finder.
    static func abbreviated(_ path: String) -> String {
        let home = UserHome.path
        guard !home.isEmpty, path == home || path.hasPrefix(home + "/") else { return path }
        return "~" + path.dropFirst(home.count)
    }
}
