// Butler restructure parity tests — pin the macOS engine's rule cascade,
// folder classification, and apply guards against the Windows engine
// (restructure.rs / restructure_apply.rs), the source of truth for behavior.
import Testing
import Foundation
import GRDB
import FileIDShared
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database

@Suite("Restructure rule cascade + folder classification")
struct RestructureCascadeTests {

    private let root = URL(fileURLWithPath: "/Library")
    // 2024-03-15 12:00:00 UTC — comfortably mid-month so UTC year/month is stable.
    private let ts = 1_710_504_000.0

    private func f(
        _ id: Int64, _ kind: String, source: String? = nil,
        hasText: Bool = false, person: String? = nil,
        lat: Double? = nil, lon: Double? = nil,
        modified: Double? = nil, created: Double? = nil
    ) -> Restructure.FileForClassify {
        Restructure.FileForClassify(
            fileID: id, source: source ?? "/in/file\(id).\(kind)", kind: kind,
            modifiedUnix: modified ?? ts, createdUnix: created,
            personName: person, lat: lat, lon: lon, hasText: hasText)
    }

    @Test("monthName(6) == June (full English month names)")
    func monthNameFull() {
        #expect(Restructure.monthName(6) == "June")
        #expect(Restructure.monthName(1) == "January")
        #expect(Restructure.monthName(12) == "December")
    }

    @Test("Wire category strings match the Windows lowercase vocabulary")
    func categoryVocabulary() {
        let moves = Restructure.ruleClassify(
            [f(1, "image"), f(2, "video"), f(3, "audio"), f(4, "pdf"), f(5, "other")],
            libraryRoot: root)
        let cat = Dictionary(uniqueKeysWithValues: moves.map { ($0.fileID, $0.bucket) })
        #expect(cat[1] == "photo")
        #expect(cat[2] == "video")
        #expect(cat[3] == "audio")
        #expect(cat[4] == "document")
        #expect(cat[5] == "misc")
    }

    @Test("A dated video routes to Videos/<Year>, dated audio to Audio")
    func videoAudioBuckets() {
        let moves = Restructure.ruleClassify(
            [f(1, "video"), f(2, "audio")], libraryRoot: root)
        let vid = moves.first { $0.fileID == 1 }!
        #expect(vid.newPath.contains("/Videos/2024/"))
        #expect(!vid.newPath.contains("March"), "videos have no month: \(vid.newPath)")
        #expect(vid.bucket == "video")
        let aud = moves.first { $0.fileID == 2 }!
        #expect(aud.newPath.contains("/Audio/"))
        #expect(!aud.newPath.contains("2024"), "audio has no year: \(aud.newPath)")
        #expect(aud.bucket == "audio")
    }

    @Test("A dated image routes to Photos/<Year>/<MonthName>")
    func imageYearMonth() {
        let moves = Restructure.ruleClassify([f(1, "image")], libraryRoot: root)
        #expect(moves[0].newPath.contains("/Photos/2024/March"))
        #expect(moves[0].bucket == "photo")
    }

    @Test("A file with no timestamp coerces to the 1970 year bucket (Windows)")
    func missingTimestampYear() {
        #expect(Restructure.yearMonth(0).year == 1970)
        #expect(Restructure.yearMonth(0).month == 1)
        // modifiedUnix 0, no createdUnix → ts 0 → 1970.
        let moves = Restructure.ruleClassify(
            [f(1, "image", modified: 0)], libraryRoot: root)
        #expect(moves[0].newPath.contains("/Photos/1970/"))
    }

    @Test("Anchor-folder files emit no move proposals (classify + strip)")
    func anchorFolderStrip() {
        // Three same-kind photos in one well-named folder → that folder is an
        // Anchor (>=80% one category, >2 files, non-generic name).
        let files = (0..<3).map { i in
            f(Int64(i), "image", source: "/Library/Vacation2019/\(i).jpg")
        }
        let moves = Restructure.ruleClassify(files, libraryRoot: root)
        #expect(moves.count == 3)
        let classified = Restructure.classifyFolders(moves)
        #expect(classified.contains { $0.classification == .anchor })
        let kept = Restructure.stripAnchorFolderMovesExcept(
            moves, classified: classified, exempt: [])
        #expect(kept.isEmpty, "anchor-folder moves must drop: \(kept)")
        // Exempting the source folder (a semantic-claimed relocation) keeps them.
        let keptExempt = Restructure.stripAnchorFolderMovesExcept(
            moves, classified: classified, exempt: ["/Library/Vacation2019"])
        #expect(keptExempt.count == 3)
    }

    @Test("Mixed-tier homogeneity is measured against the dominant person")
    func mixedHomogeneityDominantPerson() {
        // A folder dominated by Alice (5) with one Bob outlier (6 files). The bug
        // measured homogeneity against a non-dominant person, flagging most of
        // the folder as outliers. The dominant category must be the dominant
        // PERSON (Alice), not Bob.
        var files: [Restructure.FileForClassify] = []
        for i in 0..<5 {
            files.append(f(Int64(i), "image", source: "/Library/Family/\(i).jpg", person: "Alice"))
        }
        files.append(f(99, "image", source: "/Library/Family/bob.jpg", person: "Bob"))
        let moves = Restructure.ruleClassify(files, libraryRoot: root)
        let classified = Restructure.classifyFolders(moves)
        let family = classified.first { $0.sourceFolder == "/Library/Family" }
        #expect(family?.dominantCategory == "People/Alice")
        // 5/6 ≈ 0.83 ≥ 0.80 → Anchor (homogeneity measured against Alice, not Bob).
        #expect(family?.classification == .anchor)
    }
}

@Suite("Restructure apply guards")
struct RestructureApplyTests {

    private func makeDB(_ tmp: URL) throws -> Database {
        try Database(at: tmp.appendingPathComponent("test.sqlite"))
    }

    private func insertRow(_ db: Database, id: Int64, path: String) async throws {
        try await db.pool.write { d in
            try d.execute(
                sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension) VALUES (?,?,?,4,0,'image','jpg')",
                arguments: [id, path, StablePathHash.hash(path)])
        }
    }

    /// F-C3-009 + F-C3-011: two proposals to the SAME destination both apply via
    /// uniquified names (no skipped-conflict), and each moved row's path_hash is
    /// refreshed to StablePathHash(newPath).
    @Test("Two proposals to one dest both apply (uniquified); path_hash refreshed")
    func applyUniquifyAndPathHash() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDRestructure-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        let aDir = root.appendingPathComponent("a")
        let bDir = root.appendingPathComponent("b")
        try FileManager.default.createDirectory(at: aDir, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: bDir, withIntermediateDirectories: true)
        let srcA = aDir.appendingPathComponent("IMG_0001.jpg")
        let srcB = bDir.appendingPathComponent("IMG_0001.jpg")
        try Data("AAAA".utf8).write(to: srcA)
        try Data("BBBB".utf8).write(to: srcB)

        let db = try makeDB(tmp)
        try await insertRow(db, id: 1, path: srcA.path)
        try await insertRow(db, id: 2, path: srcB.path)

        let dest = root.appendingPathComponent("Sorted").appendingPathComponent("IMG_0001.jpg")
        let result = await Restructure.apply(
            proposals: [
                RestructureProposal(fileID: 1, oldPath: srcA.path, newPath: dest.path, bucket: "photo"),
                RestructureProposal(fileID: 2, oldPath: srcB.path, newPath: dest.path, bucket: "photo"),
            ],
            database: db, libraryRoot: root)

        #expect(result.moved == 2)
        #expect(result.failed == 0)
        #expect(result.conflicts.isEmpty)
        let first = root.appendingPathComponent("Sorted/IMG_0001.jpg")
        let second = root.appendingPathComponent("Sorted/IMG_0001 (2).jpg")
        #expect(FileManager.default.fileExists(atPath: first.path))
        #expect(FileManager.default.fileExists(atPath: second.path))

        // Each row's path_hash must equal StablePathHash of its (new) path_text.
        let rows: [(String, Int64)] = try await db.pool.read { d in
            try Row.fetchAll(d, sql: "SELECT path_text, path_hash FROM files")
                .map { ($0["path_text"], $0["path_hash"]) }
        }
        let sortedPrefix = root.appendingPathComponent("Sorted").path
        #expect(rows.count == 2)
        for (pt, h) in rows {
            #expect(pt.hasPrefix(sortedPrefix))
            #expect(h == StablePathHash.hash(pt))
        }
    }

    /// F-C3-010: a move whose live DB path no longer matches the proposal's
    /// oldPath (a stale plan) is counted failed and NOT executed.
    @Test("A stale move (live path != oldPath) is failed, not executed")
    func applyStalePlanGuard() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDRestructure-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let real = root.appendingPathComponent("real.jpg")
        try Data("data".utf8).write(to: real)

        let db = try makeDB(tmp)
        // The DB says file 1 lives at `real`; the stale plan claims another source.
        try await insertRow(db, id: 1, path: real.path)

        let staleSrc = root.appendingPathComponent("vanished.jpg")
        let dest = root.appendingPathComponent("Sorted/x.jpg")
        let result = await Restructure.apply(
            proposals: [RestructureProposal(
                fileID: 1, oldPath: staleSrc.path, newPath: dest.path, bucket: "photo")],
            database: db, libraryRoot: root)

        #expect(result.moved == 0)
        #expect(result.failed == 1)
        #expect(FileManager.default.fileExists(atPath: real.path), "the real file is untouched")
        #expect(!FileManager.default.fileExists(atPath: dest.path))
    }

    /// F-C3-012: when the on-disk move succeeds but the DB UPDATE fails, the move
    /// is counted ONCE (moved), never double-counted as moved+failed; the file is
    /// at its new path and a recovery record is written (best-effort sidecar).
    @Test("UPDATE-after-move failure is counted once, not double-counted")
    func applyDbFailureNoDoubleCount() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDRestructure-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let src = root.appendingPathComponent("src.jpg")
        try Data("x".utf8).write(to: src)

        let db = try makeDB(tmp)
        try await insertRow(db, id: 1, path: src.path)
        // Force the post-move UPDATE to throw (the B4 SELECT still succeeds).
        try await db.pool.write { d in
            try d.execute(sql: """
                CREATE TRIGGER reject_files_update BEFORE UPDATE ON files
                BEGIN SELECT RAISE(ABORT, 'no updates'); END
                """)
        }

        let dest = root.appendingPathComponent("Sorted/moved.jpg")
        let result = await Restructure.apply(
            proposals: [RestructureProposal(
                fileID: 1, oldPath: src.path, newPath: dest.path, bucket: "photo")],
            database: db, libraryRoot: root)

        #expect(result.moved == 1)
        #expect(result.failed == 0, "a failed DB update must not also count failed")
        #expect(FileManager.default.fileExists(atPath: dest.path), "the move happened on disk")
        #expect(!FileManager.default.fileExists(atPath: src.path))
    }

    @Test("uniqueDestination disambiguates on-disk and claimed collisions")
    func uniqueDestination() throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUniq-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        let fm = FileManager.default
        let dest = tmp.appendingPathComponent("audio.mp3")

        // Free → returned as-is.
        #expect(Restructure.uniqueDestination(dest, claimed: [], fm: fm) == dest)
        // Claimed this batch → " (2)".
        let d2 = Restructure.uniqueDestination(dest, claimed: [dest.path], fm: fm)
        #expect(d2 == tmp.appendingPathComponent("audio (2).mp3"))
        // On disk → also bumped.
        try Data("x".utf8).write(to: dest)
        let d3 = Restructure.uniqueDestination(dest, claimed: [], fm: fm)
        #expect(d3 == tmp.appendingPathComponent("audio (2).mp3"))
    }
}
