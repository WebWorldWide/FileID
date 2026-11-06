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
        var id: Int64 { fileID }
        var currentName: String { (path as NSString).lastPathComponent }
        var canCheck: Bool { !proposed.isEmpty }
    }

    /// Rows for a reply. A file the user unticked in the last preview stays
    /// unticked; rows without a proposed name can't be ticked.
    static func rows(from hits: [CatalogHit], previous: [Row]) -> [Row] {
        let unticked = Set(previous.filter { $0.canCheck && !$0.checked }.map(\.fileID))
        return hits.filter { $0.kind == "proposedName" }.map { hit in
            Row(fileID: hit.fileID, path: hit.path, proposed: hit.text, reason: hit.evidenceID ?? "noEvidence",
                edited: hit.text, checked: !hit.text.isEmpty && !unticked.contains(hit.fileID))
        }
    }

    static func validName(_ text: String) -> String? {
        let name = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, name != ".", name != "..", !name.contains("/"), !name.contains("\0") else { return nil }
        return name
    }

    static func renames(_ rows: [Row]) -> [CatalogWrites.NamedRename] {
        rows.compactMap { row in
            guard row.checked, row.canCheck, let name = validName(row.edited), name != row.currentName else { return nil }
            return CatalogWrites.NamedRename(fileID: row.fileID, newName: name)
        }
    }

    static func accessibilityLabel(_ row: Row) -> String {
        let reason = reasonLabel(row.reason)
        let because = reason.prefix(1).lowercased() + reason.dropFirst()
        guard row.canCheck else { return "\(row.currentName), keeps its name, \(because)" }
        return "\(row.currentName), rename to \(row.edited.trimmingCharacters(in: .whitespacesAndNewlines)), \(because)"
    }
}
