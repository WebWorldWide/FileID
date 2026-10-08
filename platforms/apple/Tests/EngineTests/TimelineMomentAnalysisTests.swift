import Foundation
import Testing
import GRDB
import FileIDShared
@testable import FileIDEngine

@Suite("Frame-sequence moment evidence")
struct TimelineMomentAnalysisTests {
    @Test func quietFootageRetainsOverlappingFallbackWindows() {
        let windows = TimelineMomentAnalysis.windows(duration: 8, signals: [])
        #expect(windows.map(\.start) == [0, 3, 6])
        #expect(windows.map(\.end) == [4, 7, 8])
        #expect(windows.allSatisfy { $0.times.count == 8 && $0.times.first == $0.start && $0.times.last! < $0.end })
        #expect(zip(windows, windows.dropFirst()).allSatisfy { $0.end > $1.start })
    }

    @Test func motionPeakDoesNotRemoveQuietCoverage() {
        let windows = TimelineMomentAnalysis.windows(duration: 10, signals: [
            .init(seconds: 1.23, changeScore: 0.8), .init(seconds: 6.11, changeScore: 0.9),
            .init(seconds: .nan, changeScore: 1), .init(seconds: 8, changeScore: .infinity)
        ])
        #expect(windows.map(\.start) == [0, 3, 6])
        #expect(windows[0].times.contains(1.23))
        #expect(windows[1].times.contains(6.11))
        #expect(windows.allSatisfy { $0.times == $0.times.sorted() })
    }

    @Test func invalidOrUnboundedDurationsAreRejected() {
        for duration in [0.0, -1, .nan, .infinity, 21_601] {
            #expect(TimelineMomentAnalysis.windows(duration: duration, signals: []).isEmpty)
        }
        #expect(TimelineMomentAnalysis.windows(duration: 0.2, signals: []).count == 1)
    }

    @Test func parsesEvidenceAndAbstentionWithoutInventingConfidence() throws {
        let raw = "```json\n{\"moments\":[{\"title\":\"Gift opening\",\"summary\":\"Wrapping is removed\",\"firstFrame\":2,\"lastFrame\":5}]}\n```"
        let moments = try TimelineMomentAnalysis.parse(raw, frameCount: 8)
        let window = try #require(TimelineMomentAnalysis.windows(duration: 4, signals: []).first)
        let chapters = TimelineMomentAnalysis.chapters(moments: moments, times: window.times, window: window,
                                                       fileID: 1, sourceRevision: "fixture", modelVersion: "sequence-test")
        #expect(chapters.count == 1)
        #expect(chapters[0].startSeconds == 1)
        #expect(chapters[0].endSeconds == 3)
        #expect(chapters[0].confidence == 0.2)
        #expect(!chapters[0].userEdited)
        #expect(try TimelineMomentAnalysis.parse("{\"moments\":[]}", frameCount: 8).isEmpty)
    }

    @Test func rejectsUnboundedOrMalformedModelEvidence() {
        for raw in ["No events happened.", "{\"moments\":[{\"title\":\"Catch\",\"summary\":\"\",\"firstFrame\":0,\"lastFrame\":8}]}",
                    "{\"moments\":[{\"title\":\"Catch\",\"summary\":\"\",\"firstFrame\":5,\"lastFrame\":2}]}",
                    "{\"moments\":[{\"title\":\" \",\"summary\":\"\",\"firstFrame\":0,\"lastFrame\":1}]}",
                    String(repeating: "x", count: 16_385)] {
            #expect(throws: TimelineMomentAnalysis.InvalidResponse.self) {
                try TimelineMomentAnalysis.parse(raw, frameCount: 8)
            }
        }
    }

    @Test func ignoresInvalidDecodedTimestamps() throws {
        let window = try #require(TimelineMomentAnalysis.windows(duration: 4, signals: []).first)
        let moment = TimelineMomentAnalysis.Moment(title: "Swing", summary: "Bat moves", firstFrame: 0, lastFrame: 2)
        #expect(TimelineMomentAnalysis.chapters(moments: [moment], times: [.nan], window: window,
                                                fileID: 1, sourceRevision: "r", modelVersion: "m").isEmpty)
        #expect(TimelineMomentAnalysis.chapters(moments: [moment], times: window.times.reversed(), window: window,
                                                fileID: 1, sourceRevision: "r", modelVersion: "m").isEmpty)
    }

    @Test func deduplicatesLowFrameRateSamplesWithoutInventingExtraEvidence() throws {
        let window = try #require(TimelineMomentAnalysis.windows(duration: 2, signals: []).first)
        let decoded = [0.0, 0, 0.5, 0.5, 1, 1, 1.5, 1.5]
        let indices = try TimelineMomentAnalysis.distinctFrameIndices(times: decoded, window: window)
        #expect(indices == [0, 2, 4, 6])
        let times = indices.map { decoded[$0] }
        let moment = TimelineMomentAnalysis.Moment(title: "Ball movement", summary: "Ball rolls", firstFrame: 1, lastFrame: 3)
        let chapter = try #require(TimelineMomentAnalysis.chapters(
            moments: [moment], times: times, window: window, fileID: 1,
            sourceRevision: "fixture", modelVersion: "sequence-test"
        ).first)
        #expect(chapter.startSeconds == 0.5)
        #expect(chapter.endSeconds == 2)
        for invalid in [[0.0, 0], [1.0, 0], [0.0, .nan], [0.0, 3]] {
            #expect(throws: TimelineMomentAnalysis.InvalidResponse.self) {
                try TimelineMomentAnalysis.distinctFrameIndices(times: invalid, window: window)
            }
        }
    }

    @Test func transactionalWindowPersistenceKeepsUserEditsAndSampledCoverage() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(11,'/internal/party.mov',11,100,0,'video','mov')")
            try db.execute(sql: "INSERT INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,created_at,updated_at) VALUES('job','timelineSample','[11]','{}','running',0,0)")
        }
        let revision = try await database.pool.read { try CatalogStore.revision($0, fileID: 11) }
        let window = try #require(TimelineMomentAnalysis.windows(duration: 4, signals: []).first)
        let moments = [TimelineMomentAnalysis.Moment(title: "Gift opening", summary: "Wrapping removed", firstFrame: 1, lastFrame: 6)]
        let chapters = TimelineMomentAnalysis.chapters(moments: moments, times: window.times, window: window,
                                                       fileID: 11, sourceRevision: revision, modelVersion: "sequence-test")
        try await TimelineAnalysis.persistMomentWindow(chapters: chapters, coverageID: "coverage", window: window,
                                                       fileID: 11, sourceRevision: revision, modelVersion: "sequence-test",
                                                       jobID: "job", database: database)
        try await database.pool.write { db in
            try db.execute(sql: "UPDATE catalog_chapters SET title='My corrected moment',user_edited=1 WHERE id=?", arguments: [chapters[0].id])
        }
        try await TimelineAnalysis.persistMomentWindow(chapters: chapters, coverageID: "coverage", window: window,
                                                       fileID: 11, sourceRevision: revision, modelVersion: "sequence-test",
                                                       jobID: "job", database: database)
        let saved = try await database.pool.read { db in
            (try String.fetchOne(db, sql: "SELECT title FROM catalog_chapters WHERE id=?", arguments: [chapters[0].id]),
             try String.fetchOne(db, sql: "SELECT status FROM catalog_coverage WHERE id='coverage'"))
        }
        #expect(saved.0 == "My corrected moment")
        #expect(saved.1 == "sampled")
        let extra = TimelineMomentAnalysis.chapters(moments: moments + moments, times: window.times, window: window,
                                                   fileID: 11, sourceRevision: revision, modelVersion: "sequence-test")
        try await TimelineAnalysis.persistMomentWindow(chapters: extra, coverageID: "coverage", window: window,
                                                       fileID: 11, sourceRevision: revision, modelVersion: "sequence-test",
                                                       jobID: "job", database: database)
        try await TimelineAnalysis.persistMomentWindow(chapters: [], coverageID: "coverage", window: window,
                                                       fileID: 11, sourceRevision: revision, modelVersion: "sequence-test",
                                                       jobID: "job", database: database)
        let remaining = try await database.pool.read { db in
            try String.fetchAll(db, sql: "SELECT title FROM catalog_chapters WHERE file_id=11")
        }
        #expect(remaining == ["My corrected moment"])
        try await database.pool.write { db in try db.execute(sql: "UPDATE catalog_jobs SET state='paused' WHERE id='job'") }
        await #expect(throws: (any Error).self) {
            try await TimelineAnalysis.persistMomentWindow(chapters: [], coverageID: "paused", window: window,
                                                           fileID: 11, sourceRevision: revision, modelVersion: "sequence-test",
                                                           jobID: "job", database: database)
        }
        try await database.pool.write { db in
            try db.execute(sql: "UPDATE catalog_jobs SET state='running' WHERE id='job'")
            try db.execute(sql: "UPDATE files SET size_bytes=200 WHERE id=11")
        }
        await #expect(throws: (any Error).self) {
            try await TimelineAnalysis.persistMomentWindow(chapters: chapters, coverageID: "stale", window: window,
                                                           fileID: 11, sourceRevision: revision, modelVersion: "sequence-test",
                                                           jobID: "job", database: database)
        }
        let refused = try await database.pool.read { db in
            try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM catalog_coverage WHERE id IN ('paused','stale')")
        }
        #expect(refused == 0)
    }
}
