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

@Suite("C6 scan pipeline perf")
struct ScanPipelinePerfTests {

    // MARK: - F-C6-001: pure skip predicate

    @Test("isAlreadyCurrent: unchanged file (same size, scanned after mtime) skips")
    func skipPredicateUnchanged() {
        #expect(Discovery.isAlreadyCurrent(
            dbScannedAt: 2_000, dbSize: 100, currentModified: 1_000, currentSize: 100))
        // Exactly-equal scanned_at == modified is still "captured".
        #expect(Discovery.isAlreadyCurrent(
            dbScannedAt: 1_000, dbSize: 100, currentModified: 1_000, currentSize: 100))
    }

    @Test("isAlreadyCurrent: a newer on-disk mtime than scanned_at never skips")
    func skipPredicateModified() {
        #expect(!Discovery.isAlreadyCurrent(
            dbScannedAt: 1_000, dbSize: 100, currentModified: 1_500, currentSize: 100))
    }

    @Test("isAlreadyCurrent: a size change never skips")
    func skipPredicateSize() {
        #expect(!Discovery.isAlreadyCurrent(
            dbScannedAt: 2_000, dbSize: 100, currentModified: 1_000, currentSize: 101))
    }

    @Test("isAlreadyCurrent: a missing on-disk mtime can't prove unchanged → never skips")
    func skipPredicateNilMtime() {
        #expect(!Discovery.isAlreadyCurrent(
            dbScannedAt: 2_000, dbSize: 100, currentModified: nil, currentSize: 100))
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
        // Use the resolved root so the skip-set prefix range (built from
        // root.path) matches the enumerator's symlink-resolved paths — in
        // production the scan root arrives already resolved from the app side.
        let root = tmp.resolvingSymlinksInPath()

        let names = ["a.jpg", "b.jpg", "c.jpg", "d.jpg"]
        for n in names { try Data("payload".utf8).write(to: tmp.appendingPathComponent(n)) }

        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        let discovery = Discovery()

        // Discover once with no DB to learn the exact enumerator paths + sizes.
        let baseline = await discovery.walk(root: root)
        #expect(baseline.count == 4)
        var pathByName: [String: String] = [:]
        var sizeByName: [String: Int64] = [:]
        for f in baseline {
            pathByName[f.url.lastPathComponent] = f.url.path
            sizeByName[f.url.lastPathComponent] = f.sizeBytes
        }

        // scanned_at far in the future guarantees scanned_at >= on-disk mtime.
        let future = Date().timeIntervalSince1970 + 1_000_000
        func insert(name: String, size: Int64, failed: Int) throws {
            try db.pool.write { d in
                try d.execute(sql: """
                    INSERT INTO files (path_text, path_hash, size_bytes, scanned_at,
                                       kind, extension, failed)
                    VALUES (?, 0, ?, ?, 'image', 'jpg', ?)
                    """, arguments: [pathByName[name]!, size, future, failed])
            }
        }
        // a: current → MUST be skipped. b: wrong size → reprocess.
        // c: prior failure (failed=1, excluded from the set) → reprocess.
        // d: no row at all → reprocess.
        try insert(name: "a.jpg", size: sizeByName["a.jpg"]!,     failed: 0)
        try insert(name: "b.jpg", size: sizeByName["b.jpg"]! + 1, failed: 0)
        try insert(name: "c.jpg", size: sizeByName["c.jpg"]!,     failed: 1)

        let incremental = await discovery.walk(root: root, database: db, forceReprocess: false)
        let got = Set(incremental.map { $0.url.lastPathComponent })
        #expect(got == ["b.jpg", "c.jpg", "d.jpg"], "only the genuinely-current file is skipped")

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
