// C6 scan-pipeline performance regressions. These assert the *mechanism* of the
// perf fixes (work avoided / behavior preserved), not wall-clock throughput
// (which is hardware-bound and verified on the dev Mac / RTX 2060 rig):
//   - F-C6-001: discovery-time incremental skip set drops unchanged files
//     UPSTREAM of the ANE/Vision/CLIP/OCR pass; forceReprocess re-runs all.
//   - F-C6-005: streaming discovery yields the same file set as the array walk.
//   - F-C6-004: the decoupled DB-writer committer commits every batch (no loss
//     across the >2-batch boundary) — the throughput win itself is hardware.
import Testing
import Foundation
import GRDB
import AsyncAlgorithms
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database

@Suite("C6 scan pipeline perf")
struct ScanPipelinePerfTests {

    // MARK: - F-C6-001: pure skip predicate

    @Test("isAlreadyCurrent: unchanged file (same size, mtime == stored modified_at) skips")
    func skipPredicateUnchanged() {
        #expect(Discovery.isAlreadyCurrent(
            dbModifiedAt: 1_000, dbSize: 100, currentModified: 1_000, currentSize: 100))
    }

    @Test("isAlreadyCurrent: a changed on-disk mtime never skips")
    func skipPredicateModified() {
        // Mirrors DBWriter's `unchanged` contract: only an EXACT mtime match
        // counts as captured, so a newer mtime reprocesses.
        #expect(!Discovery.isAlreadyCurrent(
            dbModifiedAt: 1_000, dbSize: 100, currentModified: 1_500, currentSize: 100))
    }

    @Test("isAlreadyCurrent: a backdated same-size edit (mtime != stored) reprocesses")
    func skipPredicateBackdatedMtime() {
        // R-09: an archive extract / rsync -a / Time Machine restore can move a
        // file's mtime to a value still <= the prior scan time but != the stored
        // modified_at. The old `scanned_at >= mtime` test skipped it forever
        // (stale tags/OCR); DBWriter would have reprocessed (M1 != M2), so the
        // skip predicate must agree and NOT skip.
        #expect(!Discovery.isAlreadyCurrent(
            dbModifiedAt: 1_500, dbSize: 100, currentModified: 1_000, currentSize: 100))
    }

    @Test("isAlreadyCurrent: a size change never skips")
    func skipPredicateSize() {
        #expect(!Discovery.isAlreadyCurrent(
            dbModifiedAt: 1_000, dbSize: 100, currentModified: 1_000, currentSize: 101))
    }

    @Test("isAlreadyCurrent: a one-sided nil mtime can't prove equality → never skips")
    func skipPredicateNilMtime() {
        #expect(!Discovery.isAlreadyCurrent(
            dbModifiedAt: 1_000, dbSize: 100, currentModified: nil, currentSize: 100))
        #expect(!Discovery.isAlreadyCurrent(
            dbModifiedAt: nil, dbSize: 100, currentModified: 1_000, currentSize: 100))
        // Both-nil mtimes match DBWriter's (nil, nil) == unchanged branch.
        #expect(Discovery.isAlreadyCurrent(
            dbModifiedAt: nil, dbSize: 100, currentModified: nil, currentSize: 100))
    }

    // MARK: - F-C6-001: end-to-end discovery skip set

    /// Creates a small tree, records DB rows for a subset, and asserts discovery
    /// drops only the rows that are genuinely current — so the expensive tagging
    /// pass is never even handed those files.
    @Test("Incremental walk drops current files; failed/changed/unknown reprocess")
    func discoverySkipSet() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDSkipTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        // Use the REAL resolved root (realpath, keeps /private) so the skip-set
        // prefix range (built from root.path) matches the enumerator's
        // /private-prefixed output. Foundation's resolvingSymlinksInPath strips
        // /private and so would NOT match (the range would exclude every row).
        // In production the scan root (/Users, /Volumes) never involves /private.
        let root = realResolved(tmp)

        let names = ["a.jpg", "b.jpg", "c.jpg", "d.jpg"]
        for n in names { try Data("payload".utf8).write(to: tmp.appendingPathComponent(n)) }

        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        let discovery = Discovery()

        // Discover once with no DB to learn the exact enumerator paths + sizes
        // + on-disk mtimes (the skip predicate now matches stored modified_at to
        // the current mtime, mirroring DBWriter).
        let baseline = await discovery.walk(root: root)
        #expect(baseline.count == 4)
        var pathByNameBuild: [String: String] = [:]
        var sizeByNameBuild: [String: Int64] = [:]
        var modByNameBuild: [String: Double?] = [:]
        for f in baseline {
            pathByNameBuild[f.url.lastPathComponent] = f.url.path
            sizeByNameBuild[f.url.lastPathComponent] = f.sizeBytes
            modByNameBuild[f.url.lastPathComponent] = f.modificationDate?.timeIntervalSince1970
        }
        // Immutable bindings: the GRDB read/write closures below are @Sendable,
        // and capturing a `var` is an error under Swift 6 strict concurrency.
        let pathByName = pathByNameBuild
        let sizeByName = sizeByNameBuild
        let modByName = modByNameBuild

        // scanned_at far in the future: proves the R-08 touch fired (it bumps
        // scanned_at DOWN to the current scan time for the skipped row).
        let future = Date().timeIntervalSince1970 + 1_000_000
        func insert(name: String, size: Int64, modified: Double?, failed: Int) throws {
            try db.pool.write { d in
                try d.execute(sql: """
                    INSERT INTO files (path_text, path_hash, size_bytes, modified_at,
                                       scanned_at, kind, extension, failed)
                    VALUES (?, 0, ?, ?, ?, 'image', 'jpg', ?)
                    """, arguments: [pathByName[name]!, size, modified, future, failed])
            }
        }
        // a: current (size + mtime match) → MUST be skipped. b: wrong size →
        // reprocess. c: prior failure (failed=1, excluded from the set) →
        // reprocess. d: no row at all → reprocess.
        try insert(name: "a.jpg", size: sizeByName["a.jpg"]!,     modified: modByName["a.jpg"]!, failed: 0)
        try insert(name: "b.jpg", size: sizeByName["b.jpg"]! + 1, modified: modByName["b.jpg"]!, failed: 0)
        try insert(name: "c.jpg", size: sizeByName["c.jpg"]!,     modified: modByName["c.jpg"]!, failed: 1)
        // a.jpg is an image and the skip candidate: give it a CLIP embedding so
        // the R-14 backfill carve-out (active whenever a CLIP model is installed
        // on the host — true on a dev box that ran a real scan) doesn't force the
        // embeddingless image to stay in the pipeline. Without this the test is
        // green only on a CLIP-less runner — env-fragile.
        try await db.pool.write { d in
            let aID = try Int64.fetchOne(
                d, sql: "SELECT id FROM files WHERE path_text = ?",
                arguments: [pathByName["a.jpg"]!])!
            try d.execute(
                sql: "INSERT INTO clip_embeddings (file_id, embedding, model) VALUES (?, ?, 'test')",
                arguments: [aID, Data(count: 2048)])
        }

        let incremental = await discovery.walk(root: root, database: db, forceReprocess: false)
        let got = Set(incremental.map { $0.url.lastPathComponent })
        #expect(got == ["b.jpg", "c.jpg", "d.jpg"], "only the genuinely-current file is skipped")

        // R-08: the skipped row's scanned_at must be bumped to the current scan
        // time (touched down from `future`), so the post-scan orphan sweep —
        // which treats `scanned_at < scanStart` as deleted — counts it present.
        let touchedA = try await db.pool.read { d in
            try Double.fetchOne(d, sql: "SELECT scanned_at FROM files WHERE path_text = ?",
                                arguments: [pathByName["a.jpg"]!])
        }
        #expect((touchedA ?? .greatestFiniteMagnitude) < future,
                "skipped file's scanned_at was bumped to the scan time, not left stale")

        // forceReprocess empties the skip set: every file flows through again.
        let forced = await discovery.walk(root: root, database: db, forceReprocess: true)
        #expect(Set(forced.map { $0.url.lastPathComponent }) == Set(names))
    }

    // MARK: - F-C6-005: streaming yields the same set as the array walk

    @Test("walkStreaming emits exactly the files walk returns")
    func streamingMatchesWalk() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDStreamTest-\(UUID().uuidString)")
        let sub = tmp.appendingPathComponent("nested")
        try FileManager.default.createDirectory(at: sub, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        for n in ["x.png", "y.pdf"] { try Data("z".utf8).write(to: tmp.appendingPathComponent(n)) }
        for n in ["m.jpg", "n.mp4"] { try Data("z".utf8).write(to: sub.appendingPathComponent(n)) }

        let discovery = Discovery()
        let arrayWalk = await discovery.walk(root: tmp)

        let box = StreamBox()
        await discovery.walkStreaming(root: tmp) { file in
            await box.append(file.url.path)
        }
        let streamed = await box.paths

        #expect(Set(streamed) == Set(arrayWalk.map { $0.url.path }))
        #expect(streamed.count == arrayWalk.count, "no duplicates while streaming")
    }

    private actor StreamBox {
        private(set) var paths: [String] = []
        func append(_ p: String) { paths.append(p) }
    }

    // MARK: - F-C6-004: decoupled committer loses no rows across batch boundaries

    @Test("Draining >2 batches through the decoupled committer commits every row")
    func decoupledCommitNoLoss() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDDrainTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        let writer = DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(),
                              sessionID: UUID().uuidString)

        let total = 250   // > 2 × the 100-file batch ceiling
        let channel = AsyncChannel<TaggedFile>()
        let producer = Task {
            for i in 0..<total {
                await channel.send(TaggedFile(
                    url: tmp.appendingPathComponent("f\(i).jpg"),
                    kind: "image", extension: "jpg", sizeBytes: 1,
                    createdAt: nil, modifiedAt: Date(timeIntervalSince1970: 1_700_000_000)))
            }
            channel.finish()
        }
        await writer.drain(channel)
        await producer.value

        let count = try await db.pool.read { d in
            try Int.fetchOne(d, sql: "SELECT COUNT(*) FROM files") ?? -1
        }
        #expect(count == total, "every file committed across the decoupled batch handoff")
    }
}
