import Foundation
import FileIDShared

public enum LibrarySortKey: String, CaseIterable, Identifiable, Sendable {
    case dateAdded, dateTaken, name, size, kind

    public var id: String { rawValue }

    var title: String {
        switch self {
        case .dateAdded: "Date added"
        case .dateTaken: "Date taken"
        case .name: "Name"
        case .size: "Size"
        case .kind: "Kind"
        }
    }
}

public enum LibraryLayout: String, Sendable {
    case grid, list
}

public struct LibrarySort: Equatable, Sendable {
    static let keyStorage = "library.sortKey"
    static let ascendingStorage = "library.sortAscending"
    static let layoutStorage = "library.layout"
    public static let standard = LibrarySort(key: .dateAdded, ascending: false)

    var key: LibrarySortKey
    var ascending: Bool

    init(key: LibrarySortKey, ascending: Bool) {
        self.key = key
        self.ascending = ascending
    }

    init(storedKey: String, ascending: Bool) {
        self.init(key: LibrarySortKey(rawValue: storedKey) ?? Self.standard.key, ascending: ascending)
    }

    var summary: String { "\(key.title) \(ascending ? "↑" : "↓")" }

    /// The direction a key starts in when first chosen: names and kinds read A to Z, everything else newest or largest first.
    static func firstDirection(for key: LibrarySortKey) -> Bool { key == .name || key == .kind }

    /// Choosing the key already in use flips its direction; a new key starts in its natural one.
    func choosing(_ next: LibrarySortKey) -> LibrarySort {
        next == key ? LibrarySort(key: key, ascending: !ascending) : LibrarySort(key: next, ascending: Self.firstDirection(for: next))
    }

    /// The menu's key choice: the key already in use keeps its direction, a new key starts in its natural one.
    func selecting(_ next: LibrarySortKey) -> LibrarySort {
        next == key ? self : LibrarySort(key: next, ascending: Self.firstDirection(for: next))
    }

    /// SQL for `ORDER BY`. Ties and missing values resolve the same way in both directions so paging by offset never repeats or skips a row.
    var orderSQL: String {
        let direction = ascending ? "ASC" : "DESC"
        switch key {
        case .dateAdded:
            return "scanned_at \(direction), id \(direction)"
        case .dateTaken:
            return "(COALESCE(created_at, modified_at) IS NULL), COALESCE(created_at, modified_at) \(direction), id \(direction)"
        case .name:
            return "\(Self.fileNameSQL) COLLATE \(Self.nameCollation) \(direction), id \(direction)"
        case .size:
            return "size_bytes \(direction), id \(direction)"
        case .kind:
            return "kind \(direction), \(Self.fileNameSQL) COLLATE \(Self.nameCollation) ASC, id ASC"
        }
    }

    public static let nameCollation = "localizedStandardName"

    /// The text after the last "/" of `path_text`: strips the longest leading run that ends in a slash.
    static let fileNameSQL = "REPLACE(path_text, RTRIM(path_text, REPLACE(path_text, '/', '')), '')"
}

extension LibrarySort {
    /// The date a row shows under this sort: when it was added for Date added, the capture date otherwise, so the dates read in the order the rows come.
    func shownDate(of row: FileRow) -> Date? {
        key == .dateAdded ? row.scannedAt : row.displayDate
    }
}

extension FileRow {
    /// Whether this row's file moved since `old` was shown, so its picture must be loaded again. `index` is where it sits now; the same spot is checked first so a refresh costs nothing per row.
    func pathDiffers(at index: Int, from old: [FileRow]) -> Bool {
        if index < old.count, old[index].id == id { return old[index].pathText != pathText }
        guard let match = old.first(where: { $0.id == id }) else { return true }
        return match.pathText != pathText
    }
}
