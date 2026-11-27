import Foundation
import GRDB
import FileIDShared

/// Where the next Library page starts: the sort values of the last row shown. Paging from a key never re-sorts the rows before it, unlike OFFSET.
public struct LibraryCursor: Equatable, Sendable {
    var id: Int64
    var number: Double?
    var integer: Int64?
    var text: String?
    var name: String?
}

public struct LibraryPage: Sendable {
    public var rows: [FileRow]
    public var cursor: LibraryCursor?
}

struct LibrarySegment {
    var condition: String
    var arguments: [any DatabaseValueConvertible]
    var order: String
}

extension LibrarySort {
    static let takenSQL = "COALESCE(created_at, modified_at)"

    /// A date past this is a clock or metadata fault, not a capture date: those files list after the undated ones.
    static func futureLimit(_ now: Date) -> Double { now.timeIntervalSince1970 + 86_400 }

    /// `ORDER BY` for a catalog whose engine writes `files.name_sort`: names and kinds then sort by an indexed key, not the app's collation.
    func orderSQL(keyed: Bool) -> String {
        guard keyed, key == .name || key == .kind else { return orderSQL }
        let direction = ascending ? "ASC" : "DESC"
        return key == .name ? "name_sort \(direction), id \(direction)" : "kind \(direction), name_sort ASC, id ASC"
    }

    /// Columns read next to `id`, in the order `cursor(from:)` expects them.
    func cursorColumns(keyed: Bool) -> [String]? {
        switch key {
        case .dateAdded: return ["scanned_at"]
        case .dateTaken: return [Self.takenSQL]
        case .size: return ["size_bytes"]
        case .name: return keyed ? ["name_sort"] : nil
        case .kind: return keyed ? ["kind", "name_sort"] : nil
        }
    }

    func cursor(id: Int64, values: [DatabaseValue]) -> LibraryCursor {
        var cursor = LibraryCursor(id: id)
        switch key {
        case .dateAdded, .dateTaken: cursor.number = Double.fromDatabaseValue(values[0])
        case .size: cursor.integer = Int64.fromDatabaseValue(values[0])
        case .name: cursor.name = String.fromDatabaseValue(values[0])
        case .kind:
            cursor.text = String.fromDatabaseValue(values[0])
            cursor.name = String.fromDatabaseValue(values[1])
        }
        return cursor
    }

    /// The queries that, run in order until the page is full, list the rows after `cursor`. Each one reads a contiguous range of an index, so a page far down the library costs what the first page does.
    func segments(after cursor: LibraryCursor?, keyed: Bool, now: Date = Date()) -> [LibrarySegment] {
        let direction = ascending ? "ASC" : "DESC"
        let beyond = ascending ? ">" : "<"
        switch key {
        case .dateAdded:
            guard let cursor, let number = cursor.number else { return [LibrarySegment(condition: "1", arguments: [], order: orderSQL)] }
            return [LibrarySegment(condition: "(scanned_at, id) \(beyond) (?, ?)", arguments: [number, cursor.id], order: orderSQL)]
        case .size:
            guard let cursor, let size = cursor.integer else { return [LibrarySegment(condition: "1", arguments: [], order: orderSQL)] }
            return [LibrarySegment(condition: "(size_bytes, id) \(beyond) (?, ?)", arguments: [size, cursor.id], order: orderSQL)]
        case .name:
            guard keyed else { return [LibrarySegment(condition: "1", arguments: [], order: orderSQL)] }
            guard let cursor, let name = cursor.name else { return [LibrarySegment(condition: "1", arguments: [], order: orderSQL(keyed: true))] }
            return [LibrarySegment(condition: "(name_sort, id) \(beyond) (?, ?)", arguments: [name, cursor.id], order: orderSQL(keyed: true))]
        case .kind:
            guard keyed else { return [LibrarySegment(condition: "1", arguments: [], order: orderSQL)] }
            guard let cursor, let kind = cursor.text, let name = cursor.name else {
                return [LibrarySegment(condition: "1", arguments: [], order: orderSQL(keyed: true))]
            }
            if ascending {
                return [LibrarySegment(condition: "(kind, name_sort, id) > (?, ?, ?)", arguments: [kind, name, cursor.id], order: orderSQL(keyed: true))]
            }
            return [
                LibrarySegment(condition: "kind = ? AND (name_sort, id) > (?, ?)", arguments: [kind, name, cursor.id], order: "name_sort ASC, id ASC"),
                LibrarySegment(condition: "kind < ?", arguments: [kind], order: orderSQL(keyed: true)),
            ]
        case .dateTaken:
            let taken = Self.takenSQL
            let dated = "(\(taken) IS NULL) = 0"
            let ordered = "\(taken) \(direction), id \(direction)"
            let limit = Self.futureLimit(now)
            let sane = LibrarySegment(condition: "\(dated) AND \(taken) <= ?", arguments: [limit], order: ordered)
            let undated = LibrarySegment(condition: "\(taken) IS NULL", arguments: [], order: "id \(direction)")
            let future = LibrarySegment(condition: "\(dated) AND \(taken) > ?", arguments: [limit], order: ordered)
            guard let cursor else { return [sane, undated, future] }
            guard let number = cursor.number else {
                return [LibrarySegment(condition: "\(taken) IS NULL AND id \(beyond) ?", arguments: [cursor.id], order: "id \(direction)"), future]
            }
            let after = "(\(taken), id) \(beyond) (?, ?)"
            if number > limit {
                return [LibrarySegment(condition: "\(dated) AND \(after)", arguments: [number, cursor.id], order: ordered)]
            }
            return [LibrarySegment(condition: "\(dated) AND \(taken) <= ? AND \(after)", arguments: [limit, number, cursor.id], order: ordered), undated, future]
        }
    }
}

/// How many rows each Library query asks for.
enum LibraryPageSizes {
    static let first = 300
    static let scrollCap = 2_000
    /// Rows per query when jumping to the end of a listing; tags follow afterwards.
    static let jump = 5_000
    static let tagChunk = 2_000

    /// Pages the grid asks for as the user scrolls grow with what is already loaded, so a long scroll needs few round trips.
    static func scroll(loaded: Int) -> Int { min(max(first, loaded / 2), scrollCap) }
}

/// What a refresh of an already loaded listing re-queries. Re-reading every loaded row costs seconds at 130,000 rows, so a refresh re-pages only the head and re-checks the rest by id in the background.
enum LibraryReload {
    static let head = 5_000
    static let recheckChunk = 5_000

    struct Plan: Equatable {
        var want: Int
        var keepsTail: Bool
    }

    static func plan(loaded: Int, sameListing: Bool, firstPageOnly: Bool, searching: Bool, complete: Bool, firstPage: Int = LibraryPageSizes.first) -> Plan {
        guard sameListing, !firstPageOnly else { return Plan(want: firstPage, keepsTail: false) }
        guard loaded > head else { return Plan(want: max(firstPage, loaded), keepsTail: false) }
        return Plan(want: head, keepsTail: !searching && complete)
    }

    /// The fresh head followed by the loaded rows after it, or nil when the head's last row is not among them (the listing moved too far to splice).
    static func splice(head: [FileRow], onto loaded: [FileRow]) -> [FileRow]? {
        guard let last = head.last, let at = loaded.firstIndex(where: { $0.id == last.id }) else { return nil }
        let known = Set(head.map(\.id))
        return head + loaded[(at + 1)...].filter { !known.contains($0.id) }
    }

    /// The loaded rows with each re-checked one replaced by its fresh copy, or dropped when it left the listing.
    static func applying(_ fresh: [Int64: FileRow], checked: Set<Int64>, to rows: [FileRow]) -> [FileRow] {
        rows.compactMap { row in checked.contains(row.id) ? fresh[row.id] : row }
    }
}
