// F-2 / F-C7-001: macOS rename/move heal regression.
//
// Port of the Windows B1 heal tests (dbwriter.rs heal_candidate_moved). When a
// file is renamed/moved on the same volume, a rescan finds it at a NEW path; the
// DBWriter must RE-BIND the existing row (same id + all FK-linked tags / faces
// incl. manual person_id / OCR / embeddings) to the new path instead of
// orphaning it and inserting a fresh row that loses everything. The exact
// old-path-gone gate keeps two COEXISTING hardlinks (same inode, both paths
// present) as two distinct rows — only a genuine move (old path gone) heals.
import Testing
import Foundation
import GRDB
import AsyncAlgorithms
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database

@Suite("DBWriter rename/move heal (F-2)")
struct DBWriterRenameHealTests {

    private static let mtime = Date(timeIntervalSince1970: 1_700_000_000)

    private func drain(_ writer: DBWriter, _ file: TaggedFile) async {
        let channel = AsyncChannel<TaggedFile>()
        let producer = Task {
            await channel.send(file)
            channel.finish()
        }
        await writer.drain(channel)
        await producer.value
    }

    /// A fully-evaluated successful image scan with one face (later named) and
    /// OCR text. `fileRef` is the real on-disk inode so the heal lookup + gate
    /// exercise the production path. Size/mtime fixed so a pure move reads as
    /// "unchanged" and the carried-over children are preserved.
    private func scan(url: URL, fileRef: UInt64, size: Int64) -> TaggedFile {
        var f = TaggedFile(
            url: url, kind: "image", extension: "jpg", sizeBytes: size,
            createdAt: Date(timeIntervalSince1970: 1_600_000_000),
            modifiedAt: Self.mtime,
            fileRef: fileRef,
            visionTags: ["beach"],
            ocrText: "alpha bravo",
            tagsEvaluated: true, facesEvaluated: true, ocrStageRan: true
        )
        f.hasFaces = true
        f.facePrints = [Data([1, 2, 3])]
        f.faceBBoxes = ["0.1,0.1,0.5,0.5"]
        f.faceQualities = [0.9]
        f.faceYaws = [nil]
        f.facePitches = [nil]
        return f
    }

    @Test("A moved file (old path gone) re-binds the existing row, preserving id + tags + faces(person_id)")
    func movedFileHealsRow() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDHealTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))

        func newWriter() -> DBWriter {
            DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(),
                     sessionID: UUID().uuidString)
        }

        // Real file on disk so we have a genuine inode and the old-path-gone gate
        // (lstat) observes the real move.
        let oldURL = tmp.appendingPathComponent("IMG_OLD.jpg")
        let payload = Data(repeating: 0xAB, count: 1024)
        try payload.write(to: oldURL)
        let inode = try #require(Discovery.inode(of: oldURL), "inode must be readable")
        let size = Int64(payload.count)

        // First scan at the old path.
        await drain(newWriter(), scan(url: oldURL, fileRef: inode, size: size))

        let (id1, faceID): (Int64, Int64) = try await db.pool.read { db in
            let id = try Int64.fetchOne(db, sql:
                "SELECT id FROM files WHERE path_text = ?", arguments: [oldURL.path])
            let fid = try Int64.fetchOne(db, sql:
                "SELECT id FROM face_prints WHERE file_id = ?", arguments: [id])
            return (try #require(id), try #require(fid))
        }

        // User names the person and adds a manual tag.
        try await db.pool.write { db in
            try db.execute(sql:
                "INSERT INTO persons (name, file_count, created_at) VALUES ('Mom', 1, ?)",
                arguments: [Date().timeIntervalSince1970])
            let personID = db.lastInsertedRowID
            try db.execute(sql: "UPDATE face_prints SET person_id = ? WHERE id = ?",
                           arguments: [personID, faceID])
            try db.execute(sql: "INSERT INTO tags (file_id, tag, source) VALUES (?, 'keepme', 'user')",
                           arguments: [id1])
        }

        // Move the file on disk (same volume → rename(2) preserves the inode).
        let newURL = tmp.appendingPathComponent("sub/IMG_RENAMED.jpg")
        try FileManager.default.createDirectory(
            at: newURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.moveItem(at: oldURL, to: newURL)
        #expect(Discovery.inode(of: newURL) == inode, "move must preserve the inode")
        #expect(!FileManager.default.fileExists(atPath: oldURL.path), "old path must be gone")

        // Rescan: the file is discovered at its NEW path with the same inode.
        await drain(newWriter(), scan(url: newURL, fileRef: inode, size: size))

        try await db.pool.read { db in
            let rowCount = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files") ?? -1
            #expect(rowCount == 1, "the moved file must heal in place, not add a second row")

            let oldExists = try Int64.fetchOne(db, sql:
                "SELECT id FROM files WHERE path_text = ?", arguments: [oldURL.path])
            #expect(oldExists == nil, "no orphaned row left at the old path")

            let newRow = try #require(try Row.fetchOne(db, sql:
                "SELECT id, file_ref FROM files WHERE path_text = ?", arguments: [newURL.path]))
            #expect((newRow["id"] as Int64) == id1, "files.id preserved across the move")
            #expect((newRow["file_ref"] as Int64?) == Int64(bitPattern: inode),
                    "file_ref persisted bit-for-bit (Windows-parity)")

            let faces = try Row.fetchAll(db, sql:
                "SELECT id, person_id FROM face_prints WHERE file_id = ?", arguments: [id1])
            #expect(faces.count == 1, "no duplicate face rows after the move")
            #expect(faces.first?["id"] == faceID, "face row identity preserved")
            #expect((faces.first?["person_id"] as Int64?) != nil,
                    "manual person assignment survives the move")

            let userTags = try String.fetchAll(db, sql:
                "SELECT tag FROM tags WHERE file_id = ? AND source = 'user'", arguments: [id1])
            #expect(userTags == ["keepme"], "user tags survive the move")

            let ftsHit = try Int64.fetchOne(db, sql:
                "SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH 'alpha'")
            #expect(ftsHit == id1, "OCR/FTS postings follow the healed row")
        }
    }

    @Test("Two coexisting hardlinks (same inode, both paths present) stay TWO rows")
    func coexistingHardlinksStayDistinct() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDHealTest-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))

        func newWriter() -> DBWriter {
            DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(),
                     sessionID: UUID().uuidString)
        }

        // A file and a HARD LINK to it: both paths exist and share one inode —
        // the macOS analog of two coexisting byte-identical files. The
        // old-path-gone gate must NOT collapse them.
        let aURL = tmp.appendingPathComponent("LINK_A.jpg")
        let bURL = tmp.appendingPathComponent("LINK_B.jpg")
        let payload = Data(repeating: 0xCD, count: 2048)
        try payload.write(to: aURL)
        try FileManager.default.linkItem(at: aURL, to: bURL)
        let size = Int64(payload.count)
        let inodeA = try #require(Discovery.inode(of: aURL))
        let inodeB = try #require(Discovery.inode(of: bURL))
        #expect(inodeA == inodeB, "a hard link shares the inode of its target")

        // Scan both paths — both on disk throughout.
        await drain(newWriter(), scan(url: aURL, fileRef: inodeA, size: size))
        await drain(newWriter(), scan(url: bURL, fileRef: inodeB, size: size))

        try await db.pool.read { db in
            let rowCount = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM files") ?? -1
            #expect(rowCount == 2, "coexisting hardlinks must remain two distinct rows")

            let idA = try Int64.fetchOne(db, sql:
                "SELECT id FROM files WHERE path_text = ?", arguments: [aURL.path])
            let idB = try Int64.fetchOne(db, sql:
                "SELECT id FROM files WHERE path_text = ?", arguments: [bURL.path])
            #expect(idA != nil && idB != nil, "both paths keep their own row")
            #expect(idA != idB, "the two rows are distinct (no heal-collapse)")
        }
    }
}
