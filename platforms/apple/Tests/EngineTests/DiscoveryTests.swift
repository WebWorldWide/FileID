// Discovery smoke test. Creates a temporary directory tree of taggable +
// non-taggable files, runs Discovery.walk, asserts the right files were
// returned in the right (sorted) order with the right kinds.
import Testing
import Foundation
import GRDB
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database

@Suite("Discovery")
struct DiscoveryTests {

    @Test("Walks a small tree and returns sorted, filtered files")
    func smallTree() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDDiscoveryTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let sub = tmp.appendingPathComponent("photos")
        try FileManager.default.createDirectory(at: sub, withIntermediateDirectories: true)

        // Files we expect to be discovered.
        let goodFiles = [
            sub.appendingPathComponent("a.jpg"),
            sub.appendingPathComponent("b.png"),
            tmp.appendingPathComponent("c.pdf"),
            tmp.appendingPathComponent("d.mp4")
        ]
        // Files we expect to be filtered out.
        let badFiles = [
            tmp.appendingPathComponent(".hidden.jpg"),     // hidden
            tmp.appendingPathComponent("notes.xyz"),        // unknown ext
            tmp.appendingPathComponent("README")            // no ext
        ]
        let payload = Data("hello".utf8)
        for url in goodFiles + badFiles {
            try payload.write(to: url)
        }

        let discovery = Discovery()
        let result = await discovery.walk(root: tmp)

        // Expected: the 4 good files. Order sorted by path lexicographically.
        // macOS resolves /var → /private/var via the enumerator, so resolve
        // the same symlinks on the expected side before comparing.
        let resultPaths = result.map { $0.url.resolvingSymlinksInPath().path }
        let expectedPaths = goodFiles.map { $0.resolvingSymlinksInPath().path }.sorted()
        #expect(resultPaths == expectedPaths)

        // Spot-check kinds.
        let byExt: [String: DiscoveredFile.Kind] = Dictionary(
            uniqueKeysWithValues: result.map { ($0.url.pathExtension.lowercased(), $0.kind) }
        )
        #expect(byExt["jpg"] == .image)
        #expect(byExt["png"] == .image)
        #expect(byExt["pdf"] == .pdf)
        #expect(byExt["mp4"] == .video)
    }

    @Test("Skips files larger than the size cap")
    func skipsLargeFiles() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDLargeTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        // Tiny taggable file.
        let small = tmp.appendingPathComponent("small.jpg")
        try Data("x".utf8).write(to: small)
        // 2 MB "video" with size cap 1 MB.
        let big = tmp.appendingPathComponent("big.mp4")
        try Data(repeating: 0, count: 2 * 1024 * 1024).write(to: big)

        let discovery = Discovery()
        let result = await discovery.walk(root: tmp, maxSizeMB: 1)

        #expect(result.count == 1)
        #expect(result.first?.url.lastPathComponent == "small.jpg")
    }

    // re-audit R-08: a file the incremental skip set DROPS is still present on
    // disk, so discovery must refresh its `scanned_at`; otherwise the post-scan
    // orphan sweep (which prunes rows with `scanned_at < scanStart`) would treat
    // every skipped-but-present file as a deletion candidate and stop pruning the
    // genuinely-deleted ones once its cap is saturated.
    @Test("A skipped unchanged file still gets its scanned_at bumped (R-08)")
    func skippedFileScannedAtIsBumped() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDSkipTouch-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))

        // Resolve to the REAL path so the inserted path_text, the skip-set prefix
        // range, AND the enumerator's output all agree. `realpath` (not
        // Foundation's resolvingSymlinksInPath, which STRIPS /private) yields the
        // /private-prefixed form the FileManager enumerator actually emits for a
        // /var temp dir; otherwise the skip range excludes the row and nothing
        // skips. (Real scan roots are /Users/.. or /Volumes/.. — no /private.)
        let root = realResolved(tmp)

        // A non-image doc: skippable without a CLIP embedding (the R-14 carve-out
        // forces only embeddingless IMAGES to stay in the pipeline).
        let doc = root.appendingPathComponent("report.pdf")
        let bytes = Data("hello".utf8)                              // 5 bytes
        try bytes.write(to: doc)
        let fixedMtime = Date(timeIntervalSince1970: 1_700_000_000) // whole second
        try FileManager.default.setAttributes(
            [.modificationDate: fixedMtime], ofItemAtPath: doc.path)

        // Seed a prior-scan row: same size + mtime, but a deliberately OLD
        // scanned_at (as an earlier scan, before the current scanStart, would have).
        let oldScannedAt = 1_000.0
        try await db.pool.write { conn in
            try conn.execute(sql: """
                INSERT INTO files
                  (path_text, path_hash, path_search, size_bytes, modified_at,
                   scanned_at, kind, extension)
                VALUES (?, ?, ?, ?, ?, ?, 'doc', 'pdf')
                """, arguments: [
                    doc.path, 0, doc.path.precomposedStringWithCanonicalMapping,
                    Int(bytes.count), fixedMtime.timeIntervalSince1970, oldScannedAt
                ])
        }

        let discovery = Discovery()
        let result = await discovery.walk(root: root, database: db, forceReprocess: false)

        // Unchanged → SKIPPED (never re-emitted to the pipeline)…
        #expect(!result.contains { $0.url.lastPathComponent == "report.pdf" })

        // …but its scanned_at must be refreshed to ~now so the orphan sweep
        // (scanned_at < scanStart) won't treat the present file as deleted.
        let after: Double = try await db.pool.read { conn in
            try Double.fetchOne(conn, sql:
                "SELECT scanned_at FROM files WHERE path_text = ?",
                arguments: [doc.path]) ?? -1
        }
        #expect(after > oldScannedAt)
        #expect(after > fixedMtime.timeIntervalSince1970)
    }
}

// re-audit R-09: the incremental-skip predicate must match DBWriter's `unchanged`
// contract — skip ONLY when size matches AND the current mtime EQUALS the stored
// modified_at. The prior `scanned_at >= mtime` form was looser and skipped
// same-size, backdated-mtime edits forever.
@Suite("Discovery incremental-skip predicate (R-09)")
struct DiscoverySkipPredicateTests {

    @Test("size mismatch never skips")
    func sizeMismatch() {
        #expect(!Discovery.isAlreadyCurrent(
            dbModifiedAt: 100, dbSize: 10, currentModified: 100, currentSize: 11))
    }

    @Test("equal size + equal mtime skips")
    func equalMtimeSkips() {
        #expect(Discovery.isAlreadyCurrent(
            dbModifiedAt: 1_700_000_000, dbSize: 10,
            currentModified: 1_700_000_000, currentSize: 10))
    }

    @Test("backdated same-size edit (mtime != stored) is NOT skipped")
    func backdatedEditNotSkipped() {
        // Stored mtime M1, file backdated to M2 (M2 < last scan time but M1 != M2).
        // The old `scanned_at >= mtime` predicate skipped this; the contract-
        // matching predicate must reprocess it so new content is re-tagged.
        #expect(!Discovery.isAlreadyCurrent(
            dbModifiedAt: 1_700_000_000, dbSize: 10,
            currentModified: 1_600_000_000, currentSize: 10))
    }

    @Test("both-nil mtime matches DBWriter's both-nil unchanged")
    func bothNilSkips() {
        #expect(Discovery.isAlreadyCurrent(
            dbModifiedAt: nil, dbSize: 10, currentModified: nil, currentSize: 10))
    }

    @Test("nil-vs-present mtime is a change")
    func nilVsPresentNotSkipped() {
        #expect(!Discovery.isAlreadyCurrent(
            dbModifiedAt: nil, dbSize: 10, currentModified: 100, currentSize: 10))
        #expect(!Discovery.isAlreadyCurrent(
            dbModifiedAt: 100, dbSize: 10, currentModified: nil, currentSize: 10))
    }

    @Test("sub-microsecond mtime drift still skips")
    func tinyDriftSkips() {
        #expect(Discovery.isAlreadyCurrent(
            dbModifiedAt: 1_700_000_000.0, dbSize: 10,
            currentModified: 1_700_000_000.0 + 1e-7, currentSize: 10))
    }
}
