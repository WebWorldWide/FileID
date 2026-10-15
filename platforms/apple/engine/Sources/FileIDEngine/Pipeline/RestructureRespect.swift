import Foundation

/// Final Restructure stage: keep what the user already organized.
///
/// The grouping passes propose moves by visual and textual similarity. On a
/// library that is already organized by event (e.g. "Kyle Baseball Game
/// 9-19-26/Photos/RAWs") that shuffled thousands of files between events and
/// formats. This stage enforces the rules a person organizing by hand follows:
///
/// 1. A file inside a named, non-generic folder stays where it is.
/// 2. Nothing moves from one top-level event folder into another.
/// 3. Nothing moves into a folder for a different format (a JPEG never goes
///    into "RAW", a photo never into "Videos").
/// 4. A new folder name never comes from a camera filename ("DSC01548") or a
///    raw tag identifier ("Consumer_electronics").
///
/// Loose files (library root or generic folders such as "Downloads") are still
/// organized, and one whose capture time falls inside an existing event's span
/// is filed into that event, in its matching format subfolder when one exists.
enum RestructureRespect {
    struct FileInfo: Sendable {
        let fileID: Int64
        let path: String
        let kind: String
        let captureUnix: Double?
    }

    enum FormatRole: Equatable { case raw, jpeg, video, photo }

    static let rawExtensions: Set<String> = ["arw", "cr2", "cr3", "crw", "nef", "nrw", "dng", "raf", "orf", "rw2", "pef", "srw", "x3f", "3fr", "iiq", "rwl", "erf", "mef", "mos", "srf", "sr2"]
    static let jpegExtensions: Set<String> = ["jpg", "jpeg", "jpe", "heic", "heif", "png", "tif", "tiff", "webp"]

    private static let genericFolderNames: Set<String> = [
        "downloads", "downloaded", "new folder", "untitled folder", "untitled", "temp", "tmp",
        "misc", "miscellaneous", "other", "others", "stuff", "things", "files", "unsorted",
        "desktop", "dump", "inbox", "to sort", "to-sort", "import", "imports", "camera uploads",
        "dcim", "export", "exports", "screenshots", "screen shots"
    ]

    /// Generic or camera-generated folder names ("DCIM", "100MSDCF", "New Folder 3").
    static func isGenericFolderName(_ raw: String) -> Bool {
        let name = raw.lowercased().trimmingCharacters(in: .whitespaces)
        if name.isEmpty || genericFolderNames.contains(name) { return true }
        if name.range(of: #"^(new folder|untitled folder|untitled)( \d+)?$"#, options: .regularExpression) != nil { return true }
        if name.range(of: #"^\d{3}[a-z_]{3,5}$"#, options: .regularExpression) != nil { return true }
        return false
    }

    static func formatRole(ofFolderName raw: String) -> FormatRole? {
        let tokens = Set(raw.lowercased().split(whereSeparator: { !$0.isLetter }).map(String.init))
        if tokens.contains("raw") || tokens.contains("raws") { return .raw }
        if tokens.contains("jpeg") || tokens.contains("jpegs") || tokens.contains("jpg") || tokens.contains("jpgs") { return .jpeg }
        if !tokens.isDisjoint(with: ["video", "videos", "movies", "clips", "footage"]) { return .video }
        if !tokens.isDisjoint(with: ["photo", "photos", "pictures", "images", "stills"]) { return .photo }
        return nil
    }

    static func formatRole(ofFile path: String, kind: String) -> FormatRole {
        let ext = (path as NSString).pathExtension.lowercased()
        if rawExtensions.contains(ext) { return .raw }
        if kind == "video" { return .video }
        if jpegExtensions.contains(ext) { return .jpeg }
        return .photo
    }

    /// Whether a file of `fileRole` belongs in a folder of `folderRole`.
    static func fits(_ fileRole: FormatRole, in folderRole: FormatRole?) -> Bool {
        switch folderRole {
        case nil: return true
        case .raw: return fileRole == .raw
        case .jpeg: return fileRole == .jpeg
        case .video: return fileRole == .video
        case .photo: return fileRole != .video
        }
    }

    /// Folder names under the library root, outermost first.
    static func components(of path: String, under root: String) -> [String] {
        let rootPrefix = root.hasSuffix("/") ? root : root + "/"
        guard path.hasPrefix(rootPrefix) else { return [] }
        let relative = String(path.dropFirst(rootPrefix.count))
        return Array(relative.split(separator: "/").dropLast().map(String.init))
    }

    /// True when the file already sits in a folder the user named on purpose.
    static func isOrganized(_ path: String, under root: String) -> Bool {
        let folders = components(of: path, under: root)
        guard !folders.isEmpty else { return false }
        return folders.contains { !isGenericFolderName($0) }
    }

    /// The top-level folder under the root that a path belongs to.
    static func eventRoot(of path: String, under root: String) -> String? {
        components(of: path, under: root).first
    }

    /// A camera filename stem or a raw tag identifier is not a folder name.
    static func isUsableFolderName(_ name: String) -> Bool {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        guard trimmed.count >= 3 else { return false }
        if trimmed.contains("_") { return false }
        let lower = trimmed.lowercased()
        if lower.range(of: #"\b(dsc|dscf|dscn|img|imgp|pxl|mvi|gopr|gp|dji|p\d{3}|_mg|mg)[_-]?\d{3,}"#, options: .regularExpression) != nil { return false }
        if lower.range(of: #"\b[a-z]{0,4}\d{4,}\b"#, options: .regularExpression) != nil { return false }
        return true
    }

    // MARK: - Filter

    static func apply(
        _ proposals: [RestructureProposal], files: [Int64: FileInfo], libraryRoot: String
    ) -> (kept: [RestructureProposal], dropped: Int) {
        var kept: [RestructureProposal] = []
        kept.reserveCapacity(proposals.count)
        var dropped = 0
        for proposal in proposals {
            if keep(proposal, file: files[proposal.fileID], root: libraryRoot) {
                kept.append(proposal)
            } else {
                dropped += 1
            }
        }
        return (kept, dropped)
    }

    static func keep(_ p: RestructureProposal, file: FileInfo?, root: String) -> Bool {
        if isOrganized(p.oldPath, under: root) {
            return isFormatCorrection(p, file: file, root: root)
        }
        let destinationFolders = components(of: p.newPath, under: root)
        let role = formatRole(ofFile: p.oldPath, kind: file?.kind ?? "")
        if let last = destinationFolders.last, !fits(role, in: formatRole(ofFolderName: last)) { return false }
        let existing = Set(components(of: p.oldPath, under: root))
        for folder in destinationFolders where !existing.contains(folder) && !isUsableFolderName(folder) && formatRole(ofFolderName: folder) == nil {
            if folder.range(of: #"^\d{4}$"#, options: .regularExpression) != nil { continue }
            if DateFormatter().monthSymbols.contains(folder) { continue }
            return false
        }
        return true
    }

    /// Within one event: a RAW in a JPEG folder (or the reverse) moving to the
    /// sibling folder for its format.
    static func isFormatCorrection(_ p: RestructureProposal, file: FileInfo?, root: String) -> Bool {
        let from = components(of: p.oldPath, under: root)
        let to = components(of: p.newPath, under: root)
        guard from.count == to.count, from.count >= 2, from.dropLast() == to.dropLast(),
              let source = from.last, let destination = to.last else { return false }
        let role = formatRole(ofFile: p.oldPath, kind: file?.kind ?? "")
        return !fits(role, in: formatRole(ofFolderName: source)) && fits(role, in: formatRole(ofFolderName: destination))
            && formatRole(ofFolderName: destination) != nil
    }

    // MARK: - Proposals the passes cannot make

    /// Misfiled formats inside an event folder: a RAW in "JPEGs" goes to the
    /// sibling "RAWs" (and so on) when that sibling exists.
    static func formatCorrections(files: [FileInfo], root: String) -> [RestructureProposal] {
        var siblingsByParent: [String: [(name: String, role: FormatRole)]] = [:]
        for file in files {
            let folder = (file.path as NSString).deletingLastPathComponent
            let parent = (folder as NSString).deletingLastPathComponent
            let name = (folder as NSString).lastPathComponent
            guard let role = formatRole(ofFolderName: name), role != .photo else { continue }
            if !(siblingsByParent[parent]?.contains { $0.name == name } ?? false) {
                siblingsByParent[parent, default: []].append((name, role))
            }
        }
        var out: [RestructureProposal] = []
        for file in files where isOrganized(file.path, under: root) {
            let folder = (file.path as NSString).deletingLastPathComponent
            let parent = (folder as NSString).deletingLastPathComponent
            let role = formatRole(ofFile: file.path, kind: file.kind)
            guard let folderRole = formatRole(ofFolderName: (folder as NSString).lastPathComponent), folderRole != .photo,
                  !fits(role, in: folderRole),
                  let target = siblingsByParent[parent]?.sorted(by: { $0.name < $1.name }).first(where: { fits(role, in: $0.role) })
            else { continue }
            let destination = ((parent as NSString).appendingPathComponent(target.name) as NSString)
                .appendingPathComponent((file.path as NSString).lastPathComponent)
            out.append(RestructureProposal(
                fileID: file.fileID, oldPath: file.path, newPath: destination, bucket: target.name,
                confidence: "auto", reason: "A \(role == .raw ? "RAW" : role == .video ? "video" : "JPEG") file in a \(folderRole == .raw ? "RAW" : folderRole == .video ? "video" : "JPEG") folder; moved to its sibling \(target.name) folder."))
        }
        return out
    }

    struct EventSpan: Sendable {
        let folder: String
        let start: Double
        let end: Double
        let formatFolders: [(path: String, role: FormatRole)]
    }

    /// Capture-time span of each top-level event folder (10th–90th percentile,
    /// so a stray old file does not stretch it), with its format subfolders.
    static func eventSpans(files: [FileInfo], root: String) -> [EventSpan] {
        var times: [String: [Double]] = [:]
        var formatFolders: [String: Set<String>] = [:]
        let rootPrefix = root.hasSuffix("/") ? root : root + "/"
        for file in files {
            let folders = components(of: file.path, under: root)
            guard let top = folders.first, !isGenericFolderName(top) else { continue }
            if let t = file.captureUnix, t > 0 { times[top, default: []].append(t) }
            let folder = (file.path as NSString).deletingLastPathComponent
            if let name = folders.last, formatRole(ofFolderName: name) != nil {
                formatFolders[top, default: []].insert(folder)
            }
        }
        return times.compactMap { top, values in
            guard values.count >= 5 else { return nil }
            let sorted = values.sorted()
            let lo = sorted[Int(Double(sorted.count - 1) * 0.1)]
            let hi = sorted[Int(Double(sorted.count - 1) * 0.9)]
            guard hi - lo <= 4 * 86_400 else { return nil }
            let folders = (formatFolders[top] ?? []).sorted().compactMap { path -> (path: String, role: FormatRole)? in
                formatRole(ofFolderName: (path as NSString).lastPathComponent).map { (path, $0) }
            }
            return EventSpan(folder: rootPrefix + top, start: lo, end: hi, formatFolders: folders)
        }.sorted { $0.start < $1.start }
    }

    /// The event folder a loose file was captured during (±3 hours), and the
    /// best subfolder for its format.
    static func eventDestination(for file: FileInfo, spans: [EventSpan]) -> String? {
        guard let t = file.captureUnix, t > 0 else { return nil }
        let slack = 3.0 * 3_600
        let matches = spans.filter { t >= $0.start - slack && t <= $0.end + slack }
        guard matches.count == 1, let span = matches.first else { return nil }
        let role = formatRole(ofFile: file.path, kind: file.kind)
        let exact = span.formatFolders.filter { $0.role == role }
        let general = span.formatFolders.filter { $0.role == .photo && role != .video }
        let folder = (exact.count == 1 ? exact.first?.path : nil) ?? (general.count == 1 ? general.first?.path : nil) ?? span.folder
        return (folder as NSString).appendingPathComponent((file.path as NSString).lastPathComponent)
    }
}
