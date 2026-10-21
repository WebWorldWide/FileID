import Foundation
import GRDB
import AsyncAlgorithms
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Handwriting text reading")
struct TextReadingTests {
    // Real `.fast` output on synthetic handwriting samples (Bradley Hand, Marker Felt).
    static let fastGroceries = "4roGeries for Swvbda\nZLIGGhbvbb avbd basbL\ntwo Levwovbs, oat\nGaLL AKvbt RosaLivbd"
    static let fastWhiteboard = "Quarterly roadMap\nship pelican beta\nfix syMc bugs\nowner.' PMitri"
    static let fastReceipt = "CORNER MARKET\nBananas 1 . 2 9\nCof fee beans 11.99\nTOTAL 13.28\nThank you"

    @Test func garbledFastReadsMarkHandwritingButNotPrint() {
        #expect(TextReading.garbled(Self.fastGroceries).share >= TextReading.garbledShareThreshold)
        #expect(TextReading.garbled(Self.fastWhiteboard).share >= TextReading.garbledShareThreshold)
        #expect(TextReading.garbled(Self.fastReceipt).count == 0)
        #expect(!TextReading.isGarbledWord("PDFs"))
        #expect(TextReading.isGarbledWord("denti&t"))
    }

    @Test func carefulReadRunsOnHandwritingShortOrMangledText() {
        #expect(TextReading.needsCarefulRead(fastText: Self.fastGroceries, tags: [], textBearing: false))
        #expect(TextReading.needsCarefulRead(fastText: "EXIT", tags: ["sign"], textBearing: false))
        #expect(TextReading.needsCarefulRead(fastText: Self.fastReceipt, tags: ["whiteboard"], textBearing: false))
        #expect(!TextReading.needsCarefulRead(fastText: Self.fastReceipt, tags: ["receipt"], textBearing: false))
        #expect(!TextReading.needsCarefulRead(
            fastText: "Quarterly revenue grew in every region this year", tags: ["document"], textBearing: true))
    }

    @Test func handwritingLabelNeedsATagOrAMangledFastRead() {
        #expect(TextReading.isHandwritten(fastText: Self.fastGroceries, carefulText: "Groceries for Sunday", tags: []))
        #expect(TextReading.isHandwritten(fastText: "", carefulText: "Margaret Hollis", tags: ["Sticky_Note"]))
        #expect(!TextReading.isHandwritten(fastText: "EXIT", carefulText: "EXIT ONLY", tags: ["sign"]))
        #expect(!TextReading.isHandwritten(fastText: Self.fastGroceries, carefulText: " ", tags: ["handwriting"]))
    }

    @Test func documentShapeMatchesLetterAndA4ButNotPhotos() {
        #expect(TextReading.isDocumentShape(width: 1700, height: 2200, hasCamera: false))
        #expect(TextReading.isDocumentShape(width: 2480, height: 3508, hasCamera: false))
        #expect(!TextReading.isDocumentShape(width: 4032, height: 3024, hasCamera: false))
        #expect(!TextReading.isDocumentShape(width: 1700, height: 2200, hasCamera: true))
    }

    @Test func transcriptionTargetsUnreadTextBearingImagesAndScannedPDFs() {
        #expect(TextReading.needsTranscription(kind: "image", tags: ["whiteboard"], ocrText: nil, ocrSource: nil, hasDocText: false))
        #expect(TextReading.needsTranscription(kind: "image", tags: ["Sticky_Note"], ocrText: "wifi paoword denti&t", ocrSource: "ocr", hasDocText: false))
        #expect(!TextReading.needsTranscription(kind: "image", tags: ["dog"], ocrText: nil, ocrSource: nil, hasDocText: false))
        #expect(!TextReading.needsTranscription(kind: "image", tags: ["document"],
                                                ocrText: "Quarterly revenue grew in every region this year", ocrSource: "ocr", hasDocText: false))
        #expect(!TextReading.needsTranscription(kind: "image", tags: ["whiteboard"], ocrText: "x", ocrSource: "vlm", hasDocText: false))
        #expect(TextReading.needsTranscription(kind: "pdf", tags: [], ocrText: nil, ocrSource: nil, hasDocText: false))
        #expect(!TextReading.needsTranscription(kind: "pdf", tags: [], ocrText: nil, ocrSource: nil, hasDocText: true))
        #expect(!TextReading.needsTranscription(kind: "video", tags: ["whiteboard"], ocrText: nil, ocrSource: nil, hasDocText: false))
    }

    @Test func parseTranscriptionStripsMarkerAndRejectsNone() {
        let parsed = TextReading.parseTranscription("Groceries for Sunday\nzucchini and basil\nHANDWRITTEN: yes")
        #expect(parsed?.text == "Groceries for Sunday\nzucchini and basil")
        #expect(parsed?.handwritten == true)
        #expect(TextReading.parseTranscription("NONE\nHANDWRITTEN: no") == nil)
        #expect(TextReading.parseTranscription("  none. ") == nil)
        #expect(TextReading.parseTranscription("EXIT\nHANDWRITTEN: no")?.handwritten == false)
    }

    @Test func scanStoresTheHandwritingFlagAndRescanKeepsATranscription() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        let url = directory.appendingPathComponent("IMG_4101.jpg")
        func scan(_ text: String, handwritten: Bool) async {
            var file = TaggedFile(url: url, kind: "image", extension: "jpg", sizeBytes: 4, createdAt: nil,
                                  modifiedAt: Date(timeIntervalSince1970: 1_700_000_000),
                                  ocrText: text, ocrStageRan: true)
            file.ocrHandwritten = handwritten
            let channel = AsyncChannel<TaggedFile>()
            let producer = Task { await channel.send(file); channel.finish() }
            await DBWriter(db: database, sink: IPCSink(), coordinator: ScanCoordinator(),
                           sessionID: UUID().uuidString).drain(channel)
            await producer.value
        }
        func rows() async throws -> [[String]] {
            try await database.pool.read { db in
                try Row.fetchAll(db, sql: "SELECT text,source,handwritten FROM ocr_text").map {
                    [$0["text"], $0["source"], String($0["handwritten"] as Int)]
                }
            }
        }
        await scan("Groceries for Sunday", handwritten: true)
        #expect(try await rows() == [["Groceries for Sunday", "ocr", "1"]])
        try await database.pool.write { db in
            let id = try #require(try Int64.fetchOne(db, sql: "SELECT id FROM files"))
            try DeepAnalyzeRunner.storeTranscription(db, fileID: id, text: "Groceries for Sunday\nzucchini", handwritten: true)
        }
        await scan("4roGeries for Swvbda", handwritten: false)
        #expect(try await rows() == [["Groceries for Sunday\nzucchini", "vlm", "1"]])
    }

    @Test func transcriptionIsSearchableAsHandwritingAndSurvivesRescanUntilTheFileChanges() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: """
                INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,modified_at)
                VALUES(1,'/internal/IMG_4102.jpg',1,100,0,'image','jpg',5)
                """)
            try db.execute(sql: "INSERT INTO tags(file_id,tag,source) VALUES(1,'whiteboard','auto')")
            #expect(try DeepAnalyzeRunner.transcriptionCandidate(db, fileID: 1))
            try DeepAnalyzeRunner.storeTranscription(db, fileID: 1, text: "Quarterly roadmap\nship pelican beta", handwritten: true)
            #expect(try !DeepAnalyzeRunner.transcriptionCandidate(db, fileID: 1))
        }
        let hits = try await database.pool.read { try CatalogStore.search($0, query: "pelican") }
        #expect(hits.contains { $0.fileID == 1 && $0.kind == "handwrittenText" && $0.evidenceID == "handwrittenText:1" })

        try await database.pool.write { db in
            try db.execute(sql: "UPDATE files SET modified_at=6 WHERE id=1")
        }
        #expect(try await database.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM ocr_text") } == 0)
    }
}
