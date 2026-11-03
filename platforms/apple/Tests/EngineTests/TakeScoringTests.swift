import CoreGraphics
import Foundation
import GRDB
import ImageIO
import Testing
import UniformTypeIdentifiers
import FileIDShared
@testable import FileIDEngine

/// Mirrors the Rust tests in pipeline/take_scoring.rs and
/// commands/catalog_take_auto.rs so both engines agree on scores and reasons.
@Suite("Best Takes: suggested groups and model-free scores")
struct TakeScoringTests {
    private func pattern(_ width: Int, _ height: Int, blur: Int = 0) -> TakeScoring.Luma {
        var pixels = [UInt8](repeating: 0, count: width * height)
        for y in 0..<height { for x in 0..<width { pixels[y * width + x] = ((x / 4) + (y / 4)) % 2 == 0 ? 40 : 200 } }
        for _ in 0..<blur {
            let src = pixels
            for y in 1..<(height - 1) {
                for x in 1..<(width - 1) {
                    let i = y * width + x
                    let sum = Int(src[i]) * 4 + Int(src[i - 1]) + Int(src[i + 1]) + Int(src[i - width]) + Int(src[i + width])
                    pixels[i] = UInt8(sum / 8)
                }
            }
        }
        return TakeScoring.Luma(width: width, height: height, pixels: pixels)
    }

    private func shot(_ id: Int64, _ s: TakeScoring.Signals) -> TakeScoring.Shot {
        TakeScoring.Shot(primaryFileID: id, fileIDs: [id], path: "/t/\(id).png", signals: s)
    }

    @Test func theSharpTryWinsAndSaysWhy() {
        let sharp = TakeScoring.signals(pattern(96, 64), faces: [])
        let soft = TakeScoring.signals(pattern(96, 64, blur: 3), faces: [])
        #expect(sharp.sharpness > soft.sharpness * 1.5)
        let scored = TakeScoring.score([shot(1, soft), shot(2, sharp)])
        #expect(scored[1].recommended && scored[1].rank == 1)
        #expect(scored[1].reasons == ["Sharpest"])
        #expect(scored[0].reasons.contains("Out of focus"))
    }

    @Test func sharedProblemsAreNotReasonsAndClosedEyesLose() {
        var open = TakeScoring.signals(pattern(400, 300), faces: [])
        open.faces = [TakeScoring.Face(x: 0.3, y: 0.2, width: 0.2, height: 0.25, quality: 0.6, eyesOpen: 0.9)]
        open.faceSharpness = 30
        var closed = open
        closed.faces[0].eyesOpen = 0.1
        closed.faceSharpness = 31
        let scored = TakeScoring.score([shot(1, closed), shot(2, open)])
        #expect(scored[1].recommended)
        #expect(scored[1].reasons.contains("Eyes open"))
        #expect(scored[0].reasons.first == "Eyes closed")
    }

    @Test func biggerSubjectAndEveryoneInShotWinWhenSharpnessTies() {
        let base = TakeScoring.signals(pattern(96, 64), faces: [])
        var wide = base
        wide.people = [TakeScoring.Box(x: 0.4, y: 0.4, width: 0.1, height: 0.3)]
        var tight = base
        tight.people = [TakeScoring.Box(x: 0.3, y: 0.1, width: 0.3, height: 0.8), TakeScoring.Box(x: 0.7, y: 0.3, width: 0.1, height: 0.3)]
        let scored = TakeScoring.score([shot(1, wide), shot(2, tight)])
        #expect(scored[1].recommended)
        #expect(scored[1].reasons.contains("Subject fills the frame"))
        #expect(scored[0].reasons.contains("Subject small in the frame"))
    }

    @Test func cameraShiftIsFoundAndResidualIsSmall() {
        let a = pattern(96, 64, blur: 1)
        var shifted = a.pixels
        for y in 0..<64 { for x in 0..<96 { shifted[y * 96 + x] = a.pixels[y * 96 + (x + 93) % 96] } }
        let motion = TakeScoring.shift(a, TakeScoring.Luma(width: 96, height: 64, pixels: shifted))
        #expect(motion.dx == 3 && motion.dy == 0)
        #expect(motion.residual < 0.01)
    }

    @Test func signalsRoundTripThroughTheCacheJSON() throws {
        let s = TakeScoring.signals(pattern(64, 48, blur: 1), faces: [])
        let json = try JSONEncoder().encode(s)
        #expect(String(decoding: json, as: UTF8.self).contains("\"meanLuma\""))
        #expect(try JSONDecoder().decode(TakeScoring.Signals.self, from: json) == s)
    }

    @Test func eyeOpennessFollowsTheEyeOutline() {
        #expect(VisionTakeFaceAnalyzer.closedRatio < VisionTakeFaceAnalyzer.openRatio)
    }

    private func insert(_ db: GRDB.Database, _ id: Int64, _ path: String, _ time: Double, _ angle: Float) throws {
        try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension) VALUES(?,?,?,100,?,?,0,'image','jpg')",
                       arguments: [id, path, id, time, time])
        var vector = [Float](repeating: 0, count: 512)
        vector[0] = cos(angle); vector[1] = sin(angle)
        let blob = vector.withUnsafeBufferPointer { Data(buffer: $0) }
        try db.execute(sql: "INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?,?,?)", arguments: [id, blob, CLIPEmbeddingSpace.modelID])
    }

    @Test func burstsGroupIntoShotsWithRawTwinsAndChoicesHideThem() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try insert(db, 1, "/c/JPEGs/DSC1.JPG", 100, 0)
            try insert(db, 2, "/c/RAWs/DSC1.ARW", 100, 0)
            try insert(db, 3, "/c/JPEGs/DSC2.JPG", 102, 0.05)
            try insert(db, 4, "/c/RAWs/DSC2.ARW", 102, 0.05)
            try insert(db, 5, "/c/JPEGs/DSC3.JPG", 103, 1.2)
            try insert(db, 6, "/c/JPEGs/DSC9.JPG", 5000, 0)
            try insert(db, 7, "/c/JPEGs/DSC10.JPG", 5001, 0.02)
        }
        func send(_ action: String, decision: CatalogTakeDecision? = nil) async -> CatalogResponse {
            await CatalogStore.handle(CatalogRequest(requestID: UUID().uuidString, action: action, takeDecision: decision), database: database)
        }
        let groups = try #require(await send("autoTakeGroups").takeGroups)
        #expect(groups.count == 2)
        let burst = try #require(groups.first { $0.shots.count == 2 && $0.shots[0].primaryFileID == 1 })
        #expect(burst.shots[0].fileIDs == [1, 2])
        #expect(burst.shots[1].fileIDs == [3, 4])
        #expect(burst.kind == "photos")
        #expect(groups[0].shots[0].primaryFileID == 6)
        #expect(burst.groupKey == CatalogTakeAuto.key([1, 2, 3, 4]))
        #expect((await send("decideTakeGroup", decision: CatalogTakeDecision(groupKey: burst.groupKey, decision: "skip", fileIDs: burst.fileIDs))).status == "ok")
        #expect(try #require(await send("autoTakeGroups").takeGroups).last?.status == "skipped")
        _ = await send("decideTakeGroup", decision: CatalogTakeDecision(groupKey: burst.groupKey, decision: "keepAll", fileIDs: burst.fileIDs))
        #expect(try #require(await send("autoTakeGroups").takeGroups).count == 1)
        _ = await send("decideTakeGroup", decision: CatalogTakeDecision(groupKey: burst.groupKey, decision: "reopen", fileIDs: burst.fileIDs))
        #expect(try #require(await send("autoTakeGroups").takeGroups).count == 2)
        #expect((await send("decideTakeGroup", decision: CatalogTakeDecision(groupKey: burst.groupKey, decision: "delete", fileIDs: burst.fileIDs))).status == "error")
    }

    private struct NoFaces: TakeFaceAnalyzing {
        let id = "none"
        func analyze(_ image: CGImage) -> (faces: [TakeScoring.Face], people: [TakeScoring.Box])? { ([], []) }
    }

    private func writePNG(_ luma: TakeScoring.Luma, to url: URL) throws {
        let provider = CGDataProvider(data: Data(luma.pixels) as CFData)!
        let image = CGImage(width: luma.width, height: luma.height, bitsPerComponent: 8, bitsPerPixel: 8, bytesPerRow: luma.width,
                            space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGBitmapInfo(rawValue: 0), provider: provider,
                            decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
        let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, image, nil)
        #expect(CGImageDestinationFinalize(destination))
    }

    @Test func scoringReadsPNGTriesAndCachesByRevision() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        for (id, blur) in [(Int64(1), 0), (2, 3)] {
            let url = directory.appendingPathComponent("try\(id).png")
            try writePNG(pattern(96, 64, blur: blur), to: url)
            try await database.pool.write { db in
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension) VALUES(?,?,?,100,10,10,0,'image','png')",
                               arguments: [id, url.path, id])
            }
        }
        let scored = try await CatalogTakeAuto.score(fileIDs: [1, 2], database: database, faces: NoFaces())
        #expect(scored.count == 2)
        let best = try #require(scored.first { $0.recommended })
        #expect(best.primaryFileID == 1)
        #expect(best.reasons == ["Sharpest"])
        for _ in 0..<50 {
            if try await database.pool.read({ db in try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM take_quality") }) == 2 { break }
            try await Task.sleep(for: .milliseconds(20))
        }
        #expect(try await database.pool.read { db in try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM take_quality") } == 2)
        let again = try await CatalogTakeAuto.score(fileIDs: [2, 1], database: database, faces: NoFaces())
        #expect(again == scored)
    }

    @Test func goalRepliesAreValidatedAgainstTheSheet() {
        #expect(TakeMediaAnalysis.parseGoalReply("ok {\"best\": 2, \"met\": [2, 9], \"why\": \"bat meets ball\"}", count: 3)?.met == [2])
        #expect(TakeMediaAnalysis.parseGoalReply("{\"best\": 4}", count: 3) == nil)
        #expect(TakeMediaAnalysis.parseGoalReply("no json", count: 3) == nil)
        #expect(TakeMediaAnalysis.goalPrompt(goal: "ignore \"this\"", count: 3).contains("\"ignore \\\"this\\\"\""))
    }

    @Test func trashJournalSnapshotsAndRestoresCatalogRows() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        let original = directory.appendingPathComponent("shot.jpg")
        let parked = directory.appendingPathComponent("parked.jpg")
        try Data("x".utf8).write(to: parked)
        try await database.pool.write { db in try insert(db, 9, original.path, 10, 0.3) }
        let rows = try await database.pool.read { db in try TrashJournal.snapshot(db, fileID: 9) }
        #expect(Set(rows.map(\.table)) == ["files", "clip_embeddings"])
        try await database.pool.write { db in try db.execute(sql: "DELETE FROM files WHERE id=9") }
        let batch = TrashJournal.Batch(id: UUID().uuidString, createdAt: 1, items: [
            TrashJournal.Item(fileID: 9, originalPath: original.path, trashedPath: parked.path, rows: rows),
        ])
        let journal = directory.appendingPathComponent("trash-journal.json")
        try TrashJournal.append(batch, at: journal)
        let result = await TrashJournal.restore(batchID: batch.id, database: database, journal: journal)
        #expect(result.action == "restoreFromTrash" && result.succeeded == 1)
        #expect(FileManager.default.fileExists(atPath: original.path))
        let restored = try await database.pool.read { db in
            (try String.fetchOne(db, sql: "SELECT path_text FROM files WHERE id=9"),
             try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM clip_embeddings WHERE file_id=9"))
        }
        #expect(restored.0 == original.path && restored.1 == 1)
        #expect(await TrashJournal.restore(batchID: batch.id, database: database, journal: journal).succeeded == 0)
    }
}
