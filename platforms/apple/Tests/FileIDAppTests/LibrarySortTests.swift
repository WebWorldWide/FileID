import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileID

@Suite("Library sort", .serialized)
struct LibrarySortTests {
    private static let rowCount = 5_000
    private static let kinds = ["image", "video", "pdf", "doc", "audio", "other"]

    private struct Fixture {
        let directory: URL
        let store: ReadStore
    }

    private func makeCatalog(futureIDs: Set<Int64> = []) async throws -> Fixture {
        let directory = FileManager.default.temporaryDirectory.appending(component: "fileid-sort-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let url = directory.appending(component: "fileid.sqlite")
        let queue = try DatabaseQueue(path: url.path)
        try await queue.write { db in
            try db.execute(sql: """
                CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, size_bytes INTEGER NOT NULL DEFAULT 1,
                    created_at REAL, modified_at REAL, scanned_at REAL NOT NULL DEFAULT 0,
                    kind TEXT NOT NULL, extension TEXT NOT NULL DEFAULT '', phash INTEGER, aesthetic REAL,
                    has_faces INTEGER, has_text INTEGER, camera_model TEXT, location_lat REAL, location_lon REAL,
                    failed INTEGER NOT NULL DEFAULT 0, error_message TEXT, vlm_description TEXT,
                    vlm_proposed_name TEXT, vlm_model TEXT, vlm_full_model TEXT, vlm_analyzed_at REAL,
                    vlm_inherited_from INTEGER, auto_tags TEXT, content_hash TEXT, path_search TEXT);
                CREATE VIRTUAL TABLE ocr_fts USING fts5(text);
                CREATE VIRTUAL TABLE doc_fts USING fts5(text);
                CREATE TABLE tags (file_id INTEGER NOT NULL, tag TEXT NOT NULL, source TEXT NOT NULL);
                CREATE TABLE persons (id INTEGER PRIMARY KEY, name TEXT, first_name TEXT, last_name TEXT, middle_name TEXT, title TEXT, suffix TEXT);
                CREATE TABLE face_prints (id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL, person_id INTEGER);
                """)
            for i in 1...Self.rowCount {
                let id = Int64(i)
                let folder = "/m/Photos/\(["Zeta", "alpha", "Mid"][i % 3])/\(i % 7)"
                let name = "IMG \(String(format: "%04d", (i * 37) % 997)) \(i).jpg"
                let taken: Double? = futureIDs.contains(id) ? Date().timeIntervalSince1970 + 15 * 365 * 86_400 : i % 11 == 0 ? nil : Double((i * 7919) % 100_003)
                let modified: Double? = i % 11 == 0 ? (i % 22 == 0 ? nil : Double(i)) : nil
                try db.execute(sql: """
                    INSERT INTO files(id, path_text, path_search, size_bytes, created_at, modified_at, scanned_at, kind, extension)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'jpg')
                    """, arguments: [id, "\(folder)/\(name)", "\(folder)/\(name)", Int64((i * 104_729) % 9_973), taken, modified,
                                     Double((i * 6571) % 5_003), Self.kinds[i % Self.kinds.count]])
            }
        }
        try queue.close()
        let store = ReadStore(dbURL: url)
        store.openIfPossible()
        return Fixture(directory: directory, store: store)
    }

    private func everyPage(_ fixture: Fixture, _ sort: LibrarySort) -> [FileRow] {
        var all: [FileRow] = []
        while true {
            let page = fixture.store.files(offset: all.count, limit: 300, sort: sort)
            all += page
            if page.count < 300 { return all }
        }
    }

    private func isOrdered<T: Comparable>(_ rows: [FileRow], ascending: Bool, _ key: (FileRow) -> T) -> Bool {
        zip(rows, rows.dropFirst()).allSatisfy { a, b in ascending ? key(a) <= key(b) : key(a) >= key(b) }
    }

    @Test func everyKeyAndDirectionOrdersTheWholeCatalogThroughPaging() async throws {
        let fixture = try await makeCatalog()
        defer { fixture.store.close(); try? FileManager.default.removeItem(at: fixture.directory) }
        for ascending in [true, false] {
            let added = everyPage(fixture, LibrarySort(key: .dateAdded, ascending: ascending))
            #expect(added.count == Self.rowCount)
            #expect(Set(added.map(\.id)).count == Self.rowCount, "paging repeated or skipped a row")
            #expect(isOrdered(added, ascending: ascending) { $0.scannedAt })

            let sized = everyPage(fixture, LibrarySort(key: .size, ascending: ascending))
            #expect(Set(sized.map(\.id)).count == Self.rowCount)
            #expect(isOrdered(sized, ascending: ascending) { $0.sizeBytes })

            let taken = everyPage(fixture, LibrarySort(key: .dateTaken, ascending: ascending))
            #expect(Set(taken.map(\.id)).count == Self.rowCount)
            let dated = taken.compactMap(\.displayDate)
            #expect(dated.count == taken.filter { $0.displayDate != nil }.count)
            #expect(zip(dated, dated.dropFirst()).allSatisfy { ascending ? $0 <= $1 : $0 >= $1 })
            let firstUndated = try #require(taken.firstIndex { $0.displayDate == nil })
            #expect(taken[firstUndated...].allSatisfy { $0.displayDate == nil }, "files without a date sort last in both directions")

            let named = everyPage(fixture, LibrarySort(key: .name, ascending: ascending))
            #expect(Set(named.map(\.id)).count == Self.rowCount)
            let fileNames = named.map { $0.url.lastPathComponent }
            #expect(zip(fileNames, fileNames.dropFirst()).allSatisfy {
                let order = $0.localizedStandardCompare($1)
                return ascending ? order != .orderedDescending : order != .orderedAscending
            }, "names are ordered by file name, not by folder")

            let kinds = everyPage(fixture, LibrarySort(key: .kind, ascending: ascending))
            #expect(Set(kinds.map(\.id)).count == Self.rowCount)
            #expect(isOrdered(kinds, ascending: ascending) { $0.kind })
        }
    }

    @Test func aDateInTheFutureIsNotACaptureDateAndSortsAmongTheUndated() async throws {
        let fixture = try await makeCatalog(futureIDs: [1, 2])
        defer { fixture.store.close(); try? FileManager.default.removeItem(at: fixture.directory) }
        let future = Date().timeIntervalSince1970 + 15 * 365 * 86_400
        for ascending in [true, false] {
            let sort = LibrarySort(key: .dateTaken, ascending: ascending)
            let firstPage = fixture.store.files(limit: 300, sort: sort)
            #expect(firstPage.allSatisfy { $0.createdAt.map { $0.timeIntervalSince1970 < future } ?? true })
            let all = everyPage(fixture, sort)
            #expect(all.count == Self.rowCount)
            #expect(Set(all.map(\.id)).count == Self.rowCount)
            #expect(Set(all.suffix(2).map(\.id)) == [1, 2], "future-dated files list last")
        }
    }

    @Test func theDefaultListingStillShowsTheNewestScanFirst() async throws {
        let fixture = try await makeCatalog()
        defer { fixture.store.close(); try? FileManager.default.removeItem(at: fixture.directory) }
        let defaultRows = fixture.store.files(limit: 50)
        #expect(defaultRows.map(\.id) == fixture.store.files(limit: 50, sort: LibrarySort(key: .dateAdded, ascending: false)).map(\.id))
        #expect(isOrdered(defaultRows, ascending: false) { $0.scannedAt })
    }

    @Test func aSortComposesWithTheKindFilterAndTheSearch() async throws {
        let fixture = try await makeCatalog()
        defer { fixture.store.close(); try? FileManager.default.removeItem(at: fixture.directory) }
        let videos = fixture.store.files(limit: 100, kindFilter: "video", sort: LibrarySort(key: .size, ascending: true))
        #expect(videos.count == 100)
        #expect(videos.allSatisfy { $0.kind == "video" })
        #expect(isOrdered(videos, ascending: true) { $0.sizeBytes })
        let searched = await fixture.store.filesAsync(limit: 40, search: "IMG 0042", sort: LibrarySort(key: .name, ascending: false))
        #expect(!searched.isEmpty)
        #expect(searched.allSatisfy { $0.pathText.contains("IMG 0042") })
        let names = searched.map { $0.url.lastPathComponent }
        #expect(names == names.sorted { $0.localizedStandardCompare($1) == .orderedDescending })
    }

    @Test func aBigCatalogSortsInTheDatabaseQuickly() async throws {
        let fixture = try await makeCatalog()
        defer { fixture.store.close(); try? FileManager.default.removeItem(at: fixture.directory) }
        for key in LibrarySortKey.allCases {
            let started = ContinuousClock.now
            let page = await fixture.store.filesAsync(offset: 4_700, limit: 300, sort: LibrarySort(key: key, ascending: true))
            #expect(page.count == 300, "\(key)")
            #expect(started.duration(to: .now) < .seconds(2), "\(key) took \(started.duration(to: .now))")
        }
    }

    @Test func choosingTheKeyInUseFlipsItAndANewKeyStartsNaturally() {
        let standard = LibrarySort.standard
        #expect(standard == LibrarySort(key: .dateAdded, ascending: false))
        #expect(standard.choosing(.dateAdded) == LibrarySort(key: .dateAdded, ascending: true))
        #expect(standard.choosing(.name) == LibrarySort(key: .name, ascending: true))
        #expect(standard.choosing(.size) == LibrarySort(key: .size, ascending: false))
        #expect(LibrarySort(storedKey: "garbage", ascending: true) == LibrarySort(key: .dateAdded, ascending: true))
        #expect(LibrarySort(storedKey: "kind", ascending: false).summary == "Kind ↓")
    }

    @Test("Date added, Date taken and Size page through an index, never a full sort")
    func indexedSortsNeedNoTempSorter() throws {
        let sql = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .appending(path: "shared/catalog/v37.sql")
        let queue = try DatabaseQueue()
        try queue.write { db in
            try db.execute(sql: """
                CREATE TABLE files (id INTEGER PRIMARY KEY, size_bytes INTEGER NOT NULL DEFAULT 0, created_at REAL, modified_at REAL,
                    scanned_at REAL NOT NULL DEFAULT 0, failed INTEGER NOT NULL DEFAULT 0, path_text TEXT NOT NULL DEFAULT '');
                CREATE INDEX idx_files_scanned ON files(scanned_at);
                """)
            try db.execute(sql: String(contentsOf: sql, encoding: .utf8))
        }
        for key in [LibrarySortKey.dateAdded, .dateTaken, .size] {
            for ascending in [true, false] {
                let sort = LibrarySort(key: key, ascending: ascending)
                let plan = try queue.read { db in
                    try Row.fetchAll(db, sql: "EXPLAIN QUERY PLAN SELECT id FROM files WHERE failed = 0 ORDER BY \(sort.orderSQL) LIMIT 200")
                        .map { ($0["detail"] as String) }.joined(separator: " | ")
                }
                #expect(!plan.contains("TEMP B-TREE"), "\(sort.summary): \(plan)")
            }
        }
    }
}

@Suite struct LibraryShownDateTests {
    private func row(id: Int64, path: String = "/a/x.jpg", taken: Date?, added: Date) -> FileRow {
        FileRow(id: id, pathText: path, sizeBytes: 1, createdAt: taken, modifiedAt: nil, scannedAt: added, kind: "image", extension: "jpg", phash: nil,
                aesthetic: nil, hasFaces: false, hasText: false, cameraModel: nil, locationLat: nil, locationLon: nil, failed: false, errorMessage: nil)
    }

    @Test func aCaptureDateInTheFutureIsNotShown() {
        let future = Date().addingTimeInterval(15 * 365 * 86_400)
        #expect(row(id: 1, taken: future, added: Date()).displayDate == nil)
        #expect(row(id: 2, taken: Date(timeIntervalSince1970: 1_000), added: Date()).displayDate == Date(timeIntervalSince1970: 1_000))
    }

    @Test func dateAddedShowsWhenItWasAddedAndOtherSortsShowTheCaptureDate() {
        let taken = Date(timeIntervalSince1970: 1_000), added = Date(timeIntervalSince1970: 9_000)
        let file = row(id: 1, taken: taken, added: added)
        #expect(LibrarySort(key: .dateAdded, ascending: false).shownDate(of: file) == added)
        #expect(LibrarySort(key: .dateTaken, ascending: false).shownDate(of: file) == file.displayDate)
        #expect(file.displayDate != added)
    }

    @Test func aRefreshComparesPathsAtTheSameSpotAndFindsMovedRowsElsewhere() {
        let day = Date(timeIntervalSince1970: 5)
        let old = [row(id: 1, path: "/a/1.jpg", taken: nil, added: day), row(id: 2, path: "/a/2.jpg", taken: nil, added: day)]
        #expect(!old[0].pathDiffers(at: 0, from: old))
        #expect(row(id: 2, path: "/b/2.jpg", taken: nil, added: day).pathDiffers(at: 1, from: old))
        #expect(!old[1].pathDiffers(at: 0, from: old))
        #expect(row(id: 3, taken: nil, added: day).pathDiffers(at: 2, from: old))
    }
}
