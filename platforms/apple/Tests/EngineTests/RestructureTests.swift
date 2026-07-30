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

    @Test("A dated video routes to Videos/<Year>, dated audio to Audio/<Year>")
    func videoAudioBuckets() {
        let moves = Restructure.ruleClassify(
            [f(1, "video"), f(2, "audio")], libraryRoot: root)
        let vid = moves.first { $0.fileID == 1 }!
        #expect(vid.newPath.contains("/Videos/2024/"))
        #expect(!vid.newPath.contains("March"), "videos have no month: \(vid.newPath)")
        #expect(vid.bucket == "video")
        let aud = moves.first { $0.fileID == 2 }!
        #expect(aud.newPath.contains("/Audio/2024/"), "audio should have year sub-folder: \(aud.newPath)")
        #expect(aud.bucket == "audio")
    }

    @Test("A dated image routes to Photos/<Year>/<MonthName>")
    func imageYearMonth() {
        let moves = Restructure.ruleClassify([f(1, "image")], libraryRoot: root)
        #expect(moves[0].newPath.contains("/Photos/2024/March"))
        #expect(moves[0].bucket == "photo")
    }

    @Test("A file with no timestamp gets flat folder + Ask confidence")
    func missingTimestampYear() {
        #expect(Restructure.yearMonth(0).year == 1970)
        #expect(Restructure.yearMonth(0).month == 1)
        // modifiedUnix 0, no createdUnix → ts invalid → flat Photos/, Ask confidence.
        let moves = Restructure.ruleClassify(
            [f(1, "image", modified: 0)], libraryRoot: root)
        #expect(!moves[0].newPath.contains("1970"), "zero-timestamp must not land in 1970: \(moves[0].newPath)")
        #expect(moves[0].confidence == "ask", "zero-timestamp must surface for user decision")
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
    /// refreshed to StablePathHash(newPath). The second move's planned basename
    /// collided and was uniquified, so it is reported in `conflicts` (audit
    /// F-A4 — the array was previously dead/always-empty).
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
        let result = try await Restructure.apply(
            proposals: [
                RestructureProposal(fileID: 1, oldPath: srcA.path, newPath: dest.path, bucket: "photo"),
                RestructureProposal(fileID: 2, oldPath: srcB.path, newPath: dest.path, bucket: "photo"),
            ],
            database: db, libraryRoot: root)

        #expect(result.moved == 2)
        #expect(result.failed == 0)
        // Exactly one collision: the second proposal's planned dest already held
        // the first move, so it was renamed to ` (2)` and reported here.
        #expect(result.conflicts == [dest.path])
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
        let result = try await Restructure.apply(
            proposals: [RestructureProposal(
                fileID: 1, oldPath: staleSrc.path, newPath: dest.path, bucket: "photo")],
            database: db, libraryRoot: root)

        #expect(result.moved == 0)
        #expect(result.failed == 1)
        #expect(FileManager.default.fileExists(atPath: real.path), "the real file is untouched")
        #expect(!FileManager.default.fileExists(atPath: dest.path))
    }

    /// R-#14: a real same-path SWAP — the DB recorded one file_ref (inode) for the
    /// planned file, but a DIFFERENT file now occupies that exact path — must be
    /// skipped, not moved. Real inodes (runs on the dev Mac); mirrors the Windows
    /// engine's apply_skips_move_when_file_ref_swapped.
    @Test("A same-path file swap (file_ref mismatch) is failed, not executed")
    func applySwapGuard() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDRestructure-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let doc = root.appendingPathComponent("doc.pdf")
        try Data("SWAPPED-IN".utf8).write(to: doc)
        // The real inode of the file now on disk; if unreadable the guard is inert.
        guard let realInode = Discovery.inode(of: doc) else { return }

        let db = try makeDB(tmp)
        // The DB names the SAME path but a DIFFERENT file_ref — the file we planned to
        // move, since replaced on disk by another. realInode &+ 1 is guaranteed differ.
        try await db.pool.write { d in
            try d.execute(
                sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, file_ref) VALUES (?,?,?,10,0,'doc','pdf',?)",
                arguments: [1, doc.path, StablePathHash.hash(doc.path),
                            Int64(bitPattern: realInode &+ 1)])
        }

        let dest = root.appendingPathComponent("Sorted/doc.pdf")
        let result = try await Restructure.apply(
            proposals: [RestructureProposal(
                fileID: 1, oldPath: doc.path, newPath: dest.path, bucket: "document")],
            database: db, libraryRoot: root)

        #expect(result.moved == 0, "a swapped file must not be moved")
        #expect(result.failed == 1)
        #expect(FileManager.default.fileExists(atPath: doc.path), "the swapped-in file is untouched")
        #expect(!FileManager.default.fileExists(atPath: dest.path))
    }

    /// R-#14 pure: the swap detector fires ONLY on a both-known mismatch — any missing
    /// input leaves the move to proceed (no false skips). Pins the Int64↔UInt64
    /// bit-cast round-trip; mirrors the Rust file_ref_swapped_only_on_positive_mismatch.
    @Test("fileRefSwapped is true only on a both-known mismatch")
    func fileRefSwappedPredicate() {
        #expect(Restructure.fileRefSwapped(dbRef: 100, currentRef: 200))
        #expect(!Restructure.fileRefSwapped(dbRef: 100, currentRef: 100))
        #expect(!Restructure.fileRefSwapped(dbRef: -1, currentRef: UInt64.max),
                "-1 as Int64 bit-casts to UInt64.max — the high-bit round-trip matches")
        #expect(!Restructure.fileRefSwapped(dbRef: nil, currentRef: 200))
        #expect(!Restructure.fileRefSwapped(dbRef: 100, currentRef: nil))
        #expect(!Restructure.fileRefSwapped(dbRef: nil, currentRef: nil))
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
        let result = try await Restructure.apply(
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
        // Claimed this batch → " (2)". The claimed set is stored case-folded
        // (APFS/NTFS are case-insensitive), so claims are registered lowercased.
        let d2 = Restructure.uniqueDestination(
            dest, claimed: [Restructure.DestinationClaim(dest.path)], fm: fm)
        #expect(d2 == tmp.appendingPathComponent("audio (2).mp3"))
        // Case-ONLY collision is detected (the data-loss fix): a destination that
        // differs from a claimed path only in case maps to the same on-disk slot and
        // must be uniquified, never silently overwritten.
        let dUpper = tmp.appendingPathComponent("AUDIO.mp3")
        let d2ci = Restructure.uniqueDestination(
            dUpper, claimed: [Restructure.DestinationClaim(dest.path)], fm: fm)
        #expect(d2ci == tmp.appendingPathComponent("AUDIO (2).mp3"))
        // On disk → also bumped.
        try Data("x".utf8).write(to: dest)
        let d3 = Restructure.uniqueDestination(dest, claimed: [], fm: fm)
        #expect(d3 == tmp.appendingPathComponent("audio (2).mp3"))
    }

    @Test("An unavailable undo journal prevents every move")
    func unavailableUndoJournalFailsClosed() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUndoUnavailable-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let src = root.appendingPathComponent("source.jpg")
        let dest = root.appendingPathComponent("Sorted/source.jpg")
        try Data("source".utf8).write(to: src)
        let db = try makeDB(tmp)
        try await insertRow(db, id: 1, path: src.path)
        let blocker = tmp.appendingPathComponent("not-a-directory")
        try Data("x".utf8).write(to: blocker)
        let journal = blocker.appendingPathComponent("undo.ndjson")

        do {
            _ = try await Restructure.apply(
                proposals: [RestructureProposal(
                    fileID: 1, oldPath: src.path, newPath: dest.path, bucket: "photo")],
                database: db, libraryRoot: root, undoJournal: journal)
            Issue.record("apply unexpectedly succeeded without an undo journal")
        } catch Restructure.UndoJournalError.unavailable(_) {
        } catch {
            Issue.record("unexpected error: \(error)")
        }
        #expect(FileManager.default.fileExists(atPath: src.path))
        #expect(!FileManager.default.fileExists(atPath: dest.path))
    }

    @Test("An undo journal write failure stops before the next move")
    func undoJournalWriteFailureStopsApply() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUndoWriteFailure-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        let incoming = root.appendingPathComponent("incoming")
        try FileManager.default.createDirectory(at: incoming, withIntermediateDirectories: true)
        let src1 = incoming.appendingPathComponent("one.jpg")
        let src2 = incoming.appendingPathComponent("two.jpg")
        let dest1 = root.appendingPathComponent("Photos/one.jpg")
        let dest2 = root.appendingPathComponent("Photos/two.jpg")
        try Data("one".utf8).write(to: src1)
        try Data("two".utf8).write(to: src2)
        let db = try makeDB(tmp)
        try await insertRow(db, id: 1, path: src1.path)
        try await insertRow(db, id: 2, path: src2.path)
        let journal = tmp.appendingPathComponent("undo.ndjson")
        var appendCount = 0

        do {
            _ = try await Restructure.applyForTesting(
                proposals: [
                    RestructureProposal(fileID: 1, oldPath: src1.path,
                                        newPath: dest1.path, bucket: "photo"),
                    RestructureProposal(fileID: 2, oldPath: src2.path,
                                        newPath: dest2.path, bucket: "photo")
                ],
                database: db, libraryRoot: root, undoJournal: journal,
                journalAppender: { entry, handle in
                    appendCount += 1
                    if appendCount == 2 {
                        try handle.write(contentsOf: Data("{".utf8))
                        throw CocoaError(.fileWriteOutOfSpace)
                    }
                    try Restructure.appendUndoEntry(entry, to: handle)
                })
            Issue.record("apply unexpectedly continued after a journal write failure")
        } catch Restructure.UndoJournalError.writeFailed(let result) {
            #expect(result.moved == 1)
            #expect(result.failed == 1)
        } catch {
            Issue.record("unexpected error: \(error)")
        }

        #expect(FileManager.default.fileExists(atPath: dest1.path))
        #expect(!FileManager.default.fileExists(atPath: src1.path))
        #expect(FileManager.default.fileExists(atPath: src2.path))
        #expect(!FileManager.default.fileExists(atPath: dest2.path))
        #expect(Restructure.hasUndoableRun(undoJournal: journal))

        let undone = await Restructure.undoLast(
            database: db, libraryRoot: root, undoJournal: journal)
        #expect(undone.moved == 1)
        #expect(undone.failed == 0)
        #expect(FileManager.default.fileExists(atPath: src1.path))
        #expect(!Restructure.hasUndoableRun(undoJournal: journal))
    }

    /// R2 reversibility: apply relocates a file, undoLast moves it back to its
    /// original path + updates the DB + clears the journal (so it can't be undone
    /// twice). Uses a temp journal so the real one is never touched.
    @Test("Undo last run restores files to their original locations")
    func undoLastRoundTrip() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUndo-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        let downloads = root.appendingPathComponent("downloads")
        try FileManager.default.createDirectory(at: downloads, withIntermediateDirectories: true)
        let src = downloads.appendingPathComponent("invoice.pdf")
        try Data("PDF".utf8).write(to: src)

        let db = try makeDB(tmp)
        try await insertRow(db, id: 1, path: src.path)

        let journal = tmp.appendingPathComponent("undo.ndjson")
        let dest = root.appendingPathComponent("Documents").appendingPathComponent("invoice.pdf")

        let applied = try await Restructure.apply(
            proposals: [RestructureProposal(
                fileID: 1, oldPath: src.path, newPath: dest.path, bucket: "document")],
            database: db, libraryRoot: root, undoJournal: journal)
        #expect(applied.moved == 1)
        #expect(FileManager.default.fileExists(atPath: dest.path))
        #expect(!FileManager.default.fileExists(atPath: src.path))
        #expect(Restructure.hasUndoableRun(undoJournal: journal))

        let undone = await Restructure.undoLast(
            database: db, libraryRoot: root, undoJournal: journal)
        #expect(undone.moved == 1)
        #expect(undone.failed == 0)
        #expect(FileManager.default.fileExists(atPath: src.path), "restored to original path")
        #expect(!FileManager.default.fileExists(atPath: dest.path), "new path vacated")

        let livePath: String? = try await db.pool.read { d in
            try String.fetchOne(d, sql: "SELECT path_text FROM files WHERE id = 1")
        }
        #expect(livePath == src.path, "DB points back at the original path")

        // Journal cleared → a second undo is a no-op (no accidental redo).
        #expect(!Restructure.hasUndoableRun(undoJournal: journal))
        let again = await Restructure.undoLast(
            database: db, libraryRoot: root, undoJournal: journal)
        #expect(again.moved == 0)
    }

    @Test("Undo recovers a crash after move but before DB update")
    func undoRecoversPostMoveCrash() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUndoCrash-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let src = root.appendingPathComponent("source.jpg")
        let dest = root.appendingPathComponent("Photos/source.jpg")
        try Data("source".utf8).write(to: src)
        let inode = try #require(Discovery.inode(of: src))
        let db = try makeDB(tmp)
        try await db.pool.write { d in
            try d.execute(
                sql: "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, file_ref) VALUES (?,?,?,?,0,'image','jpg',?)",
                arguments: [1, src.path, StablePathHash.hash(src.path), 6,
                            Int64(bitPattern: inode)])
        }
        try FileManager.default.createDirectory(
            at: dest.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.moveItem(at: src, to: dest)
        let journal = tmp.appendingPathComponent("undo.ndjson")
        let handle = try Restructure.openUndoJournalTruncating(at: journal)
        try Restructure.appendUndoEntry(
            Restructure.UndoEntry(fileID: 1, from: dest.path, to: src.path), to: handle)
        try handle.close()

        let undone = await Restructure.undoLast(
            database: db, libraryRoot: root, undoJournal: journal)
        #expect(undone.moved == 1)
        #expect(undone.failed == 0)
        #expect(FileManager.default.fileExists(atPath: src.path))
        #expect(!FileManager.default.fileExists(atPath: dest.path))
    }

    @Test("Undo replays dependent moves in reverse order")
    func undoReversesDependentMoves() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUndoOrder-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let a = root.appendingPathComponent("A.txt")
        let b = root.appendingPathComponent("B.txt")
        let x = root.appendingPathComponent("X.txt")
        try Data("first".utf8).write(to: a)
        try Data("second".utf8).write(to: b)
        let db = try makeDB(tmp)
        try await insertRow(db, id: 1, path: a.path)
        try await insertRow(db, id: 2, path: b.path)
        let journal = tmp.appendingPathComponent("undo.ndjson")

        let applied = try await Restructure.apply(
            proposals: [
                RestructureProposal(fileID: 1, oldPath: a.path,
                                    newPath: x.path, bucket: "document"),
                RestructureProposal(fileID: 2, oldPath: b.path,
                                    newPath: a.path, bucket: "document")
            ], database: db, libraryRoot: root, undoJournal: journal)
        #expect(applied.moved == 2)

        let undone = await Restructure.undoLast(
            database: db, libraryRoot: root, undoJournal: journal)
        #expect(undone.moved == 2)
        #expect(undone.failed == 0)
        #expect(String(data: try Data(contentsOf: a), encoding: .utf8) == "first")
        #expect(String(data: try Data(contentsOf: b), encoding: .utf8) == "second")
        #expect(!FileManager.default.fileExists(atPath: x.path))
    }

    /// Audit R2 fix: a CANCELLED undo must NOT clear the journal, so the user can
    /// re-run it and finish — otherwise a mistimed Stop orphans the un-restored
    /// files with no recovery path. Worst case: cancel before any move.
    @Test("A cancelled undo preserves the journal for a re-run")
    func undoCancelPreservesJournal() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUndoCancel-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        let downloads = root.appendingPathComponent("downloads")
        try FileManager.default.createDirectory(at: downloads, withIntermediateDirectories: true)
        let src = downloads.appendingPathComponent("invoice.pdf")
        try Data("PDF".utf8).write(to: src)

        let db = try makeDB(tmp)
        try await insertRow(db, id: 1, path: src.path)
        let journal = tmp.appendingPathComponent("undo.ndjson")
        let dest = root.appendingPathComponent("Documents").appendingPathComponent("invoice.pdf")

        _ = try await Restructure.apply(
            proposals: [RestructureProposal(
                fileID: 1, oldPath: src.path, newPath: dest.path, bucket: "document")],
            database: db, libraryRoot: root, undoJournal: journal)
        #expect(Restructure.hasUndoableRun(undoJournal: journal))

        // Cancel the undo before it moves anything.
        let cancelled = await Restructure.undoLast(
            database: db, libraryRoot: root, isCancelled: { true }, undoJournal: journal)
        #expect(cancelled.moved == 0)
        #expect(Restructure.hasUndoableRun(undoJournal: journal), "journal preserved on cancel")
        #expect(FileManager.default.fileExists(atPath: dest.path), "file still at restructured loc")

        // Re-run undo (not cancelled) → restores and only NOW clears the journal.
        let done = await Restructure.undoLast(
            database: db, libraryRoot: root, undoJournal: journal)
        #expect(done.moved == 1)
        #expect(FileManager.default.fileExists(atPath: src.path), "restored on re-run")
        #expect(!Restructure.hasUndoableRun(undoJournal: journal), "journal cleared after completion")
    }

    @Test("A partially completed undo is idempotent when retried")
    func partialUndoCanResume() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUndoResume-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        let incoming = root.appendingPathComponent("incoming")
        try FileManager.default.createDirectory(at: incoming, withIntermediateDirectories: true)
        let src1 = incoming.appendingPathComponent("one.jpg")
        let src2 = incoming.appendingPathComponent("two.jpg")
        try Data("one".utf8).write(to: src1)
        try Data("two".utf8).write(to: src2)
        let db = try makeDB(tmp)
        try await insertRow(db, id: 1, path: src1.path)
        try await insertRow(db, id: 2, path: src2.path)
        let journal = tmp.appendingPathComponent("undo.ndjson")
        let dest1 = root.appendingPathComponent("Photos/one.jpg")
        let dest2 = root.appendingPathComponent("Photos/two.jpg")

        let applied = try await Restructure.apply(
            proposals: [
                RestructureProposal(fileID: 1, oldPath: src1.path,
                                    newPath: dest1.path, bucket: "photo"),
                RestructureProposal(fileID: 2, oldPath: src2.path,
                                    newPath: dest2.path, bucket: "photo")
            ], database: db, libraryRoot: root, undoJournal: journal)
        #expect(applied.moved == 2)

        // Make the second inverse fail after the first has already restored.
        try FileManager.default.removeItem(at: dest2)
        let partial = await Restructure.undoLast(
            database: db, libraryRoot: root, undoJournal: journal)
        #expect(partial.moved == 1)
        #expect(partial.failed == 1)
        #expect(Restructure.hasUndoableRun(undoJournal: journal))
        #expect(FileManager.default.fileExists(atPath: src1.path))

        // Repair the unavailable source and retry. Entry 1 is already restored;
        // it must be an idempotent skip rather than a permanent stale failure.
        try Data("two".utf8).write(to: dest2)
        let completed = await Restructure.undoLast(
            database: db, libraryRoot: root, undoJournal: journal)
        #expect(completed.failed == 0)
        #expect(completed.skipped == 1)
        #expect(FileManager.default.fileExists(atPath: src2.path))
        #expect(!Restructure.hasUndoableRun(undoJournal: journal))
    }

    @Test("Large-library planning uses an on-disk plan and bounded preview")
    func largePlannerIsDiskBacked() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDLargePlanner-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        let root = tmp.appendingPathComponent("Library")
        let db = try makeDB(tmp)
        let total = Restructure.storedPlanPreviewCap + 37
        try await db.pool.write { d in
            try d.execute(sql: """
                WITH RECURSIVE ids(x) AS (
                    SELECT 1 UNION ALL SELECT x + 1 FROM ids WHERE x < ?
                )
                INSERT INTO files
                  (id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed)
                SELECT x, printf(? || '/downloads/%d.jpg', x), x, 1, 1,
                       'image', 'jpg', 0 FROM ids
                """, arguments: [total, root.path])
        }
        let planDir = tmp.appendingPathComponent("plans")
        let maybePlan = try await Restructure.proposeLargeStoredIfNeeded(
            database: db, libraryRoot: root, directory: planDir,
            threshold: Restructure.storedPlanPreviewCap)
        let plan = try #require(maybePlan)

        #expect(plan.truncated)
        #expect(plan.totalMoves == total)
        #expect(plan.moves.count == Restructure.storedPlanPreviewCap)
        let confidence = try #require(plan.confidenceCounts)
        #expect(confidence.auto == 0)
        #expect(confidence.review == 0)
        #expect(confidence.ask == total)
        #expect(confidence.unknown == 0)
        #expect(
            confidence.auto + confidence.review + confidence.ask + confidence.unknown
                == total)
        let planID = try #require(plan.planID)
        #expect(FileManager.default.fileExists(
            atPath: planDir.appendingPathComponent("\(planID).ndjson").path))
    }

    @Test("Stored restructure plans expose a bounded preview")
    func storedPlanPreviewIsBounded() throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDPlanSpool-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let total = Restructure.storedPlanPreviewCap + 37
        let moves = (0..<total).map { index in
            RestructureMove(
                fileID: Int64(index),
                source: "/library/incoming/\(index).jpg",
                destination: "/library/Photos/2024/\(index).jpg",
                category: "photo", tier: "Mixed",
                confidence: "review", reason: "Photo from 2024")
        }

        let stored = try Restructure.storePlan(
            libraryRoot: "/library", moves: moves, directory: tmp)
        #expect(stored.preview.count == Restructure.storedPlanPreviewCap)
        #expect(UUID(uuidString: stored.planID) != nil)
        let file = tmp.appendingPathComponent("\(stored.planID).ndjson")
        let data = try Data(contentsOf: file)
        #expect(data.split(separator: 0x0A).count == total + 1)
    }

    /// Audit R1 (data-safety fix): a stored/truncated plan's rows past the
    /// preview cap were NEVER shown to the user, so `applyStoredPlan` must
    /// apply only the "auto" tier — exactly like the Rust engine's
    /// `auto_tier_only` gate — and hold Review/Ask/unknown-confidence rows
    /// back rather than moving them sight-unseen.
    @Test("applyStoredPlan applies only the Auto tier; Review/Ask/unknown stay put")
    func applyStoredPlanFiltersToAutoTier() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDStoredPlanTierFilter-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let root = tmp.appendingPathComponent("Library")
        let incoming = root.appendingPathComponent("incoming")
        try FileManager.default.createDirectory(at: incoming, withIntermediateDirectories: true)

        let autoSrc = incoming.appendingPathComponent("auto.jpg")
        let reviewSrc = incoming.appendingPathComponent("review.jpg")
        let askSrc = incoming.appendingPathComponent("ask.jpg")
        let unknownSrc = incoming.appendingPathComponent("unknown.jpg")
        for src in [autoSrc, reviewSrc, askSrc, unknownSrc] {
            try Data(src.lastPathComponent.utf8).write(to: src)
        }

        let db = try makeDB(tmp)
        try await insertRow(db, id: 1, path: autoSrc.path)
        try await insertRow(db, id: 2, path: reviewSrc.path)
        try await insertRow(db, id: 3, path: askSrc.path)
        try await insertRow(db, id: 4, path: unknownSrc.path)

        let autoDest = root.appendingPathComponent("Photos/auto.jpg")
        let reviewDest = root.appendingPathComponent("Photos/review.jpg")
        let askDest = root.appendingPathComponent("Photos/ask.jpg")
        let unknownDest = root.appendingPathComponent("Photos/unknown.jpg")

        let moves = [
            RestructureMove(fileID: 1, source: autoSrc.path, destination: autoDest.path,
                            category: "photo", confidence: "auto"),
            RestructureMove(fileID: 2, source: reviewSrc.path, destination: reviewDest.path,
                            category: "photo", confidence: "review"),
            RestructureMove(fileID: 3, source: askSrc.path, destination: askDest.path,
                            category: "photo", confidence: "ask"),
            RestructureMove(fileID: 4, source: unknownSrc.path, destination: unknownDest.path,
                            category: "photo", confidence: ""),
        ]
        let planDir = tmp.appendingPathComponent("plans")
        let stored = try Restructure.storePlan(
            libraryRoot: root.path, moves: moves, directory: planDir)

        let result = try await Restructure.applyStoredPlan(
            planID: stored.planID, expectedRoot: root.path,
            database: db, libraryRoot: root, directory: planDir)

        #expect(result.moved == 1)
        #expect(result.failed == 0)
        #expect(result.cancelled == false)
        #expect(result.heldReview == 1)
        #expect(result.heldAsk == 1)
        #expect(result.heldUnknown == 1)

        #expect(FileManager.default.fileExists(atPath: autoDest.path), "auto move applied")
        #expect(!FileManager.default.fileExists(atPath: autoSrc.path))

        // Review/Ask/unknown must stay exactly where they were — the whole
        // point of this fix: those rows were never shown to the user.
        #expect(FileManager.default.fileExists(atPath: reviewSrc.path))
        #expect(!FileManager.default.fileExists(atPath: reviewDest.path))
        #expect(FileManager.default.fileExists(atPath: askSrc.path))
        #expect(!FileManager.default.fileExists(atPath: askDest.path))
        #expect(FileManager.default.fileExists(atPath: unknownSrc.path))
        #expect(!FileManager.default.fileExists(atPath: unknownDest.path))

        let pathAuto: String? = try await db.pool.read { d in
            try String.fetchOne(d, sql: "SELECT path_text FROM files WHERE id = 1")
        }
        #expect(pathAuto == autoDest.path)
        let pathReview: String? = try await db.pool.read { d in
            try String.fetchOne(d, sql: "SELECT path_text FROM files WHERE id = 2")
        }
        #expect(pathReview == reviewSrc.path)
        let pathAsk: String? = try await db.pool.read { d in
            try String.fetchOne(d, sql: "SELECT path_text FROM files WHERE id = 3")
        }
        #expect(pathAsk == askSrc.path)
        let pathUnknown: String? = try await db.pool.read { d in
            try String.fetchOne(d, sql: "SELECT path_text FROM files WHERE id = 4")
        }
        #expect(pathUnknown == unknownSrc.path)
    }
}
