// C1 gate trio (F-C3-001) + timeout-retry (F-C3-036) regression.
//
// Port of the Windows tags_evaluated / faces_evaluated / ocr_stage_ran gates
// (dbwriter.rs): a swallowed Vision/ANE/OCR timeout on a CHANGED file emits an
// empty result with all three stage-ran flags FALSE. The DBWriter must then
// leave the file's previously-persisted auto-tags, OCR text + FTS postings, and
// — critically — manual person_id assignments untouched (it used to run three
// unconditional DELETEs, wiping all of them). And a Vision-timeout file is
// marked failed so the incremental unchanged-skip re-tags it next scan instead
// of stranding it forever.
import Testing
import Foundation
import GRDB
import AsyncAlgorithms
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database

@Suite("DBWriter stage-ran gates (C1 trio) + timeout retry")
struct DBWriterStageGateTests {

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

    /// A successful first scan: tags + a face (later named) + OCR text. All
    /// three stage-ran gates true so the rows actually land.
    private func successScan(url: URL) -> TaggedFile {
        var f = TaggedFile(
            url: url, kind: "image", extension: "jpg", sizeBytes: 4,
            createdAt: Date(timeIntervalSince1970: 1_600_000_000),
            modifiedAt: Self.mtime,
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

    @Test("A swallowed Vision/OCR timeout on a changed file does NOT wipe tags, faces(person_id), or OCR")
    func stageGatesPreserveOnTimeout() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDStageGateTest-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        let fileURL = tmp.appendingPathComponent("IMG_GATE.jpg")

        func newWriter() -> DBWriter {
            DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(),
                     sessionID: UUID().uuidString)
        }

        // First scan succeeds with tags + face + OCR.
        await drain(newWriter(), successScan(url: fileURL))

        let (fileID, faceID): (Int64, Int64) = try await db.pool.read { db in
            let id = try Int64.fetchOne(db, sql:
                "SELECT id FROM files WHERE path_text = ?", arguments: [fileURL.path])
            let fid = try Int64.fetchOne(db, sql:
                "SELECT id FROM face_prints WHERE file_id = ?", arguments: [id])
            return (try #require(id), try #require(fid))
        }

        // User names the person — the most important thing to protect.
        try await db.pool.write { db in
            try db.execute(sql: """
                INSERT INTO persons (name, file_count, created_at) VALUES ('Mom', 1, ?)
                """, arguments: [Date().timeIntervalSince1970])
            let personID = db.lastInsertedRowID
            try db.execute(sql: "UPDATE face_prints SET person_id = ? WHERE id = ?",
                           arguments: [personID, faceID])
        }

        // Rescan: the file CHANGED (new mtime, so the unchanged-skip does NOT
        // short-circuit), but the Vision/OCR pass timed out and was swallowed —
        // the worker emits failed=FALSE with empty tags/faces/ocr and all three
        // stage-ran gates FALSE. The pre-gate code ran three unconditional
        // DELETEs here and lost everything.
        let timedOut = TaggedFile(
            url: fileURL, kind: "image", extension: "jpg", sizeBytes: 4,
            createdAt: Date(timeIntervalSince1970: 1_600_000_000),
            modifiedAt: Self.mtime.addingTimeInterval(60),
            failed: false
            // tagsEvaluated / facesEvaluated / ocrStageRan default false
        )
        await drain(newWriter(), timedOut)

        try await db.pool.read { db in
            let autoTags = try String.fetchAll(db, sql:
                "SELECT tag FROM tags WHERE file_id = ? AND source = 'auto'", arguments: [fileID])
            #expect(autoTags == ["beach"],
                    "auto-tags survive a stage-not-evaluated rescan (gate skips the DELETE)")

            let faces = try Row.fetchAll(db, sql:
                "SELECT id, person_id FROM face_prints WHERE file_id = ?", arguments: [fileID])
            #expect(faces.count == 1, "face row survives a swallowed timeout rescan")
            #expect(faces.first?["id"] == faceID, "face row identity preserved")
            #expect((faces.first?["person_id"] as Int64?) != nil,
                    "manual person assignment must survive a swallowed timeout rescan")

            let ocr = try String.fetchOne(db, sql:
                "SELECT text FROM ocr_text WHERE file_id = ?", arguments: [fileID])
            #expect(ocr == "alpha bravo", "OCR text survives a stage-not-evaluated rescan")
            let ftsHit = try Int64.fetchOne(db, sql:
                "SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH 'alpha'")
            #expect(ftsHit == fileID, "OCR FTS postings intact after a swallowed timeout rescan")
        }
    }

    @Test("A Vision-timeout file is marked failed, so an identical-mtime rescan still re-tags it")
    func timeoutFileIsRetriedNotStranded() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDStageGateTest-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        let fileURL = tmp.appendingPathComponent("IMG_RETRY.jpg")

        func newWriter() -> DBWriter {
            DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(),
                     sessionID: UUID().uuidString)
        }

        // First scan: the primary Vision pass timed out. The tagging stage marks
        // the file failed (gates all false) rather than persisting it
        // failed=false-and-empty. Same size + mtime it will have on rescan, so
        // the ONLY reason the next scan reprocesses is the persisted failed flag.
        let timedOut = TaggedFile(
            url: fileURL, kind: "image", extension: "jpg", sizeBytes: 4,
            createdAt: Date(timeIntervalSince1970: 1_600_000_000),
            modifiedAt: Self.mtime,
            failed: true,
            errorMessage: "Vision pass timed out (will retry next scan)"
        )
        await drain(newWriter(), timedOut)

        let firstID: Int64 = try await db.pool.read { db in
            let row = try #require(try Row.fetchOne(db, sql: """
                SELECT id, failed FROM files WHERE path_text = ?
                """, arguments: [fileURL.path]))
            #expect((row["failed"] as Int64) == 1, "a timed-out file is persisted failed=1")
            let n = try Int.fetchOne(db, sql:
                "SELECT COUNT(*) FROM tags WHERE file_id = ?", arguments: [row["id"] as Int64]) ?? -1
            #expect(n == 0, "no tags written for the timed-out file")
            return row["id"]
        }

        // Rescan: same size + mtime (would be skipped as "unchanged" if the file
        // had been persisted complete). Vision recovers this time.
        var recovered = TaggedFile(
            url: fileURL, kind: "image", extension: "jpg", sizeBytes: 4,
            createdAt: Date(timeIntervalSince1970: 1_600_000_000),
            modifiedAt: Self.mtime,
            visionTags: ["sunset"],
            failed: false,
            tagsEvaluated: true
        )
        recovered.facesEvaluated = true
        await drain(newWriter(), recovered)

        try await db.pool.read { db in
            let row = try #require(try Row.fetchOne(db, sql: """
                SELECT id, failed FROM files WHERE path_text = ?
                """, arguments: [fileURL.path]))
            #expect((row["id"] as Int64) == firstID, "row identity preserved across the retry")
            #expect((row["failed"] as Int64) == 0, "failure cleared after a successful retry")
            let autoTags = try String.fetchAll(db, sql:
                "SELECT tag FROM tags WHERE file_id = ? AND source = 'auto'", arguments: [firstID])
            #expect(autoTags == ["sunset"],
                    "the previously-timed-out file is re-tagged on rescan, not stranded")
        }
    }
}
