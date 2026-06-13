// Re-audit R-14 regression: F-C6-001's discovery incremental skip drops
// unchanged files (size+mtime match) upstream of the pipeline, which made
// DBWriter's unchanged-file CLIP-backfill branch (insertOne, `if unchanged`)
// unreachable on a normal incremental rescan — so a CLIP model installed AFTER
// the original scan never backfilled embeddings. The fix gives discovery a
// shared predicate (`DBWriter.skipSetClipBackfillExclusionSQL`) that keeps an
// embeddable image lacking a clip_embeddings row IN the pipeline. These tests
// assert (1) the predicate excludes exactly that file from a skip-set-shaped
// query while leaving embedded images + non-images skippable, and (2) the
// backfill branch itself fills the embedding for an unchanged file once a blob
// arrives, without duplicating it on a later scan.
import Testing
import Foundation
import GRDB
import AsyncAlgorithms
@testable import FileIDEngine

@Suite("DBWriter CLIP backfill reachability (R-14)")
struct DBWriterClipBackfillTests {

    private static let fixedMtime = Date(timeIntervalSince1970: 1_700_000_000)

    private func makeFile(url: URL, kind: String, clip: Data?) -> TaggedFile {
        TaggedFile(
            url: url, kind: kind, extension: url.pathExtension, sizeBytes: 4,
            createdAt: Date(timeIntervalSince1970: 1_600_000_000),
            modifiedAt: Self.fixedMtime,
            clipEmbeddingBlob: clip,
            tagsEvaluated: true, facesEvaluated: true, ocrStageRan: true
        )
    }

    private func drain(_ db: Database, _ file: TaggedFile) async {
        let writer = DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(),
                              sessionID: UUID().uuidString)
        let channel = AsyncChannel<TaggedFile>()
        let producer = Task {
            await channel.send(file)
            channel.finish()
        }
        await writer.drain(channel)
        await producer.value
    }

    private func newDB() throws -> (Database, URL) {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDClipBackfill-\(UUID().uuidString)")
        let db = try Database(at: tmp.appendingPathComponent("test.sqlite"))
        return (db, tmp)
    }

    @Test("skip-set predicate excludes only embeddable images lacking an embedding")
    func predicateExcludesEmbeddinglessImages() async throws {
        let (db, tmp) = try newDB()
        defer { try? FileManager.default.removeItem(at: tmp) }

        let imgEmbedded   = tmp.appendingPathComponent("a_with_embedding.jpg")
        let imgNoEmbedding = tmp.appendingPathComponent("b_no_embedding.jpg")
        let docNoEmbedding = tmp.appendingPathComponent("c_document.pdf")

        await drain(db, makeFile(url: imgEmbedded, kind: "image", clip: Data([1, 2, 3, 4])))
        await drain(db, makeFile(url: imgNoEmbedding, kind: "image", clip: nil))
        await drain(db, makeFile(url: docNoEmbedding, kind: "doc", clip: nil))

        // Discovery's skip-set query shape, with the shared exclusion ANDed in.
        let skippable = try await db.pool.read { db -> [String] in
            try String.fetchAll(db, sql: """
                SELECT path_text FROM files
                WHERE failed = 0 AND \(DBWriter.skipSetClipBackfillExclusionSQL)
                ORDER BY path_text
                """)
        }

        #expect(skippable.contains(imgEmbedded.path),
                "an image that already has an embedding stays skippable")
        #expect(skippable.contains(docNoEmbedding.path),
                "a non-image without an embedding stays skippable (only images are forced)")
        #expect(!skippable.contains(imgNoEmbedding.path),
                "an image lacking an embedding must be EXCLUDED so it reaches the backfill branch")
    }

    @Test("unchanged-file backfill fills the embedding once and never duplicates it")
    func unchangedBackfillFillsThenIsIdempotent() async throws {
        let (db, tmp) = try newDB()
        defer { try? FileManager.default.removeItem(at: tmp) }
        let url = tmp.appendingPathComponent("IMG_backfill.jpg")

        // First scan: CLIP not installed yet — no embedding produced.
        await drain(db, makeFile(url: url, kind: "image", clip: nil))
        let fileID: Int64 = try await db.pool.read { db in
            try #require(try Int64.fetchOne(db, sql:
                "SELECT id FROM files WHERE path_text = ?", arguments: [url.path]))
        }
        var count = try await db.pool.read { db in
            try Int.fetchOne(db, sql:
                "SELECT COUNT(*) FROM clip_embeddings WHERE file_id = ?", arguments: [fileID]) ?? -1
        }
        #expect(count == 0, "no embedding after a CLIP-less first scan")

        // CLIP installed; rescan an UNCHANGED file (same size+mtime) now yields a
        // blob — the backfill branch must fill it without rebuilding children.
        await drain(db, makeFile(url: url, kind: "image", clip: Data([9, 8, 7, 6])))
        count = try await db.pool.read { db in
            try Int.fetchOne(db, sql:
                "SELECT COUNT(*) FROM clip_embeddings WHERE file_id = ?", arguments: [fileID]) ?? -1
        }
        #expect(count == 1, "unchanged-file backfill inserts the missing embedding")

        // A later unchanged scan with a blob present must not duplicate the row.
        await drain(db, makeFile(url: url, kind: "image", clip: Data([5, 4, 3, 2])))
        count = try await db.pool.read { db in
            try Int.fetchOne(db, sql:
                "SELECT COUNT(*) FROM clip_embeddings WHERE file_id = ?", arguments: [fileID]) ?? -1
        }
        #expect(count == 1, "backfill is idempotent — exactly one embedding row remains")
    }
}
