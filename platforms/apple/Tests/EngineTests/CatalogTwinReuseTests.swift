import AsyncAlgorithms
import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine
private typealias Database = FileIDEngine.Database

@Suite("Identical-image reuse across scans")
struct CatalogTwinReuseTests {
    private typealias Fixtures = ScanImageFixtures

    private struct Library {
        let dir: URL
        let db: Database
        let bytes: Data
        let hash: Data
        let original: URL
        let clip: Data

        func writer(force: Bool = false) -> DBWriter {
            DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(), sessionID: UUID().uuidString,
                     forceReprocess: force)
        }

        func write(_ file: TaggedFile) async {
            let channel = AsyncChannel<TaggedFile>()
            let producer = Task {
                await channel.send(file)
                channel.finish()
            }
            await writer().drain(channel)
            await producer.value
        }

        func fileID(_ url: URL) throws -> Int64? {
            try db.pool.read { try Int64.fetchOne($0, sql: "SELECT id FROM files WHERE path_text = ?", arguments: [url.path]) }
        }

        func tags(_ url: URL) throws -> [String] {
            try db.pool.read {
                try String.fetchAll($0, sql: """
                    SELECT t.tag || ':' || COALESCE(t.score, 'none') FROM tags t JOIN files f ON f.id = t.file_id
                    WHERE f.path_text = ? AND t.source = 'auto' ORDER BY t.rowid
                    """, arguments: [url.path])
            }
        }

        func summary(_ id: Int64) throws -> String? {
            try db.pool.read {
                try String.fetchOne($0, sql: """
                    SELECT f.has_faces || '|' || f.has_text || '|' || f.aesthetic || '|' || f.camera_model || '|' ||
                           o.text || '|' || o.handwritten || '|' || c.model
                    FROM files f JOIN ocr_text o ON o.file_id = f.id JOIN clip_embeddings c ON c.file_id = f.id
                    WHERE f.id = ?
                    """, arguments: [id])
            }
        }

        func embedding(_ id: Int64) throws -> Data? {
            try db.pool.read { try Data.fetchOne($0, sql: "SELECT embedding FROM clip_embeddings WHERE file_id = ?", arguments: [id]) }
        }

        func faces(_ url: URL) throws -> [String] {
            try db.pool.read {
                try String.fetchAll($0, sql: """
                    SELECT p.bbox || '|' || COALESCE(p.face_quality, 'none') || '|' || p.excluded
                    FROM face_prints p JOIN files f ON f.id = p.file_id WHERE f.path_text = ? ORDER BY p.id
                    """, arguments: [url.path])
            }
        }
    }

    private static let provenance = TwinCatalogStore.Provenance(
        clipModel: CLIPEmbeddingSpace.modelID, clipBytes: CLIPEmbeddingSpace.dimension * 4,
        clipLoaded: true, taggerLoaded: true,
        faceProcessing: FaceAnalysisCache.processingVersion, faceModel: "sface-test")

    private static func unitClip() -> Data {
        var values = [Float](repeating: 0, count: CLIPEmbeddingSpace.dimension)
        values[0] = 0.6
        values[1] = 0.8
        return values.withUnsafeBufferPointer { Data(buffer: $0) }
    }

    /// One image analyzed by an "earlier scan": tags with scores, a Year and a
    /// camera tag, two faces (one held back for its pose), text and an embedding.
    private static func library() async throws -> Library {
        let dir = try Fixtures.directory("FileIDCatalogTwin")
        let db = try Database(at: dir.appendingPathComponent("catalog.sqlite"))
        let bytes = try Fixtures.noisyImage(type: .jpeg, seed: 0xCA7A106)
        try #require(bytes.count >= 4096)
        let original = dir.appendingPathComponent("original.jpg")
        try bytes.write(to: original)
        let hash = try #require(ContentHash.compute(url: original, size: UInt64(bytes.count)))
        let clip = unitClip()
        var analyzed = TaggedFile(
            url: original, kind: "image", extension: "jpg", sizeBytes: Int64(bytes.count),
            createdAt: Date(timeIntervalSince1970: 1_560_000_000), modifiedAt: Date(timeIntervalSince1970: 1_560_000_000),
            fileRef: 11,
            visionTags: ["beach", "dog", "Year_2019", "Canon"], tagScores: ["beach": 0.9, "dog": 0.75], phash: 0xFEED_0000_0000_0042,
            contentHash: hash, aestheticScore: 0.625, hasFaces: true,
            facePrints: [Data(), Data()], faceBBoxes: ["0.1,0.1,0.3,0.3", "0.6,0.2,0.2,0.2"],
            faceQualities: [0.9, 0.8], faceYaws: [0.0, 1.5], facePitches: [0.0, 0.0], faceMinDimPx: [400, 300],
            ocrText: "grocery list milk eggs", cameraModel: "Canon EOS R5", locationLat: 48.1, locationLon: 11.5,
            clipEmbeddingBlob: clip, tagsEvaluated: true, facesEvaluated: true, ocrStageRan: true)
        analyzed.ocrHandwritten = true
        let library = Library(dir: dir, db: db, bytes: bytes, hash: hash, original: original, clip: clip)
        await library.write(analyzed)
        try #require(try library.fileID(original) != nil)
        try #require(try library.tags(original) == ["beach:0.9", "dog:0.75", "Year_2019:none", "Canon:none"])
        return library
    }

    private static func options(_ calls: ScanImageFixtures.Counter) -> ScanCatchUp.ScanOptions {
        ScanCatchUp.ScanOptions(
            mode: .inline,
            tag: { _ in
                calls.bump()
                return []
            },
            carefulRead: { _ in
                calls.bump()
                return nil
            })
    }

    private static func seeded(_ library: Library, _ provenance: TwinCatalogStore.Provenance = provenance) async -> CatalogTwins? {
        await CatalogTwins.seeded(database: library.db, provenance: { provenance })
    }

    @Test("a second copy takes the stored analysis: same tags, faces, text and embedding, its own path and Year, no model run")
    func copyReusesCatalogRow() async throws {
        let library = try await Self.library()
        defer { try? FileManager.default.removeItem(at: library.dir) }
        let copy = library.dir.appendingPathComponent("backup").appendingPathComponent("copy of original.JPG")
        try FileManager.default.createDirectory(at: copy.deletingLastPathComponent(), withIntermediateDirectories: true)
        try library.bytes.write(to: copy)
        let originalFaces = try library.faces(library.original)
        #expect(originalFaces.count == 2 && originalFaces[0].hasSuffix("|0") && originalFaces[1].hasSuffix("|1"),
                "\(originalFaces)")

        let catalog = try #require(await Self.seeded(library))
        #expect(catalog.count == 1)
        let registry = TwinRegistry()
        let calls = Fixtures.Counter()
        let discovered = Fixtures.discovered(copy, size: library.bytes.count, year: 2023, ref: 77)
        let tagged = await Tagging.processFile(
            discovered: discovered, worker: VisionWorker(), twins: registry, catalog: catalog, options: Self.options(calls))

        #expect(calls.count == 0, "no tagger or reader ran")
        #expect(tagged.loadMs == 0 && tagged.visionMs == 0 && tagged.clipMs == 0 && tagged.ramMs == 0 && tagged.ocrMs == 0)
        #expect(registry.counts.catalog == 1 && registry.counts.analyzed == 0 && registry.counts.reused == 0)
        #expect(tagged.url == copy && tagged.extension == "jpg" && tagged.fileRef == 77)
        #expect(tagged.createdAt == discovered.creationDate && tagged.modifiedAt == discovered.modificationDate)
        #expect(tagged.visionTags == ["beach", "dog", "Year_2023", "Canon"])
        #expect(tagged.tagScores == ["beach": 0.9, "dog": 0.75])
        #expect(tagged.phash == 0xFEED_0000_0000_0042)
        #expect(tagged.contentHash == library.hash)
        #expect(tagged.aestheticScore == 0.625)
        #expect(tagged.hasFaces)
        #expect(tagged.faceBBoxes == ["0.1,0.1,0.3,0.3", "0.6,0.2,0.2,0.2"])
        #expect(tagged.faceQualities == [0.9, 0.8])
        #expect(tagged.faceExcluded == [false, true])
        #expect(tagged.facePrints == [Data(), Data()])
        #expect(tagged.ocrText == "grocery list milk eggs" && tagged.ocrHandwritten)
        #expect(tagged.clipEmbeddingBlob == library.clip)
        #expect(tagged.cameraModel == "Canon EOS R5" && tagged.locationLat == 48.1 && tagged.locationLon == 11.5)
        #expect(tagged.tagsEvaluated && tagged.facesEvaluated && tagged.ocrStageRan)
        #expect(!tagged.failed && tagged.catchUp == nil)

        let originalID = try #require(try library.fileID(library.original))
        await library.write(tagged)
        let copyID = try #require(try library.fileID(copy))
        #expect(copyID != originalID)
        #expect(try library.fileID(library.original) == originalID)
        #expect(try library.tags(copy) == ["beach:0.9", "dog:0.75", "Year_2023:none", "Canon:none"])
        #expect(try library.tags(library.original) == ["beach:0.9", "dog:0.75", "Year_2019:none", "Canon:none"])
        #expect(try library.faces(copy) == originalFaces)
        #expect(try library.faces(library.original) == originalFaces)
        #expect(try library.summary(copyID) == "1|1|0.625|Canon EOS R5|grocery list milk eggs|1|\(CLIPEmbeddingSpace.modelID)")
        #expect(try library.embedding(copyID) == library.clip)

        // A third copy in the same scan takes the result now held in memory.
        let third = library.dir.appendingPathComponent("third.jpg")
        try library.bytes.write(to: third)
        let again = await Tagging.processFile(
            discovered: Fixtures.discovered(third, size: library.bytes.count, year: 2021, ref: 78),
            worker: VisionWorker(), twins: registry, catalog: catalog, options: Self.options(calls))
        #expect(calls.count == 0)
        #expect(registry.counts.catalog == 1 && registry.counts.reused == 1)
        #expect(again.visionTags == ["beach", "dog", "Year_2021", "Canon"])
        #expect(again.faceExcluded == [false, true] && again.clipEmbeddingBlob == library.clip)
    }

    @Test("a file whose date changed but whose bytes did not keeps its row, faces and people untouched")
    func touchedFileKeepsItsRow() async throws {
        let library = try await Self.library()
        defer { try? FileManager.default.removeItem(at: library.dir) }
        let fileID = try #require(try library.fileID(library.original))
        try await library.db.pool.write { db in
            try db.execute(sql: "INSERT INTO persons (name, file_count, created_at) VALUES ('Ada', 1, 0)")
            try db.execute(sql: "UPDATE face_prints SET person_id = ? WHERE file_id = ? AND excluded = 0",
                           arguments: [db.lastInsertedRowID, fileID])
            try db.execute(sql: "INSERT INTO tags (file_id, tag, source) VALUES (?, 'holiday', 'user')", arguments: [fileID])
        }
        let faceRows = { () throws -> [String] in
            try library.db.pool.read {
                try String.fetchAll($0, sql: """
                    SELECT id || '|' || COALESCE(person_id, 'none') || '|' || bbox FROM face_prints WHERE file_id = ? ORDER BY id
                    """, arguments: [fileID])
            }
        }
        let facesBefore = try faceRows()
        #expect(facesBefore.count == 2 && !facesBefore[0].contains("none"))

        let catalog = try #require(await Self.seeded(library))
        let registry = TwinRegistry()
        let calls = Fixtures.Counter()
        let touched = Date(timeIntervalSince1970: 1_700_000_000)
        let discovered = DiscoveredFile(
            url: library.original, sizeBytes: Int64(library.bytes.count),
            creationDate: Date(timeIntervalSince1970: 1_560_000_000), modificationDate: touched, kind: .image, fileRef: 11)
        let tagged = await Tagging.processFile(
            discovered: discovered, worker: VisionWorker(), twins: registry, catalog: catalog, options: Self.options(calls))

        #expect(calls.count == 0)
        #expect(registry.counts.catalog == 1)
        #expect(tagged.url == library.original && tagged.modifiedAt == touched && tagged.contentHash == library.hash)
        #expect(!tagged.failed)
        #expect(!tagged.tagsEvaluated && !tagged.facesEvaluated && !tagged.ocrStageRan,
                "nothing asks the writer to rebuild the row's analysis")
        #expect(tagged.clipEmbeddingBlob == library.clip, "the catalog drops the vector of a re-dated file; it is put back")

        await library.write(tagged)
        #expect(try library.fileID(library.original) == fileID)
        #expect(try faceRows() == facesBefore, "face rows and their people survive")
        #expect(try library.tags(library.original) == ["beach:0.9", "dog:0.75", "Year_2019:none", "Canon:none"])
        #expect(try library.summary(fileID) == "1|1|0.625|Canon EOS R5|grocery list milk eggs|1|\(CLIPEmbeddingSpace.modelID)")
        #expect(try library.embedding(fileID) == library.clip)
        let state = try await library.db.pool.read { db in
            try String.fetchOne(db, sql: """
                SELECT f.failed || '|' || (f.modified_at = ?) || '|' ||
                       (SELECT COUNT(*) FROM tags t WHERE t.file_id = f.id AND t.source = 'user')
                FROM files f WHERE f.id = ?
                """, arguments: [touched.timeIntervalSince1970, fileID])
        }
        #expect(state == "0|1|1", "not failed, new date stored, the user's tag kept")

        // The row's analysis is also offered to copies met later in the scan.
        let copy = library.dir.appendingPathComponent("copy.jpg")
        try library.bytes.write(to: copy)
        let second = await Tagging.processFile(
            discovered: Fixtures.discovered(copy, size: library.bytes.count, year: 2022, ref: 12),
            worker: VisionWorker(), twins: registry, catalog: catalog, options: Self.options(calls))
        #expect(calls.count == 0 && registry.counts.reused == 1)
        #expect(second.visionTags == ["beach", "dog", "Year_2022", "Canon"] && second.tagsEvaluated)
    }

    private static func expectAnalyzedItself(
        _ library: Library, catalog: CatalogTwins?, _ comment: Comment
    ) async throws {
        let copy = library.dir.appendingPathComponent("copy-\(UUID().uuidString).jpg")
        try library.bytes.write(to: copy)
        let registry = TwinRegistry()
        let tagged = await Tagging.processFile(
            discovered: Fixtures.discovered(copy, size: library.bytes.count, year: 2023, ref: 90),
            worker: VisionWorker(), twins: registry, catalog: catalog, options: options(Fixtures.Counter()))
        #expect(registry.counts.catalog == 0, comment)
        #expect(tagged.clipEmbeddingBlob != library.clip, comment)
        #expect(tagged.ocrText != "grocery list milk eggs", comment)
        #expect(tagged.faceExcluded == nil, comment)
        #expect(tagged.failed || tagged.loadMs > 0, comment)
    }

    @Test("a row the catch-up still owes work is not reused")
    func pendingCatchUpNotReused() async throws {
        let library = try await Self.library()
        defer { try? FileManager.default.removeItem(at: library.dir) }
        let catalog = try #require(await Self.seeded(library))
        let fileID = try #require(try library.fileID(library.original))
        try await library.db.pool.write { db in
            try db.execute(sql: """
                INSERT INTO scan_catchup (file_id, stages, size_bytes, modified_at, face_count, attempts, created_at)
                VALUES (?, ?, ?, 0, 0, 0, 0)
                """, arguments: [fileID, ScanCatchUp.tagsOwed, library.bytes.count])
        }
        #expect(await Self.seeded(library) == nil, "a scan that starts now does not list the row")
        try await Self.expectAnalyzedItself(library, catalog: catalog, "the marker appeared after the scan was seeded")
    }

    @Test("a row made by other models is not reused")
    func otherModelsNotReused() async throws {
        let library = try await Self.library()
        defer { try? FileManager.default.removeItem(at: library.dir) }
        let fileID = try #require(try library.fileID(library.original))

        var otherCLIP = Self.provenance
        otherCLIP.clipModel = "clip-from-another-release"
        try await Self.expectAnalyzedItself(library, catalog: await Self.seeded(library, otherCLIP), "embedding from another CLIP")

        try await library.db.pool.write { db in
            try db.execute(sql: "UPDATE face_prints SET processing_version = 'old-face-pipeline' WHERE file_id = ?", arguments: [fileID])
        }
        try await Self.expectAnalyzedItself(library, catalog: await Self.seeded(library), "faces from another face pipeline")

        try await library.db.pool.write { db in
            try db.execute(sql: "UPDATE face_prints SET processing_version = ?, embedding_model = 'sface-old' WHERE file_id = ?",
                           arguments: [FaceAnalysisCache.processingVersion, fileID])
        }
        try await Self.expectAnalyzedItself(library, catalog: await Self.seeded(library), "faces embedded by another model")

        try await library.db.pool.write { db in
            try db.execute(sql: "UPDATE face_prints SET embedding_model = 'sface-test' WHERE file_id = ?", arguments: [fileID])
            try db.execute(sql: "UPDATE tags SET score = NULL WHERE file_id = ?", arguments: [fileID])
        }
        try await Self.expectAnalyzedItself(library, catalog: await Self.seeded(library), "tags from before the tagger was installed")

        var noTagger = Self.provenance
        noTagger.taggerLoaded = false
        let catalog = try #require(await Self.seeded(library, noTagger))
        let copy = library.dir.appendingPathComponent("accepted.jpg")
        try library.bytes.write(to: copy)
        let registry = TwinRegistry()
        _ = await Tagging.processFile(
            discovered: Fixtures.discovered(copy, size: library.bytes.count, year: 2023, ref: 91),
            worker: VisionWorker(), twins: registry, catalog: catalog, options: Self.options(Fixtures.Counter()))
        #expect(registry.counts.catalog == 1, "with matching provenance the same row is reused")

        try await library.db.pool.write { db in
            try db.execute(sql: "DELETE FROM clip_embeddings WHERE file_id = ?", arguments: [fileID])
        }
        try await Self.expectAnalyzedItself(library, catalog: catalog, "no embedding while CLIP is loaded")
    }

    @Test("failed rows, other kinds and reuse switched off never reuse")
    func neverReused() async throws {
        let library = try await Self.library()
        defer { try? FileManager.default.removeItem(at: library.dir) }
        let catalog = try #require(await Self.seeded(library))

        // FILEID_TWIN_REUSE=0 hands the scan no registry: the catalog is not consulted.
        let copy = library.dir.appendingPathComponent("switched-off.jpg")
        try library.bytes.write(to: copy)
        let calls = Fixtures.Counter()
        let alone = await Tagging.processFile(
            discovered: Fixtures.discovered(copy, size: library.bytes.count, year: 2023, ref: 92),
            worker: VisionWorker(), twins: nil, catalog: catalog, options: Self.options(calls))
        #expect(alone.clipEmbeddingBlob != library.clip && alone.faceExcluded == nil)

        let fileID = try #require(try library.fileID(library.original))
        try await library.db.pool.write { db in
            try db.execute(sql: "UPDATE files SET failed = 1 WHERE id = ?", arguments: [fileID])
        }
        #expect(await Self.seeded(library) == nil)
        try await Self.expectAnalyzedItself(library, catalog: catalog, "the row failed after the scan was seeded")

        #expect(!catalog.contains(TwinKey(hash: library.hash, size: Int64(library.bytes.count) + 1)))
        #expect(!catalog.contains(TwinKey(hash: Data(repeating: 7, count: 32), size: Int64(library.bytes.count))))
        #expect(catalog.contains(TwinKey(hash: library.hash, size: Int64(library.bytes.count))))
    }
}
