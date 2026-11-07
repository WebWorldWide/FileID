// Smart-name style: the saved settings, the proposeNames request and the
// preview rows' rules. The engine proposes; the app only shows, edits and
// applies through renameFiles.
import Foundation
import FileIDShared

enum NameStyle {
    static let styleKey = "naming.style"
    static let maxLengthKey = "naming.maxLength"
    static let includePeopleKey = "naming.includePeople"
    static let includeEventKey = "naming.includeEvent"
    static let includeDateKey = "naming.includeDate"
    static let learnKey = "naming.learn"

    static let defaultStyle = "short"
    static let defaultMaxLength = 40
    static let defaultIncludePeople = true
    static let defaultIncludeEvent = true
    static let defaultIncludeDate = false
    static let defaultLearn = true

    static let styles: [(id: String, title: String)] = [("short", "Short"), ("descriptive", "Descriptive"), ("dateFirst", "Date first")]
    static let lengthRange = 4...120
    static let maxFiles = 1_000
    static let previewDelay: Duration = .milliseconds(300)

    static func dateToggleEnabled(style: String) -> Bool { style != "dateFirst" }

    static func naming(style: String, maxLength: Int, includePeople: Bool, includeEvent: Bool, includeDate: Bool, learn: Bool = false) -> CatalogNaming {
        let style = styles.contains { $0.id == style } ? style : defaultStyle
        return CatalogNaming(style: style,
                             maxLength: min(max(maxLength, lengthRange.lowerBound), lengthRange.upperBound),
                             includePeople: includePeople,
                             includeEvent: includeEvent,
                             includeDate: includeDate && dateToggleEnabled(style: style),
                             learn: learn)
    }

    /// The Library selection when there is one, otherwise `fallback`; ids are
    /// deduplicated and capped at the engine's 1,000.
    static func fileIDs(selection: [Int64], fallback: [Int64]) -> [Int64] {
        var seen = Set<Int64>()
        return Array((selection.isEmpty ? fallback : selection).filter { $0 > 0 && seen.insert($0).inserted }.prefix(maxFiles))
    }

    static func request(id: String, fileIDs: [Int64], naming: CatalogNaming) -> CatalogRequest {
        CatalogRequest(requestID: id, action: "proposeNames", fileIDs: fileIDs, naming: naming)
    }

    static func reasonLabel(_ reason: String?) -> String {
        switch reason {
        case "chapter": "From your chapter"
        case "proposedName": "From the AI name"
        case "description": "From the description"
        case "tags": "From tags"
        case "event": "From your event"
        case "unchanged": "Already well named"
        case "budget": "Too long for the length limit"
        case "reserved": "Not allowed as a file name"
        case "collision": "No free name in this folder"
        case SmartNameCompanions.unscanned: "Its partner file isn't scanned yet"
        default: "Nothing to name it from yet"
        }
    }

    struct Row: Identifiable, Equatable, Sendable {
        let fileID: Int64
        let path: String
        let proposed: String
        let reason: String
        var edited: String
        var checked: Bool
        var group: Int64? = nil
        var suffix: String = ""
        var learned: [String] = []
        var id: Int64 { fileID }
        var currentName: String { (path as NSString).lastPathComponent }
        var canCheck: Bool { !proposed.isEmpty }
        var groupKey: Int64 { group ?? fileID }
        var isLeader: Bool { group == fileID }
        var isFollower: Bool { group.map { $0 != fileID } ?? false }
    }

    /// Rows for a reply, companions kept together leader first. A file (or
    /// group) the user unticked in the last preview stays unticked; rows
    /// without a proposed name can't be ticked.
    static func rows(from hits: [CatalogHit], previous: [Row]) -> [Row] {
        let unticked = Set(previous.filter { $0.canCheck && !$0.checked }.map(\.groupKey))
        var rows = hits.filter { $0.kind == "proposedName" }.map { hit in
            let (tagged, group) = SmartNameCompanions.parse(hit.evidenceID)
            let (reason, learned) = SmartNameLearning.splitReason(tagged)
            return Row(fileID: hit.fileID, path: hit.path, proposed: hit.text, reason: reason,
                       edited: hit.text, checked: !hit.text.isEmpty && !unticked.contains(group ?? hit.fileID), group: group,
                       learned: learned)
        }
        let leaderStems = Dictionary(rows.filter(\.isLeader).map { ($0.fileID, stem($0.proposed)) }, uniquingKeysWith: { first, _ in first })
        for index in rows.indices {
            guard let group = rows[index].group, let leaderStem = leaderStems[group] else { continue }
            let proposed = rows[index].proposed.unicodeScalars
            rows[index].suffix = proposed.starts(with: leaderStem.unicodeScalars)
                ? String(String.UnicodeScalarView(proposed.dropFirst(leaderStem.unicodeScalars.count))) : ""
        }
        return rows
    }

    static func stem(_ name: String) -> String { SmartNameStyle.splitName(name).stem }

    /// Ticking any member of a companion group ticks the whole group.
    static func setChecked(_ rows: inout [Row], fileID: Int64, _ checked: Bool) {
        guard let key = rows.first(where: { $0.fileID == fileID })?.groupKey else { return }
        for index in rows.indices where rows[index].groupKey == key && rows[index].canCheck { rows[index].checked = checked }
    }

    /// Editing the leader's name gives every companion the same stem.
    static func edit(_ rows: inout [Row], fileID: Int64, _ text: String) {
        guard let at = rows.firstIndex(where: { $0.fileID == fileID }), !rows[at].isFollower else { return }
        rows[at].edited = text
        guard rows[at].isLeader else { return }
        let newStem = stem(text.trimmingCharacters(in: .whitespacesAndNewlines))
        for index in rows.indices where rows[index].isFollower && rows[index].group == fileID && rows[index].canCheck {
            rows[index].edited = newStem + rows[index].suffix
        }
    }

    /// The leader's chip naming its companions' extensions: "+ ARW, XMP".
    static func companionChip(_ row: Row, in rows: [Row]) -> String? {
        guard row.isLeader else { return nil }
        let extensions = rows.filter { $0.isFollower && $0.group == row.fileID }
            .map { SmartNameStyle.splitName($0.currentName).ext.uppercased() }
        return extensions.isEmpty ? nil : "+ " + extensions.joined(separator: ", ")
    }

    static func validName(_ text: String) -> String? {
        let name = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, name != ".", name != "..", !name.contains("/"), !name.contains("\0") else { return nil }
        return name
    }

    /// Ticked renames in row order. A companion group goes whole or not at
    /// all: one invalid member name holds every member back.
    static func renames(_ rows: [Row]) -> [CatalogWrites.NamedRename] {
        let blocked = Set(rows.filter { $0.group != nil && $0.checked && $0.canCheck && validName($0.edited) == nil }.map(\.groupKey))
        return rows.compactMap { row in
            guard row.checked, row.canCheck, !blocked.contains(row.groupKey),
                  let name = validName(row.edited), name != row.currentName else { return nil }
            return CatalogWrites.NamedRename(fileID: row.fileID, newName: name, group: row.group)
        }
    }

    static func reasonChip(_ row: Row) -> String {
        row.learned.isEmpty ? reasonLabel(row.reason) : reasonLabel(row.reason) + " · your style"
    }

    /// What the learned preferences named in a proposeNames reason did to this name.
    static func learnedHelp(_ keys: [String]) -> String? {
        let parts = keys.compactMap { key -> String? in
            switch key {
            case "dropWords": "leaves out words you remove"
            case "people": "leaves out people's names"
            case "words": "keeps it as short as you do"
            case "leadingDate", "trailingDate": "puts the date where you do"
            case "dateFormat": "writes the date your way"
            case "separator": "uses your word separator"
            case "case": "uses your capitals"
            default: nil
            }
        }
        return parts.isEmpty ? nil : "Learned from your renames: " + parts.joined(separator: ", ")
    }

    struct Learned: Equatable, Sendable {
        var samples = 0
        var items: [String] = []
    }

    static func learned(from hits: [CatalogHit]) -> Learned {
        var learned = Learned()
        for hit in hits {
            if hit.kind == "nameSamples" { learned.samples = Int(hit.text) ?? 0 }
            if hit.kind == "namePreference", let label = preferenceLabel(hit.evidenceID ?? "", hit.text) { learned.items.append(label) }
        }
        return learned
    }

    static func preferenceLabel(_ key: String, _ value: String) -> String? {
        switch (key, value) {
        case ("separator", "_"): "underscores between words"
        case ("separator", " "): "spaces between words"
        case ("case", "title"): "Capitalised Words"
        case ("case", "upper"): "CAPITALS"
        case ("leadingDate", "end"): "the date at the end"
        case ("trailingDate", "start"): "the date first"
        case ("leadingDate", "none"), ("trailingDate", "none"): "no date"
        case ("dateFormat", "compact"): "dates like 20230704"
        case ("dateFormat", "underscore"): "dates like 2023_07_04"
        case ("dateFormat", "dotted"): "dates like 2023.07.04"
        case ("dateFormat", "month"): "dates like 2023-07"
        case ("people", _): "no people's names"
        case ("words", _): "at most \(value) word\(value == "1" ? "" : "s")"
        case ("dropWords", _): "without \u{201C}\(value)\u{201D}"
        default: nil
        }
    }

    static func learnedSummary(_ learned: Learned, enabled: Bool) -> String {
        guard enabled else { return "Names follow the style above only." }
        guard learned.samples > 0 else { return "Edit names before you rename them and FileID will follow your style." }
        let count = "\(learned.samples.formatted()) rename\(learned.samples == 1 ? "" : "s")"
        guard !learned.items.isEmpty else { return "No clear preference yet from \(count)." }
        return "From \(count): " + learned.items.joined(separator: ", ") + "."
    }

    /// One sample per renamed file the user saw in the preview: singles and
    /// group leaders, never followers, whose rename landed.
    static func edits(_ rows: [Row], renamed: Set<Int64>) -> [CatalogNameEdit] {
        rows.compactMap { row in
            guard row.checked, row.canCheck, !row.isFollower, renamed.contains(row.fileID), let name = validName(row.edited) else { return nil }
            return CatalogNameEdit(fileID: row.fileID, proposed: row.proposed, applied: name)
        }
    }

    static func learnRequest(id: String, edits: [CatalogNameEdit], naming: CatalogNaming) -> CatalogRequest {
        CatalogRequest(requestID: id, action: "learnNames", naming: naming, nameEdits: edits)
    }

    static let learnedEditsKey = "naming.learnedEdits"
    static let learnedEditsKeep = 1000

    /// The samples learnNames recorded, newest last, so an Undo from any
    /// surface can tell the engine which ones to forget.
    nonisolated static func rememberLearned(_ edits: [CatalogNameEdit], defaults: UserDefaults = .standard) {
        guard !edits.isEmpty else { return }
        let all = (storedLearned(defaults) + edits).suffix(learnedEditsKeep)
        if let data = try? JSONEncoder().encode(Array(all)) { defaults.set(data, forKey: learnedEditsKey) }
    }

    nonisolated static func storedLearned(_ defaults: UserDefaults = .standard) -> [CatalogNameEdit] {
        defaults.data(forKey: learnedEditsKey).flatMap { try? JSONDecoder().decode([CatalogNameEdit].self, from: $0) } ?? []
    }

    /// Splits the stored samples after an Undo: for each rename put back, the
    /// newest stored sample of that file whose applied name is the name it was
    /// undone from is forgotten; everything else stays.
    nonisolated static func undone(_ stored: [CatalogNameEdit], outcomes: [ReadStore.RenameOutcome],
                                   putBack: Set<Int64>) -> (forget: [CatalogNameEdit], keep: [CatalogNameEdit]) {
        var keep = stored, forget: [CatalogNameEdit] = []
        for outcome in outcomes where putBack.contains(outcome.fileID) {
            let name = URL(fileURLWithPath: outcome.newPath).lastPathComponent
            guard let index = keep.lastIndex(where: { $0.fileID == outcome.fileID && $0.applied == name }) else { continue }
            forget.append(keep.remove(at: index))
        }
        return (forget, keep)
    }

    static func unlearnRequest(id: String, edits: [CatalogNameEdit]) -> CatalogRequest {
        CatalogRequest(requestID: id, action: "unlearnNames", nameEdits: edits)
    }

    static func preferencesRequest(id: String, reset: Bool) -> CatalogRequest {
        CatalogRequest(requestID: id, action: reset ? "resetNamePreferences" : "namePreferences")
    }

    static func accessibilityLabel(_ row: Row, in rows: [Row] = []) -> String {
        let reason = reasonChip(row)
        let because = reason.prefix(1).lowercased() + reason.dropFirst()
        let others = row.group.map { group in rows.filter { $0.group == group && $0.fileID != row.fileID }.map(\.currentName) } ?? []
        let together = others.isEmpty ? "" : ", together with " + others.joined(separator: ", ")
        guard row.canCheck else { return "\(row.currentName), keeps its name, \(because)\(together)" }
        return "\(row.currentName), rename to \(row.edited.trimmingCharacters(in: .whitespacesAndNewlines)), \(because)\(together)"
    }
}
