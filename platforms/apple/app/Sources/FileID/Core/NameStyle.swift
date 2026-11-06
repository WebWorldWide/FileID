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

    static let defaultStyle = "short"
    static let defaultMaxLength = 40
    static let defaultIncludePeople = true
    static let defaultIncludeEvent = true
    static let defaultIncludeDate = false

    static let styles: [(id: String, title: String)] = [("short", "Short"), ("descriptive", "Descriptive"), ("dateFirst", "Date first")]
    static let lengthRange = 4...120
    static let maxFiles = 1_000
    static let previewDelay: Duration = .milliseconds(300)

    static func dateToggleEnabled(style: String) -> Bool { style != "dateFirst" }

    static func naming(style: String, maxLength: Int, includePeople: Bool, includeEvent: Bool, includeDate: Bool) -> CatalogNaming {
        let style = styles.contains { $0.id == style } ? style : defaultStyle
        return CatalogNaming(style: style,
                             maxLength: min(max(maxLength, lengthRange.lowerBound), lengthRange.upperBound),
                             includePeople: includePeople,
                             includeEvent: includeEvent,
                             includeDate: includeDate && dateToggleEnabled(style: style))
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
            let (reason, group) = SmartNameCompanions.parse(hit.evidenceID)
            return Row(fileID: hit.fileID, path: hit.path, proposed: hit.text, reason: reason,
                       edited: hit.text, checked: !hit.text.isEmpty && !unticked.contains(group ?? hit.fileID), group: group)
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

    static func accessibilityLabel(_ row: Row, in rows: [Row] = []) -> String {
        let reason = reasonLabel(row.reason)
        let because = reason.prefix(1).lowercased() + reason.dropFirst()
        let others = row.group.map { group in rows.filter { $0.group == group && $0.fileID != row.fileID }.map(\.currentName) } ?? []
        let together = others.isEmpty ? "" : ", together with " + others.joined(separator: ", ")
        guard row.canCheck else { return "\(row.currentName), keeps its name, \(because)\(together)" }
        return "\(row.currentName), rename to \(row.edited.trimmingCharacters(in: .whitespacesAndNewlines)), \(because)\(together)"
    }
}
