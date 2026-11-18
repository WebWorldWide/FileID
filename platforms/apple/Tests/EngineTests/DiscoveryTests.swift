// Discovery smoke test. Creates a temporary directory tree of taggable +
// non-taggable files, runs Discovery.walk, asserts the right files were
// returned in the right (sorted) order with the right kinds.
import Testing
import Foundation
import GRDB
import FileIDShared
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database

@Suite("Discovery")
struct DiscoveryTests {

    private final class ProgressCapture: @unchecked Sendable {
        private let lock = NSLock()
        private var values: [Int] = []

        func record(_ value: Int) {
            lock.lock()
            values.append(value)
            lock.unlock()
        }

        func snapshot() -> [Int] {
            lock.lock()
            defer { lock.unlock() }
            return values
        }
    }

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
            tmp.appendingPathComponent("d.mp4"),
            tmp.appendingPathComponent("notes.xyz"),        // unknown ext → other
            tmp.appendingPathComponent("README")            // no ext → other
        ]
        // Files we expect to be filtered out.
        let badFiles = [
            tmp.appendingPathComponent(".hidden.jpg"),     // hidden
            tmp.appendingPathComponent("movie.mp4.part"),   // unfinished download
            tmp.appendingPathComponent("Thumbs.db")         // system file
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
        #expect(byExt["xyz"] == .other)
        #expect(byExt[""] == .other)
    }

    @Test("Streaming discovery publishes small-library progress before completion")
    func streamingPublishesEarlyProgress() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDDiscoveryProgress-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        for index in 0..<20 {
            try Data("image".utf8).write(to: tmp.appendingPathComponent("\(index).jpg"))
        }

        let capture = ProgressCapture()
        let total = await Discovery().walkStreaming(
            root: tmp,
            progress: { capture.record($0) }
        ) { _ in }

        #expect(total == 20)
        #expect(capture.snapshot() == [1, 17])
    }

    @Test("Skips large documents and images but keeps large streamed video")
    func skipsLargeFiles() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDLargeTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        try Data("x".utf8).write(to: tmp.appendingPathComponent("small.jpg"))
        let twoMB = Data(repeating: 0, count: 2 * 1024 * 1024)
        try twoMB.write(to: tmp.appendingPathComponent("big.mp4"))
        try twoMB.write(to: tmp.appendingPathComponent("big.tif"))

        let discovery = Discovery()
        let result = await discovery.walk(root: tmp, maxSizeMB: 1)

        #expect(Set(result.map { $0.url.lastPathComponent }) == ["small.jpg", "big.mp4"])
    }

    @Test("Plain-text data files are discovered as documents")
    func discoversTextData() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDTextTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        for name in ["a.csv", "b.json", "c.yaml", "d.log", "e.toml"] {
            try Data("x".utf8).write(to: tmp.appendingPathComponent(name))
        }
        let result = await Discovery().walk(root: tmp)
        #expect(result.count == 5)
        #expect(FileTypes.kind(forExtension: "json") == .doc)
    }

    @Test("Prunes user-excluded folders while keeping siblings")
    func prunesExcludedFolders() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDExcludeTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let root = realResolved(tmp)
        let keep = root.appendingPathComponent("keep")
        let excluded = root.appendingPathComponent("cache")
        let nestedExcluded = excluded.appendingPathComponent("nested")
        try FileManager.default.createDirectory(at: keep, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: nestedExcluded, withIntermediateDirectories: true)

        let keptFile = keep.appendingPathComponent("photo.jpg")
        let excludedFile = excluded.appendingPathComponent("drop.jpg")
        let nestedFile = nestedExcluded.appendingPathComponent("also-drop.png")
        let rootFile = root.appendingPathComponent("root.pdf")
        let payload = Data("hello".utf8)
        for url in [keptFile, excludedFile, nestedFile, rootFile] {
            try payload.write(to: url)
        }

        let discovery = Discovery()
        let result = await discovery.walk(
            root: root,
            excludedPaths: [excluded.path, root.path, "/definitely/outside/root"])
        let resultPaths = Set(result.map { $0.url.resolvingSymlinksInPath().path })

        #expect(resultPaths.contains(keptFile.resolvingSymlinksInPath().path))
        #expect(resultPaths.contains(rootFile.resolvingSymlinksInPath().path))
        #expect(!resultPaths.contains(excludedFile.resolvingSymlinksInPath().path))
        #expect(!resultPaths.contains(nestedFile.resolvingSymlinksInPath().path))
    }

    // A file the incremental skip set drops is still on disk, but its row is left alone: a
    // rescan with nothing new must not rewrite every row. The walk reports such files as
    // present, so the orphan sweep does not take their old `scanned_at` for a deletion.
    @Test("A skipped unchanged file keeps its row untouched and is reported as present")
    func skippedFileRowIsUntouched() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDSkipTouch-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))

        // realpath (not resolvingSymlinksInPath, which strips /private): the enumerator reports
        // /private-prefixed paths for a /var temp dir, and the skip range must agree.
        let root = realResolved(tmp)

        // A fully-processed doc: it already HAS its BGE text_embedding (seeded below),
        // so it's skippable regardless of whether BGE is installed. The carve-outs
        // force only an embeddingLESS image (CLIP) or doc/pdf (BGE) to stay in the
        // pipeline for backfill; without the embedding row this test would flake on a
        // machine that has BGE installed (the carve-out would keep report.pdf resident).
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
            // Give it a text_embeddings row so the doc carve-out treats it as fully
            // processed → the "unchanged → skipped" path is what's exercised here.
            try conn.execute(sql: """
                INSERT INTO text_embeddings (file_id, embedding, model)
                VALUES (?, ?, ?)
                """, arguments: [
                    conn.lastInsertedRowID, Data([0, 0, 0, 0]), "bge_small_en_v1_5"
                ])
        }

        let discovery = Discovery()
        let result = await discovery.walk(root: root, database: db, forceReprocess: false)

        #expect(!result.contains { $0.url.lastPathComponent == "report.pdf" })

        let after: Double = try await db.pool.read { conn in
            try Double.fetchOne(conn, sql:
                "SELECT scanned_at FROM files WHERE path_text = ?",
                arguments: [doc.path]) ?? -1
        }
        #expect(after == oldScannedAt)
        #expect(await discovery.presentUnchanged == [Discovery.PathFingerprint(doc.path)])
        let presentUnchanged = await discovery.presentUnchanged
        let candidates = try await db.pool.read { conn in
            try FileIDEngineMain.orphanCandidates(
                conn, scanRootPath: root.path, scanStart: Date(),
                presentUnchanged: presentUnchanged, cap: 10)
        }
        #expect(candidates.isEmpty)
    }

    @Test("The orphan sweep still finds the row of a file that is gone, past a crowd of present unchanged ones")
    func orphanCandidatesSkipPresentRows() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDOrphanCandidates-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        let root = "/lib"
        let present = Set((0..<20).map { Discovery.PathFingerprint("/lib/present\($0).jpg") })
        try await db.pool.write { conn in
            func insert(_ path: String, scannedAt: Double) throws {
                try conn.execute(sql: """
                    INSERT INTO files (path_text, path_hash, path_search, size_bytes, scanned_at, kind, extension)
                    VALUES (?, 0, ?, 1, ?, 'image', 'jpg')
                    """, arguments: [path, path, scannedAt])
            }
            for i in 0..<20 { try insert("/lib/present\(i).jpg", scannedAt: 1_000) }
            try insert("/lib/gone.jpg", scannedAt: 1_000)
            try insert("/lib/written-this-scan.jpg", scannedAt: 5_000)
            try insert("/elsewhere/other.jpg", scannedAt: 1_000)
        }

        let candidates = try await db.pool.read { conn in
            try FileIDEngineMain.orphanCandidates(
                conn, scanRootPath: root, scanStart: Date(timeIntervalSince1970: 2_000),
                presentUnchanged: present, cap: 5)
        }

        #expect(candidates.map(\.path) == ["/lib/gone.jpg"])
    }

    @Test("iCloud-only files are present for the orphan sweep but are not counted as unchanged files")
    func cloudOnlyFilesAreNotUnchanged() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDCloudOnly-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        let library = tmp.appendingPathComponent("library")
        try FileManager.default.createDirectory(at: library, withIntermediateDirectories: true)
        let root = realResolved(library)
        let current = root.appendingPathComponent("current.pdf")
        let cloud = root.appendingPathComponent("cloud.pdf")
        let bytes = Data("hello".utf8)
        try bytes.write(to: current)
        try bytes.write(to: cloud)
        let mtime = Date(timeIntervalSince1970: 1_700_000_000)
        for url in [current, cloud] {
            try FileManager.default.setAttributes([.modificationDate: mtime], ofItemAtPath: url.path)
        }
        try await db.pool.write { conn in
            try conn.execute(sql: """
                INSERT INTO files (path_text, path_hash, path_search, size_bytes, modified_at, scanned_at, kind, extension)
                VALUES (?, 0, ?, ?, ?, 1000, 'doc', 'pdf')
                """, arguments: [current.path, current.path, Int(bytes.count), mtime.timeIntervalSince1970])
            try conn.execute(sql: """
                INSERT INTO text_embeddings (file_id, embedding, model)
                VALUES (?, ?, ?)
                """, arguments: [conn.lastInsertedRowID, Data([0, 0, 0, 0]), "bge_small_en_v1_5"])
        }

        let discovery = Discovery()
        await discovery.simulateCloudOnly(names: ["cloud.pdf"])
        let keptFiles = await discovery.walk(root: root, database: db, forceReprocess: false)
        let kept = keptFiles.count

        #expect(kept == 0)
        #expect(await discovery.presentUnchanged == [
            Discovery.PathFingerprint(current.path), Discovery.PathFingerprint(cloud.path)])
        #expect(await discovery.currentUnchangedCount == 1)
        #expect(await discovery.lastShortfall.cloudOnlyFiles == 1)

        let onlyCloud = Discovery()
        await onlyCloud.simulateCloudOnly(names: ["cloud.pdf", "current.pdf"])
        try FileManager.default.setAttributes([.modificationDate: Date()], ofItemAtPath: current.path)
        _ = await onlyCloud.walk(root: root, database: db, forceReprocess: false)
        #expect(await onlyCloud.currentUnchangedCount == 0)
    }

    @Test("A walk that kept nothing but found current files says nothing changed; other walks do not")
    func rescanNoChangesNotice() {
        let clean = Discovery.Shortfall()
        let notice = Discovery.rescanNoChangesNotice(kept: 0, shortfall: clean, unchangedFiles: 185, root: "/lib")
        #expect(notice?.kind == "rescan_no_changes")
        #expect(notice?.path == "/lib")
        #expect(Discovery.rescanNoChangesNotice(kept: 1, shortfall: clean, unchangedFiles: 185, root: "/lib") == nil)
        #expect(Discovery.rescanNoChangesNotice(kept: 0, shortfall: clean, unchangedFiles: 0, root: "/lib") == nil)
        var partial = Discovery.Shortfall()
        partial.unreadablePaths = 1
        #expect(Discovery.rescanNoChangesNotice(kept: 0, shortfall: partial, unchangedFiles: 185, root: "/lib") == nil)
    }
}

// The incremental-skip predicate must match DBWriter's `unchanged`
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

@Suite("File type tables")
struct FileTypeTableTests {
    @Test func commonCameraAndAudioFormatsAreIndexed() {
        for ext in ["wma", "cr3", "avif", "mxf", "vob", "caf", "AIF"] {
            #expect(FileTypes.isTaggable(ext), "\(ext)")
        }
        #expect(FileTypes.kind(forExtension: "wma") == .audio)
        #expect(FileTypes.kind(forExtension: "cr3") == .image)
        #expect(FileTypes.kind(forExtension: "vob") == .video)
        #expect(!FileTypes.isTaggable("iso"))
    }

    @Test("A symlinked scan root walks its target in the same form the walk reports children")
    func symlinkedRootCanonicalizes() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDSymlinkRoot-\(UUID().uuidString)")
        let real = tmp.appendingPathComponent("real")
        let link = tmp.appendingPathComponent("link")
        try FileManager.default.createDirectory(at: real, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        try Data("x".utf8).write(to: real.appendingPathComponent("a.jpg"))
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: real)

        let canonical = ScanRootGuard.canonicalPath(link.path)
        #expect(canonical.hasPrefix("/private/"))
        #expect(canonical.hasSuffix("/real"))
        #expect(ScanRootGuard.canonicalPath(canonical) == canonical)

        let result = await Discovery().walk(root: URL(fileURLWithPath: canonical))
        #expect(result.map(\.url.path) == [canonical + "/a.jpg"],
                "children must share the root's form so prefix keys match stored rows")

        let skip = real.appendingPathComponent("Skip")
        try FileManager.default.createDirectory(at: skip, withIntermediateDirectories: true)
        try Data("y".utf8).write(to: skip.appendingPathComponent("b.jpg"))
        let pruned = await Discovery().walk(root: URL(fileURLWithPath: canonical),
                                            excludedPaths: [link.path + "/Skip"])
        #expect(pruned.map(\.url.path) == [canonical + "/a.jpg"])
        #expect(Discovery.resolvedExclusionPaths(root: URL(fileURLWithPath: canonical), rawPaths: [link.path + "/Skip"])
                == [(canonical + "/skip").lowercased()],
                "purge needles must use the stored-row form, /private included")
    }
}

@Suite("Discovery shortfall notice")
struct DiscoveryShortfallTests {
    @Test("unreadable folders and oversized files produce one plain notice; a clean walk none",
          .enabled(if: getuid() != 0, "root can read a mode 000 directory"))
    func shortfall() async throws {
        let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDShortfall-\(UUID().uuidString)")
        let locked = tmp.appendingPathComponent("locked")
        try FileManager.default.createDirectory(at: locked, withIntermediateDirectories: true)
        defer {
            try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: locked.path)
            try? FileManager.default.removeItem(at: tmp)
        }
        try Data("x".utf8).write(to: tmp.appendingPathComponent("a.jpg"))
        try Data(count: 2 * 1024 * 1024).write(to: tmp.appendingPathComponent("huge.tif"))
        try Data("y".utf8).write(to: locked.appendingPathComponent("b.jpg"))
        try FileManager.default.setAttributes([.posixPermissions: 0o000], ofItemAtPath: locked.path)

        let discovery = Discovery()
        _ = await discovery.walk(root: tmp, maxSizeMB: 1)
        let shortfall = await discovery.lastShortfall
        #expect(shortfall.oversizedFiles == 1)
        #expect(shortfall.unreadablePaths >= 1)
        let notice = try #require(Discovery.shortfallNotice(shortfall, maxSizeMB: 1))
        #expect(notice.kind == "discovery_partial")
        #expect(notice.message.contains("over 1 MB") && notice.message.contains("couldn't be opened"))
        #expect(Discovery.shortfallNotice(Discovery.Shortfall()) == nil)
    }
}

@Suite("Discovery enumerator fallback")
struct DiscoveryEnumeratorFallbackTests {

    private func makeTree() throws -> (root: URL, expected: [String]) {
        let base = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDFallback-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        var resolved: String
        if let real = realpath(base.path, nil) { resolved = String(cString: real); free(real) } else { resolved = base.path }
        let root = URL(fileURLWithPath: resolved)
        let payload = Data("x".utf8)
        let relative = [
            "top.jpg", "Photos/a.jpg", "Photos/b.jpg", "Photos/c.pdf", "Photos/.hidden.jpg",
            "Photos/Party/d.jpg", "Photos/Party/e.png", "Photos/Party/Deep/f.jpg",
            "Photos/Aaa/g.jpg", "Other/h.jpg",
        ]
        var expected: [String] = []
        for path in relative {
            let url = root.appendingPathComponent(path)
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try payload.write(to: url)
            if !url.lastPathComponent.hasPrefix(".") { expected.append(url.path) }
        }
        return (root, expected.sorted())
    }

    @Test("A directory the enumerator gives up on is listed again, once, with its subtree")
    func fallbackKeepsSubtreeWithoutDuplicates() async throws {
        let (root, expected) = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let discovery = Discovery()
        let failing: Set<String> = [
            root.appendingPathComponent("Photos").path,
            root.appendingPathComponent("Photos/Party").path,
        ]
        await discovery.simulateEnumeratorFailures(in: failing)

        let found = await discovery.walk(root: root).map(\.url.path)

        #expect(found.sorted() == expected)
        #expect(Set(found).count == found.count)
        #expect(await discovery.lastShortfall.unreadablePaths == 0)
    }

    @Test("Streaming walk with a failed directory reports the same count as a clean walk")
    func streamingFallbackCount() async throws {
        let (root, expected) = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let discovery = Discovery()
        await discovery.simulateEnumeratorFailures(in: [root.appendingPathComponent("Photos").path])
        let kept = await discovery.walkStreaming(root: root) { _ in }
        #expect(kept == expected.count)
    }

    @Test("The fallback still honors folder exclusions")
    func fallbackHonorsExclusions() async throws {
        let (root, expected) = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let discovery = Discovery()
        await discovery.simulateEnumeratorFailures(in: [root.appendingPathComponent("Photos").path])
        let excluded = root.appendingPathComponent("Photos/Party").path

        let found = await discovery.walk(root: root, excludedPaths: [excluded]).map(\.url.path)

        #expect(found.sorted() == expected.filter { !$0.hasPrefix(excluded + "/") })
    }

    @Test("Siblings the enumerator still yields after reporting a failure are not listed twice")
    func laterSiblingsAreYieldedOnce() async throws {
        let (root, expected) = try makeTree()
        defer { try? FileManager.default.removeItem(at: root) }
        let discovery = Discovery()
        await discovery.simulateEnumeratorFailures(
            in: [root.appendingPathComponent("Photos").path, root.appendingPathComponent("Photos/Party").path],
            keepYielding: true)

        let found = await discovery.walk(root: root).map(\.url.path)

        #expect(found.sorted() == expected)
        #expect(Set(found).count == found.count)
    }

    @Test("A failure reported for the same directory twice lists it once")
    func repeatedFailureListsOnce() {
        let record = Discovery.EnumerationRecord()
        let photos = URL(fileURLWithPath: "/lib/Photos")
        record.noteYield(of: photos.appendingPathComponent("a.jpg"), parent: photos.path)
        record.noteFailure(of: photos)
        record.noteFailure(of: photos)
        record.noteYield(of: photos.appendingPathComponent("b.jpg"), parent: photos.path)

        #expect(record.failures().map(\.path) == ["/lib/Photos"])
        #expect(record.covered(photos) == ["a.jpg", "b.jpg"])
    }

    @Test("The error handler and the enumerator may spell a symlinked prefix differently")
    func symlinkedPrefixIsOneDirectory() {
        let record = Discovery.EnumerationRecord()
        let viaPrivate = URL(fileURLWithPath: "/private/tmp/lib/Photos")
        let viaLink = URL(fileURLWithPath: "/tmp/lib/Photos")
        record.noteYield(of: viaPrivate.appendingPathComponent("a.jpg"), parent: viaPrivate.path)
        record.noteFailure(of: viaLink)
        record.noteYield(of: viaPrivate.appendingPathComponent("b.jpg"), parent: viaPrivate.path)
        record.noteFailure(of: viaPrivate)

        #expect(record.failures().count == 1)
        #expect(record.covered(viaLink) == ["a.jpg", "b.jpg"])
        #expect(record.covered(viaPrivate) == ["a.jpg", "b.jpg"])
    }

    @Test("A directory the fallback cannot list either is counted as unreadable",
          .enabled(if: getuid() != 0, "root can read a mode 000 directory"))
    func fallbackFailureIsCounted() async throws {
        let (root, expected) = try makeTree()
        let locked = root.appendingPathComponent("Other")
        try FileManager.default.setAttributes([.posixPermissions: 0o000], ofItemAtPath: locked.path)
        defer {
            try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: locked.path)
            try? FileManager.default.removeItem(at: root)
        }
        let discovery = Discovery()
        let found = await discovery.walk(root: root).map(\.url.path)
        #expect(found.sorted() == expected.filter { !$0.contains("/Other/") })
        #expect(await discovery.lastShortfall.unreadablePaths == 1)
    }
}
