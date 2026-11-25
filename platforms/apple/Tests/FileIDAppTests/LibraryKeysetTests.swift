import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileID

@Suite("Library keyset paging and indexed search", .serialized)
struct LibraryKeysetTests {
    private static let rowCount = 3_000
    private static let kinds = ["image", "video", "pdf", "doc"]

    private static func sortKey(_ name: String) -> String {
        var out = "", digits = ""
        func flush() { if !digits.isEmpty { out += String(format: "%02d", digits.count) + digits; digits = "" } }
        for character in name.lowercased() {
            if character.isASCII && character.isNumber { digits.append(character) } else { flush(); out.append(character) }
        }
        flush()
        return out
    }

    private static func repositoryRoot() -> URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
    }

    private static let schema = """
        CREATE TABLE files (
            id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, size_bytes INTEGER NOT NULL DEFAULT 1,
            created_at REAL, modified_at REAL, scanned_at REAL NOT NULL DEFAULT 0,
            kind TEXT NOT NULL, extension TEXT NOT NULL DEFAULT '', phash INTEGER, aesthetic REAL,
            has_faces INTEGER, has_text INTEGER, camera_model TEXT, location_lat REAL, location_lon REAL,
            failed INTEGER NOT NULL DEFAULT 0, error_message TEXT, vlm_description TEXT,
            vlm_proposed_name TEXT, vlm_model TEXT, vlm_full_model TEXT, vlm_analyzed_at REAL,
            vlm_inherited_from INTEGER, auto_tags TEXT, content_hash TEXT, path_search TEXT,
            name_sort TEXT NOT NULL DEFAULT '');
        CREATE INDEX idx_files_scanned ON files(scanned_at);
        CREATE VIRTUAL TABLE ocr_fts USING fts5(text);
        CREATE VIRTUAL TABLE doc_fts USING fts5(text);
        CREATE TABLE tags (file_id INTEGER NOT NULL, tag TEXT NOT NULL, source TEXT NOT NULL);
        CREATE TABLE persons (id INTEGER PRIMARY KEY, name TEXT, first_name TEXT, last_name TEXT, middle_name TEXT, title TEXT, suffix TEXT);
        CREATE TABLE face_prints (id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL, person_id INTEGER);
        """

    private func makeCatalog(rows: Int = rowCount, applyIndexes: Bool = true) async throws -> (directory: URL, store: ReadStore, queue: DatabaseQueue) {
        let directory = FileManager.default.temporaryDirectory.appending(component: "fileid-keyset-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let url = directory.appending(component: "fileid.sqlite")
        let queue = try DatabaseQueue(path: url.path)
        try await queue.write { db in
            try db.execute(sql: Self.schema)
            if applyIndexes {
                for version in [37, 38] {
                    try db.execute(sql: String(contentsOf: Self.repositoryRoot().appending(path: "shared/catalog/v\(version).sql"), encoding: .utf8)
                        .components(separatedBy: "ALTER TABLE files ADD COLUMN name_sort TEXT NOT NULL DEFAULT '';").joined())
                }
            }
            for i in 1...rows {
                let name = "IMG \(i % 211) copy \(i).jpg"
                let path = "/m/\(["Zeta", "alpha", "Mid"][i % 3])/\(name)"
                let taken: Double? = i % 11 == 0 ? nil : Double((i * 7919) % 100_003)
                try db.execute(sql: """
                    INSERT INTO files(id, path_text, path_search, size_bytes, created_at, scanned_at, kind, extension, name_sort)
                    VALUES (?, ?, ?, ?, ?, ?, ?, 'jpg', ?)
                    """, arguments: [Int64(i), path, path, Int64((i * 104_729) % 977), taken, Double((i * 6571) % 503),
                                     Self.kinds[i % Self.kinds.count], Self.sortKey(name)])
            }
        }
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        return (directory, store, queue)
    }

    private func walk(_ store: ReadStore, _ sort: LibrarySort, limit: Int = 250) async -> [FileRow] {
        var all: [FileRow] = []
        var cursor: LibraryCursor?
        while true {
            let page = await store.filesPageAsync(limit: limit, after: cursor, sort: sort)
            all += page.rows
            guard page.rows.count == limit, let next = page.cursor else { return all }
            cursor = next
        }
    }

    @Test("Every key and direction pages by key to the same rows an OFFSET walk returns, without repeats or gaps")
    func keysetWalkEqualsTheOffsetWalk() async throws {
        let fixture = try await makeCatalog()
        defer { fixture.store.close(); try? FileManager.default.removeItem(at: fixture.directory) }
        for key in LibrarySortKey.allCases {
            for ascending in [true, false] {
                let sort = LibrarySort(key: key, ascending: ascending)
                let keyed = await walk(fixture.store, sort)
                var offset: [FileRow] = []
                while true {
                    let page = fixture.store.files(offset: offset.count, limit: 250, sort: sort)
                    offset += page
                    if page.count < 250 { break }
                }
                #expect(keyed.count == Self.rowCount, "\(sort.summary)")
                #expect(Set(keyed.map(\.id)).count == Self.rowCount, "\(sort.summary) repeated or skipped a row")
                #expect(keyed.map(\.id) == offset.map(\.id), "\(sort.summary)")
            }
        }
    }

    @Test("Names with numbers sort by value and a page far down costs an index range, not a sort")
    func nameKeyIsNumericAndPagesThroughTheIndex() async throws {
        let fixture = try await makeCatalog()
        defer { fixture.store.close(); try? FileManager.default.removeItem(at: fixture.directory) }
        let names = (await walk(fixture.store, LibrarySort(key: .name, ascending: true))).map { $0.url.lastPathComponent }
        let copy = try #require(names.firstIndex { $0.hasPrefix("IMG 2 copy") })
        let ten = try #require(names.firstIndex { $0.hasPrefix("IMG 10 copy") })
        #expect(copy < ten, "IMG 2 sorts before IMG 10")
        for key in LibrarySortKey.allCases {
            for ascending in [true, false] {
                let sort = LibrarySort(key: key, ascending: ascending)
                let cursor = LibraryCursor(id: 1_500, number: 4_000, integer: 400, text: "image", name: "img")
                for segment in sort.segments(after: cursor, keyed: true) {
                    let sql = "EXPLAIN QUERY PLAN SELECT id FROM files WHERE failed = 0 AND \(segment.condition) ORDER BY \(segment.order) LIMIT 200"
                    let arguments: StatementArguments = StatementArguments(segment.arguments)
                    let plan = try await fixture.queue.read { db in
                        try Row.fetchAll(db, sql: sql, arguments: arguments).map { $0["detail"] as String }.joined(separator: " | ")
                    }
                    #expect(!plan.contains("TEMP B-TREE"), "\(sort.summary): \(plan)")
                }
            }
        }
    }

    @Test("A catalog without the name key still lists and pages Name and Kind by offset")
    func olderCatalogFallsBackToTheCollation() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(component: "fileid-keyset-old-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appending(component: "fileid.sqlite")
        let queue = try DatabaseQueue(path: url.path)
        try await queue.write { db in
            try db.execute(sql: Self.schema.replacingOccurrences(of: ",\n    name_sort TEXT NOT NULL DEFAULT ''", with: ""))
            for i in 1...400 {
                try db.execute(sql: "INSERT INTO files(id, path_text, path_search, kind) VALUES (?, ?, ?, 'image')", arguments: [Int64(i), "/m/file \(i).jpg", "/m/file \(i).jpg"])
            }
        }
        try queue.close()
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        defer { store.close() }
        let sort = LibrarySort(key: .name, ascending: true)
        let first = await store.filesPageAsync(limit: 100, sort: sort)
        #expect(first.rows.count == 100)
        #expect(first.cursor == nil, "no key to continue from: the caller keeps using offsets")
        let second = await store.filesPageAsync(offset: 100, limit: 100, sort: sort)
        #expect(Set(first.rows.map(\.id)).isDisjoint(with: second.rows.map(\.id)))
        let names = (first.rows + second.rows).map { $0.url.lastPathComponent }
        #expect(names == names.sorted { $0.localizedStandardCompare($1) == .orderedAscending })
    }

    @Test("A name or description search of three letters or more asks the trigram index; shorter ones scan; both find partial names")
    func searchUsesTheTrigramIndexWhenThereIsOne() async throws {
        let fixture = try await makeCatalog(rows: 60)
        defer { fixture.store.close(); try? FileManager.default.removeItem(at: fixture.directory) }
        try await fixture.queue.write { db in
            try db.execute(sql: "UPDATE files SET path_text = '/m/SunsetBeach.jpg', path_search = '/m/SunsetBeach.jpg' WHERE id = 7")
            try db.execute(sql: "UPDATE files SET path_text = '/m/beachball.png', path_search = '/m/beachball.png' WHERE id = 8")
            try db.execute(sql: "UPDATE files SET vlm_description = 'two dogs on the beach' WHERE id = 9")
            try db.execute(sql: String(contentsOf: Self.repositoryRoot().appending(path: "shared/catalog/v39.sql"), encoding: .utf8))
        }
        let wholeWord = await fixture.store.filesAsync(limit: 50, search: "beach")
        #expect(Set(wholeWord.map(\.id)) == [7, 8, 9])
        let partial = await fixture.store.filesAsync(limit: 50, search: "each")
        #expect(Set(partial.map(\.id)) == [7, 8, 9])
        let phrase = await fixture.store.filesAsync(limit: 50, search: "dogs on")
        #expect(Set(phrase.map(\.id)) == [9])
        try await fixture.queue.write { db in
            try db.execute(sql: "INSERT INTO files_search_fts(files_search_fts, rowid, path_search, vlm_proposed_name, vlm_description) VALUES ('delete', 8, '/m/beachball.png', NULL, NULL)")
        }
        let indexed = await fixture.store.filesAsync(limit: 50, search: "beach")
        #expect(Set(indexed.map(\.id)) == [7, 9], "three letters or more are answered from the index")
        let scanned = await fixture.store.filesAsync(limit: 50, search: "be")
        #expect(scanned.map(\.id).contains(8), "two letters scan the names, as before")
    }
}
