// H2 regression: re-scan must preserve files.id and face_prints.person_id
// for unchanged files (id-preserving UPSERT, never delete+reinsert), and the
// v15 FTS sync triggers must keep ocr_fts postings consistent across change
// and delete — the old INSERT OR REPLACE wiped manual person assignments and
// stranded stale FTS postings on every re-scan.
import Testing
import Foundation
import GRDB
import AsyncAlgorithms
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database

@Suite("DBWriter id-preserving upsert (H2)")
struct DBWriterUpsertTests {

    private static let fixedMtime = Date(timeIntervalSince1970: 1_700_000_000)

    private func makeFile(
        url: URL,
        mtime: Date = fixedMtime,
        tags: [String] = ["beach"],
        ocr: String? = "alpha bravo",
        withFace: Bool = true
    ) -> TaggedFile {
        // Represents a fully-evaluated successful image scan: all three stage-ran
        // gates true so the content-change assertions (auto-tag / face / OCR
        // delete-then-reinsert) exercise the firing path.
        var f = TaggedFile(
            url: url, kind: "image", extension: "jpg", sizeBytes: 4,
            createdAt: Date(timeIntervalSince1970: 1_600_000_000),
            modifiedAt: mtime,
            visionTags: tags,
            ocrText: ocr,
            tagsEvaluated: true,
            facesEvaluated: true,
            ocrStageRan: true
        )
        if withFace {
            f.hasFaces = true
            f.facePrints = [Data([1, 2, 3])]
            f.faceBBoxes = ["0.1,0.1,0.5,0.5"]
            f.faceQualities = [0.9]
            f.faceYaws = [nil]
            f.facePitches = [nil]
        }
        return f
    }

    private func drain(_ writer: DBWriter, _ file: TaggedFile) async {
        let channel = AsyncChannel<TaggedFile>()
        let producer = Task {
            await channel.send(file)
            channel.finish()
        }
        await writer.drain(channel)
        await producer.value
    }

    @Test("Unchanged re-scan preserves files.id and face_prints.person_id; change refreshes children")
    func upsertPreservesIdentity() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUpsertTest-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        let fileURL = tmp.appendingPathComponent("IMG_0001.jpg")

        func newWriter() -> DBWriter {
            DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(),
                     sessionID: UUID().uuidString)
        }

        // First scan.
        await drain(newWriter(), makeFile(url: fileURL))

        let (id1, faceID): (Int64, Int64) = try await db.pool.read { db in
            let id = try Int64.fetchOne(db, sql:
                "SELECT id FROM files WHERE path_text = ?", arguments: [fileURL.path])
            let fid = try Int64.fetchOne(db, sql:
                "SELECT id FROM face_prints WHERE file_id = ?", arguments: [id])
            return (try #require(id), try #require(fid))
        }

        // User names the person and adds a manual tag.
        try await db.pool.write { db in
            try db.execute(sql: """
                INSERT INTO persons (name, file_count, created_at) VALUES ('Mom', 1, ?)
                """, arguments: [Date().timeIntervalSince1970])
            let personID = db.lastInsertedRowID
            try db.execute(sql: "UPDATE face_prints SET person_id = ? WHERE id = ?",
                           arguments: [personID, faceID])
            try db.execute(sql: "INSERT INTO tags (file_id, tag, source) VALUES (?, 'keepme', 'user')",
                           arguments: [id1])
        }

        // Re-scan, byte-identical file (same size + mtime).
        await drain(newWriter(), makeFile(url: fileURL))

        try await db.pool.read { db in
            let id2 = try Int64.fetchOne(db, sql:
                "SELECT id FROM files WHERE path_text = ?", arguments: [fileURL.path])
            #expect(id2 == id1, "files.id must survive an unchanged re-scan")

            let faces = try Row.fetchAll(db, sql:
                "SELECT id, person_id FROM face_prints WHERE file_id = ?", arguments: [id1])
            #expect(faces.count == 1, "no duplicate face rows on re-scan")
            #expect(faces.first?["id"] == faceID, "face row identity preserved")
            #expect((faces.first?["person_id"] as Int64?) != nil,
                    "manual person assignment must survive an unchanged re-scan")

            let ftsHit = try Int64.fetchOne(db, sql:
                "SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH 'alpha'")
            #expect(ftsHit == id1, "FTS postings intact after unchanged re-scan")
        }

        // Content change: new mtime, new OCR text, no faces, new auto tags.
        await drain(newWriter(), makeFile(
            url: fileURL,
            mtime: Self.fixedMtime.addingTimeInterval(60),
            tags: ["sunset"],
            ocr: "charlie delta",
            withFace: false
        ))

        try await db.pool.read { db in
            let id3 = try Int64.fetchOne(db, sql:
                "SELECT id FROM files WHERE path_text = ?", arguments: [fileURL.path])
            #expect(id3 == id1, "files.id must survive a changed re-scan (UPSERT, not replace)")

            let faceCount = try Int.fetchOne(db, sql:
                "SELECT COUNT(*) FROM face_prints WHERE file_id = ?", arguments: [id1]) ?? -1
            #expect(faceCount == 0, "stale face rows cleared when the file changed")

            let autoTags = try String.fetchAll(db, sql:
                "SELECT tag FROM tags WHERE file_id = ? AND source = 'auto'", arguments: [id1])
            #expect(autoTags == ["sunset"], "auto tags replaced atomically")
            let userTags = try String.fetchAll(db, sql:
                "SELECT tag FROM tags WHERE file_id = ? AND source = 'user'", arguments: [id1])
            #expect(userTags == ["keepme"], "user tags untouched by re-scan")

            let stale = try Int64.fetchOne(db, sql:
                "SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH 'alpha'")
            #expect(stale == nil, "old OCR postings removed by the v15 sync triggers")
            let fresh = try Int64.fetchOne(db, sql:
                "SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH 'charlie'")
            #expect(fresh == id1, "new OCR text indexed by the v15 sync triggers")
        }
    }

    @Test("insertOne stores the NFC form in path_search (C15)")
    func pathSearchStoresNFC() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUpsertTest-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))

        let nfdName = "cafe\u{0301}.jpg"
        let fileURL = tmp.appendingPathComponent(nfdName)
        let writer = DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(),
                              sessionID: UUID().uuidString)
        await drain(writer, makeFile(url: fileURL, withFace: false))

        try await db.pool.read { db in
            let stored = try String.fetchOne(db, sql:
                "SELECT path_search FROM files WHERE path_text = ?",
                arguments: [fileURL.path])
            let nfc = fileURL.path.precomposedStringWithCanonicalMapping
            #expect(stored == nfc, "path_search must hold the NFC form")
            #expect(stored?.contains("caf\u{00E9}") == true,
                    "NFD input must precompose to U+00E9 in path_search")
        }
    }

    @Test("Failed re-scan records the failure but preserves prior children")
    func failedRescanPreservesChildren() async throws {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDUpsertTest-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        let fileURL = tmp.appendingPathComponent("IMG_0002.jpg")

        func newWriter() -> DBWriter {
            DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(),
                     sessionID: UUID().uuidString)
        }

        // First scan succeeds, with a face.
        await drain(newWriter(), makeFile(url: fileURL))

        let (id1, faceID): (Int64, Int64) = try await db.pool.read { db in
            let id = try Int64.fetchOne(db, sql:
                "SELECT id FROM files WHERE path_text = ?", arguments: [fileURL.path])
            let fid = try Int64.fetchOne(db, sql:
                "SELECT id FROM face_prints WHERE file_id = ?", arguments: [id])
            return (try #require(id), try #require(fid))
        }

        // User names the person.
        try await db.pool.write { db in
            try db.execute(sql: """
                INSERT INTO persons (name, file_count, created_at) VALUES ('Dad', 1, ?)
                """, arguments: [Date().timeIntervalSince1970])
            let personID = db.lastInsertedRowID
            try db.execute(sql: "UPDATE face_prints SET person_id = ? WHERE id = ?",
                           arguments: [personID, faceID])
        }

        // Re-scan of the same path fails transiently (NAS hiccup): the worker
        // emits a failed record with empty metadata.
        let failedFile = TaggedFile(
            url: fileURL, kind: "image", extension: "jpg", sizeBytes: 0,
            createdAt: nil, modifiedAt: nil,
            failed: true, errorMessage: "decode failed"
        )
        await drain(newWriter(), failedFile)

        try await db.pool.read { db in
            let row = try #require(try Row.fetchOne(db, sql: """
                SELECT id, failed, error_message, size_bytes FROM files WHERE path_text = ?
                """, arguments: [fileURL.path]))
            #expect((row["id"] as Int64) == id1, "files.id must survive a failed re-scan")
            #expect((row["failed"] as Int64) == 1, "failure recorded")
            #expect((row["error_message"] as String?) == "decode failed")
            #expect((row["size_bytes"] as Int64) == 4,
                    "prior metadata not clobbered by the failed scan's empty data")

            let faces = try Row.fetchAll(db, sql:
                "SELECT id, person_id FROM face_prints WHERE file_id = ?", arguments: [id1])
            #expect(faces.count == 1, "face row survives a failed re-scan")
            #expect(faces.first?["id"] == faceID, "face row identity preserved")
            #expect((faces.first?["person_id"] as Int64?) != nil,
                    "manual person assignment must survive a failed re-scan")
        }
    }
}
