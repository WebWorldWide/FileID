import AsyncAlgorithms
import CoreGraphics
import Foundation
import GRDB
import ImageIO
import Testing
import UniformTypeIdentifiers
@testable import FileIDEngine
@testable import FileIDImaging
private typealias Database = FileIDEngine.Database

@Suite("Scan catch-up: deferred tags and careful text reads")
struct ScanCatchUpTests {

    private final class Counter: @unchecked Sendable {
        private let lock = NSLock()
        private var value = 0
        func bump() -> Int { lock.lock(); defer { lock.unlock() }; value += 1; return value }
        var count: Int { lock.lock(); defer { lock.unlock() }; return value }
    }

    private static func directory() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDCatchUp-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private static func writeImage(_ url: URL, seed: UInt64) throws {
        let width = 320, height = 240
        var state = seed
        var pixels = [UInt8](repeating: 255, count: width * height * 4)
        for index in stride(from: 0, to: pixels.count, by: 4) {
            state = state &* 6364136223846793005 &+ 1442695040888963407
            pixels[index] = UInt8(truncatingIfNeeded: state >> 33)
            pixels[index + 1] = UInt8(truncatingIfNeeded: state >> 41)
            pixels[index + 2] = UInt8(truncatingIfNeeded: state >> 49)
        }
        let image = try #require(pixels.withUnsafeMutableBytes { raw in
            CGContext(data: raw.baseAddress, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                      space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)?.makeImage()
        })
        let destination = try #require(CGImageDestinationCreateWithURL(url as CFURL, UTType.jpeg.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(destination, image, nil)
        #expect(CGImageDestinationFinalize(destination))
    }

    private static func stat(_ url: URL) throws -> (size: Int64, modified: Date?, created: Date?) {
        let values = try url.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey, .creationDateKey])
        return (Int64(values.fileSize ?? 0), values.contentModificationDate, values.creationDate)
    }

    private static func discovered(_ url: URL) throws -> DiscoveredFile {
        let info = try stat(url)
        return DiscoveredFile(url: url, sizeBytes: info.size, creationDate: info.created,
                              modificationDate: info.modified, kind: .image, fileRef: nil)
    }

    private static func store(_ files: [TaggedFile], in db: Database, force: Bool = false) async {
        let writer = DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(),
                              sessionID: UUID().uuidString, forceReprocess: force)
        let channel = AsyncChannel<TaggedFile>()
        let producer = Task {
            for file in files { await channel.send(file) }
            channel.finish()
        }
        await writer.drain(channel)
        await producer.value
    }

    private static let classifierConfidences: [String: Float] = ["outdoor": 0.8, "water": 0.12, "sky": 0.31]

    private static func owingTags(_ url: URL) throws -> TaggedFile {
        let info = try stat(url)
        var file = TaggedFile(
            url: url, kind: "image", extension: "jpg", sizeBytes: info.size,
            createdAt: info.created, modifiedAt: info.modified,
            visionTags: ["outdoor", "Year_1999"], tagsEvaluated: true, facesEvaluated: true)
        file.catchUp = ScanCatchUp.Marker(
            stages: ScanCatchUp.tagsOwed,
            vision: ScanCatchUp.VisionContext(classifyTags: ["outdoor"], confidences: classifierConfidences).encoded,
            faceCount: 0)
        return file
    }

    private static func readers(
        tag: @escaping @Sendable (CGImage) -> [(tag: String, score: Float)] = { _ in [] },
        fast: String = "",
        careful: @escaping @Sendable (CGImage) -> String? = { _ in nil },
        carefulReady: Bool = true
    ) -> ScanCatchUp.Readers {
        ScanCatchUp.Readers(taggerReady: { true }, tag: tag, fastRead: { _ in fast }, carefulRead: careful,
                            carefulReady: { carefulReady }, carefulWaitSeconds: 0)
    }

    private static func autoTags(_ db: Database, path: String) async throws -> [String: Double?] {
        try await db.pool.read { db in
            var out: [String: Double?] = [:]
            for row in try Row.fetchAll(db, sql: """
                SELECT tag, score FROM tags WHERE source = 'auto' AND file_id = (SELECT id FROM files WHERE path_text = ?)
                """, arguments: [path]) {
                out[row["tag"]] = row["score"] as Double?
            }
            return out
        }
    }

    private static func stages(_ db: Database, path: String) async throws -> Int? {
        try await db.pool.read { db in
            try Int.fetchOne(db, sql: """
                SELECT stages FROM scan_catchup WHERE file_id = (SELECT id FROM files WHERE path_text = ?)
                """, arguments: [path])
        }
    }

    @Test("RAM++ is deferred when it is loaded, and FILEID_RAMPLUS_INLINE=1 keeps it in the scan")
    func modeSwitch() {
        #expect(ScanCatchUp.RamPlusMode.resolve(environment: [:], taggerReady: true) == .deferred)
        #expect(ScanCatchUp.RamPlusMode.resolve(environment: ["FILEID_RAMPLUS_INLINE": "1"], taggerReady: true) == .inline)
        #expect(ScanCatchUp.RamPlusMode.resolve(environment: ["FILEID_RAMPLUS_INLINE": "0"], taggerReady: true) == .deferred)
        #expect(ScanCatchUp.RamPlusMode.resolve(environment: [:], taggerReady: false) == .inline)
    }

    @Test("the stored classifier context gives the same combined tags as the full classifier output")
    func visionContextRoundTrip() throws {
        let full: [String: Float] = ["outdoor": 0.8, "canoe": 0.02, "frisbee": 0.004, "tree": 0.29, "dog": 0.3000001]
        let context = try #require(ScanCatchUp.VisionContext.decode(
            ScanCatchUp.VisionContext(classifyTags: ["outdoor", "dog"], confidences: full).encoded))
        let ram: [(tag: String, score: Float)] = [("canoe", 0.9), ("frisbee", 0.95), ("dog", 0.7), ("tree", 0.8), ("man", 0.9)]
        for faces in [0, 2] {
            let direct = TagAgreement.combine(ram: ram, vision: full, faceCount: faces, peopleWords: RamPlusService.peopleWords)
            let stored = TagAgreement.combine(ram: ram, vision: context.confidences, faceCount: faces, peopleWords: RamPlusService.peopleWords)
            #expect(direct.map(\.tag) == stored.map(\.tag))
            #expect(direct.map(\.score) == stored.map(\.score))
        }
        // A classifier that answered with nothing above the stored cut-offs still vetoes.
        let quiet = try #require(ScanCatchUp.VisionContext.decode(
            ScanCatchUp.VisionContext(classifyTags: [], confidences: ["tree": 0.05]).encoded))
        #expect(TagAgreement.combine(ram: [("canoe", 0.9)], vision: quiet.confidences, faceCount: 0, peopleWords: RamPlusService.peopleWords).isEmpty)
        let silent = try #require(ScanCatchUp.VisionContext.decode(
            ScanCatchUp.VisionContext(classifyTags: [], confidences: [:]).encoded))
        #expect(TagAgreement.combine(ram: [("canoe", 0.9)], vision: silent.confidences, faceCount: 0, peopleWords: RamPlusService.peopleWords).count == 1)
    }

    @Test("the writer records what a file is owed with its row and clears it on a full re-analysis")
    func writerSetsAndClearsMarker() async throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let db = try Database(at: dir.appendingPathComponent("test.sqlite"))
        let url = dir.appendingPathComponent("a.jpg")
        try Self.writeImage(url, seed: 1)

        await Self.store([try Self.owingTags(url)], in: db)
        #expect(try await Self.stages(db, path: url.path) == ScanCatchUp.tagsOwed)
        #expect(await ScanCatchUp.owed(database: db, root: dir.path) == .init(tags: 1, careful: 0))
        #expect(await ScanCatchUp.owed(database: db, root: "/somewhere/else") == .init(tags: 0, careful: 0))

        // An unchanged file that passes through again keeps its marker.
        var again = try Self.owingTags(url)
        again.catchUp = nil
        await Self.store([again], in: db)
        #expect(try await Self.stages(db, path: url.path) == ScanCatchUp.tagsOwed)

        // A full re-analysis that owes nothing clears it.
        await Self.store([again], in: db, force: true)
        #expect(try await Self.stages(db, path: url.path) == nil)
    }

    @Test("v36 moves failure memory out of scan_catchup and leaves owed rows alone")
    func v36MovesFailureMemory() throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let q = try DatabaseQueue(path: dir.appendingPathComponent("v35.sqlite").path)
        try Database.migrator.migrate(q, upTo: "v35_foreign_key_indexes")
        try q.write { db in
            try db.execute(sql: """
                INSERT INTO files (id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed) VALUES
                  (1,'/a.jpg',1,100,0,'image','jpg',1),(2,'/b.jpg',2,200,0,'image','jpg',0),
                  (3,'/c.jpg',3,300,0,'image','jpg',1),(4,'/d.jpg',4,400,0,'image','jpg',0);
                INSERT INTO scan_catchup(file_id,stages,size_bytes,modified_at,vision,face_count,attempts,created_at) VALUES
                  (1,4,100,10.5,'{"failedBy":"fileid-engine 1.2.3 apple"}',0,3,77.0),
                  (2,1,200,20.0,'{"k":[],"c":[],"u":false}',2,1,5.0),
                  (3,4,300,NULL,'not json',0,2,78.0),
                  (4,6,400,40.0,NULL,0,0,6.0);
                """)
        }
        try Database.migrator.migrate(q)
        try q.read { db in
            let failures = try Row.fetchAll(db, sql: "SELECT * FROM scan_failures ORDER BY file_id")
            #expect(failures.count == 2)
            // One typed value per check: a chain of untyped row subscripts in one
            // #expect is more than the CI compiler type-checks in its time limit.
            let first = failures[0], second = failures[1]
            let firstID: Int64 = first["file_id"], firstSize: Int64 = first["size_bytes"]
            let firstModified: Double = first["modified_at"], firstFailedAt: Double = first["failed_at"]
            let firstBuild: String = first["engine_build"], firstAttempts: Int = first["attempts"]
            #expect(firstID == 1)
            #expect(firstSize == 100)
            #expect(firstModified == 10.5)
            #expect(firstBuild == "fileid-engine 1.2.3 apple")
            #expect(firstAttempts == 3)
            #expect(firstFailedAt == 77.0)
            let secondID: Int64 = second["file_id"], secondBuild: String = second["engine_build"]
            let secondAttempts: Int = second["attempts"]
            #expect(secondID == 3)
            #expect(secondBuild == "not json")
            #expect(secondAttempts == 2)
            let owed = try Row.fetchAll(db, sql: "SELECT file_id, stages FROM scan_catchup ORDER BY file_id")
            #expect(owed.map { [$0["file_id"] as Int, $0["stages"] as Int] } == [[2, 1], [4, 6]])
        }
        try q.close()
    }

    @Test("a full analysis forgets a remembered failure")
    func analysisClearsFailureMemory() async throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let db = try Database(at: dir.appendingPathComponent("test.sqlite"))
        let url = dir.appendingPathComponent("a.jpg")
        try Self.writeImage(url, seed: 1)
        var file = try Self.owingTags(url)
        file.catchUp = nil
        await Self.store([file], in: db)
        try await db.pool.write { db in
            try db.execute(sql: """
                INSERT INTO scan_failures(file_id,size_bytes,modified_at,engine_build,attempts,failed_at)
                SELECT id,size_bytes,modified_at,'fileid-engine 1.0 apple',3,0 FROM files
                """)
        }
        await Self.store([file], in: db, force: true)
        let left = try await db.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM scan_failures") }
        #expect(left == 0)
    }

    @Test("the rename-heal lookup searches by index, not by scanning files")
    func healLookupUsesIndexes() throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let q = try DatabaseQueue(path: dir.appendingPathComponent("heal.sqlite").path)
        try Database.migrator.migrate(q)
        try q.write { db in
            try db.execute(sql: """
                WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 20000)
                INSERT INTO files (path_text,path_hash,size_bytes,scanned_at,kind,extension,file_ref,content_hash)
                SELECT '/p/'||i||'.jpg', i, i, 0, 'image', 'jpg', CASE WHEN i % 10 = 0 THEN NULL ELSE i END, randomblob(32) FROM n;
                ANALYZE;
                """)
            let plan = try Row.fetchAll(db, sql: """
                EXPLAIN QUERY PLAN SELECT id, path_text,
                       (?2 IS NOT NULL AND file_ref IS NOT NULL AND file_ref = ?2 AND size_bytes = ?4) AS by_ref
                FROM files WHERE path_text != ?1
                  AND ((file_ref IS NOT NULL AND file_ref = ?2 AND size_bytes = ?4)
                       OR (content_hash IS NOT NULL AND content_hash = ?3))
                ORDER BY by_ref DESC LIMIT 32
                """, arguments: ["/x", 5, Data(repeating: 0, count: 32), 5]).map { $0["detail"] as String }.joined(separator: " | ")
            #expect(plan.contains("USING INDEX") && !plan.contains("SCAN files"), Comment(rawValue: plan))
        }
        try q.close()
    }

    @Test("a copy made from an identical file inherits what the original is owed")
    func twinInheritsMarker() throws {
        var tagged = TaggedFile(
            url: URL(fileURLWithPath: "/tmp/a.jpg"), kind: "image", extension: "jpg", sizeBytes: 5000,
            createdAt: nil, modifiedAt: nil, visionTags: ["outdoor"], tagsEvaluated: true)
        tagged.catchUp = ScanCatchUp.Marker(stages: 3, vision: "{}", faceCount: 2)
        let twin = TwinAnalysis(tagged: tagged, modelTags: ["outdoor"], models: "100")
        let copy = Tagging.twinCopy(
            of: twin,
            for: DiscoveredFile(url: URL(fileURLWithPath: "/tmp/b.jpg"), sizeBytes: 5000, creationDate: nil,
                                modificationDate: nil, kind: .image, fileRef: nil),
            contentHash: Data(repeating: 1, count: 32), elapsedMs: 1)
        #expect(copy.catchUp == tagged.catchUp)
    }

    @Test("the catch-up replaces auto tags, keeps user tags, clears the marker and resumes after a cancel")
    func tagCatchUpResumes() async throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let db = try Database(at: dir.appendingPathComponent("test.sqlite"))
        let count = ScanCatchUp.chunkSize * 2 + 3
        var urls: [URL] = []
        for index in 0..<count {
            let url = dir.appendingPathComponent("photo-\(index).jpg")
            try Self.writeImage(url, seed: UInt64(index + 1))
            urls.append(url)
        }
        await Self.store(try urls.map(Self.owingTags), in: db)
        try await db.pool.write { db in
            try db.execute(sql: "INSERT INTO tags (file_id, tag, source) SELECT id, 'holiday', 'user' FROM files")
            try db.execute(sql: "INSERT INTO tags (file_id, tag, source) SELECT id, 'dog', 'user' FROM files")
        }

        let ram: [(tag: String, score: Float)] = [("canoe", 0.97), ("dog", 0.9), ("sky", 0.7)]
        let calls = Counter()
        let stop = Counter()
        let progress = Counter()
        let first = await ScanCatchUp.run(
            database: db, root: dir.path,
            readers: Self.readers(tag: { _ in
                if calls.bump() >= 3 { _ = stop.bump() }
                return ram
            }),
            isCancelled: { stop.count > 0 }, useAdmission: false)
        #expect(first.cancelled)
        #expect(first.owedTags == count)
        let remaining = await ScanCatchUp.owed(database: db, root: dir.path).tags
        #expect(remaining > 0 && remaining == count - first.tagged)

        let second = await ScanCatchUp.run(
            database: db, root: dir.path, readers: Self.readers(tag: { _ in ram }), useAdmission: false
        ) { event in
            if case .advance(let files) = event { for _ in 0..<files { _ = progress.bump() } }
        }
        #expect(!second.cancelled)
        #expect(second.tagged == remaining)
        #expect(progress.count == remaining)
        #expect(await ScanCatchUp.owed(database: db, root: nil) == .init(tags: 0, careful: 0))

        for url in urls {
            let expected = ScanCatchUp.imageTags(
                ram: ram, classifyTags: ["outdoor"], vision: Self.classifierConfidences, faceCount: 0,
                cameraModel: nil, creationDate: try Self.stat(url).created)
            let stored = try await Self.autoTags(db, path: url.path)
            #expect(Set(stored.keys) == Set(expected.stored))
            #expect(!expected.stored.contains("canoe") && expected.stored.contains("dog"))
            #expect(!stored.keys.contains("Year_1999"))
            for (tag, score) in stored { #expect(score == expected.scores?[tag]) }
            #expect(try await Self.stages(db, path: url.path) == nil)
        }
        let userTags = try await db.pool.read { db in
            try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM tags WHERE source = 'user'") ?? 0
        }
        #expect(userTags == count * 2)
    }

    @Test("a file that changed since the scan is left for the next scan")
    func changedFileIsLeft() async throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let db = try Database(at: dir.appendingPathComponent("test.sqlite"))
        let changed = dir.appendingPathComponent("changed.jpg")
        let gone = dir.appendingPathComponent("gone.jpg")
        try Self.writeImage(changed, seed: 5)
        try Self.writeImage(gone, seed: 6)
        await Self.store([try Self.owingTags(changed), try Self.owingTags(gone)], in: db)
        try FileManager.default.setAttributes([.modificationDate: Date(timeIntervalSince1970: 1_500_000_000)],
                                              ofItemAtPath: changed.path)
        try FileManager.default.removeItem(at: gone)

        let summary = await ScanCatchUp.run(
            database: db, root: dir.path, readers: Self.readers(tag: { _ in [("dog", 0.9)] }), useAdmission: false)
        #expect(summary.tagged == 0 && summary.left == 2)
        #expect(try await Self.autoTags(db, path: changed.path).keys.sorted() == ["Year_1999", "outdoor"])
        #expect(await ScanCatchUp.owed(database: db, root: dir.path).tags == 2)
    }

    @Test("a careful read the scan could not run is owed, then done by the catch-up")
    func carefulReadCatchUp() async throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let db = try Database(at: dir.appendingPathComponent("test.sqlite"))
        let url = dir.appendingPathComponent("note.jpg")
        try Self.writeImage(url, seed: 9)

        // Tagged "handwriting", so the scan wants a careful read; the reader is not ready.
        let options = ScanCatchUp.ScanOptions(mode: .inline, tag: { _ in [("handwriting", 0.9)] }, carefulRead: { _ in nil })
        let scanned = await Tagging.processFile(discovered: try Self.discovered(url), worker: VisionWorker(), twins: nil, options: options)
        if scanned.failed { return }
        #expect(scanned.ocrStageRan)
        #expect(scanned.visionTags.contains("handwriting"))
        let marker = try #require(scanned.catchUp)
        #expect(marker.stages == ScanCatchUp.carefulTextOwed | ScanCatchUp.textReadAtScan)
        await Self.store([scanned], in: db)
        #expect(await ScanCatchUp.owed(database: db, root: dir.path) == .init(tags: 0, careful: 1))

        // Reader still warming up: nothing changes and the read stays owed.
        let waiting = await ScanCatchUp.run(database: db, root: dir.path,
                                            readers: Self.readers(carefulReady: false), useAdmission: false)
        #expect(waiting.carefulRead == 0)
        // Reader paused by its breaker part-way: still owed.
        let paused = await ScanCatchUp.run(database: db, root: dir.path, readers: Self.readers(), useAdmission: false)
        #expect(paused.carefulRead == 0)
        #expect(await ScanCatchUp.owed(database: db, root: dir.path).careful == 1)

        let done = await ScanCatchUp.run(
            database: db, root: dir.path,
            readers: Self.readers(fast: "Dcar diarg", careful: { _ in "Dear diary, today we went sailing" }),
            useAdmission: false)
        #expect(done.carefulRead == 1)
        let stored: [String] = try await db.pool.read { db in
            guard let row = try Row.fetchOne(db, sql: """
                SELECT o.text, o.handwritten, o.source, f.has_text FROM ocr_text o JOIN files f ON f.id = o.file_id
                WHERE f.path_text = ?
                """, arguments: [url.path]) else { return [] }
            return [row["text"] as String? ?? "", "\(row["handwritten"] as Int? ?? -1)",
                    row["source"] as String? ?? "", "\(row["has_text"] as Int? ?? -1)"]
        }
        #expect(stored == ["Dear diary, today we went sailing", "1", "ocr", "1"])
        #expect(try await Self.stages(db, path: url.path) == nil)
        #expect(scanned.visionTags.sorted() == (try await Self.autoTags(db, path: url.path)).keys.sorted())
    }

    @Test("deferred tagging plus the catch-up stores exactly what inline tagging stores")
    func deferredMatchesInline() async throws {
        let dir = try Self.directory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let url = dir.appendingPathComponent("page.jpg")
        try Self.writeImage(url, seed: 21)
        let ram: [(tag: String, score: Float)] = [("receipt", 0.93), ("skateboard", 0.9), ("woman", 0.8), ("table", 0.7)]
        let carefulText = "TOTAL 12.50 thank you for shopping"
        let calls = Counter()
        let tagger: @Sendable (CGImage) -> [(tag: String, score: Float)] = { _ in _ = calls.bump(); return ram }
        let careful: @Sendable (CGImage) -> String? = { _ in carefulText }
        let worker = VisionWorker()

        // Control: FILEID_RAMPLUS_INLINE=1 runs the tagger inside the scan and owes nothing.
        let inline = await Tagging.processFile(
            discovered: try Self.discovered(url), worker: worker, twins: nil,
            options: .init(mode: .inline, tag: tagger, carefulRead: careful))
        if inline.failed { return }
        #expect(calls.count == 1)
        #expect(inline.catchUp == nil)
        #expect(inline.visionTags.contains("receipt"))

        let deferred = await Tagging.processFile(
            discovered: try Self.discovered(url), worker: worker, twins: nil,
            options: .init(mode: .deferred, tag: tagger, carefulRead: careful))
        if deferred.failed { return }
        #expect(calls.count == 1, "the scan must not run the tagger in deferred mode")
        #expect(!deferred.visionTags.contains("receipt"))
        #expect(deferred.tagScores == nil)
        #expect((deferred.catchUp?.stages ?? 0) & ScanCatchUp.tagsOwed != 0)

        let inlineDB = try Database(at: dir.appendingPathComponent("inline.sqlite"))
        let deferredDB = try Database(at: dir.appendingPathComponent("deferred.sqlite"))
        await Self.store([inline], in: inlineDB)
        await Self.store([deferred], in: deferredDB)
        let fast = worker.ocrFast
        let summary = await ScanCatchUp.run(
            database: deferredDB, root: dir.path,
            readers: ScanCatchUp.Readers(taggerReady: { true }, tag: tagger, fastRead: { fast($0) }, carefulRead: careful,
                                         carefulReady: { true }, carefulWaitSeconds: 0),
            useAdmission: false)
        #expect(summary.tagged == 1)
        #expect(try await Self.stages(deferredDB, path: url.path) == nil)

        let expectedTags = try await Self.autoTags(inlineDB, path: url.path)
        let actualTags = try await Self.autoTags(deferredDB, path: url.path)
        #expect(Set(actualTags.keys) == Set(inline.visionTags))
        #expect(actualTags.keys.sorted() == expectedTags.keys.sorted())
        for (tag, score) in actualTags { #expect(score == expectedTags[tag] ?? nil) }

        func text(_ db: Database) async throws -> [String] {
            try await db.pool.read { db in
                guard let row = try Row.fetchOne(db, sql: "SELECT o.text, o.handwritten, f.has_text FROM files f LEFT JOIN ocr_text o ON o.file_id = f.id") else { return [] }
                return ["\(row["text"] as String? ?? "-")", "\(row["handwritten"] as Int? ?? -1)", "\(row["has_text"] as Int? ?? -1)"]
            }
        }
        #expect(try await text(deferredDB) == (try await text(inlineDB)))
        #expect(try await text(deferredDB).first == carefulText)
    }

    @Test("catch-up progress grows the scan's counters and never rolls them back")
    func coordinatorProgress() async throws {
        let coordinator = ScanCoordinator()
        _ = await coordinator.startSession(rootDisplayPath: "x", epoch: await coordinator.nextScanEpoch())
        await coordinator.bumpDiscovered(to: 10)
        await coordinator.setTotal(10)
        await coordinator.bumpProcessed(by: 8)
        await coordinator.bumpFailed(by: 1)
        await coordinator.setPhase(.postScan)
        let before = try #require(await coordinator.snapshot())
        await coordinator.setCatchUp(total: 6)
        await coordinator.bumpCatchUp(by: 4)
        let during = try #require(await coordinator.snapshot())
        #expect(during.total - during.discovered == 6)
        #expect(during.processed + during.failed - during.discovered == 4)
        #expect(during.processed >= before.processed && during.discovered == before.discovered)
        await coordinator.endCatchUp()
        let after = try #require(await coordinator.snapshot())
        #expect(after.total == 10 && after.processed == 8)
    }
}
