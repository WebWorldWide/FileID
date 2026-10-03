import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Timeline chapter suggestions")
struct TimelineChapterSuggestionsTests {
    @Test func proposesOnlyAConfirmedCaptionTransitionAndMarksItAsADraft() {
        let captions = [
            TimelineChapterSuggestions.Caption(seconds: 0, text: "A batter waits near home plate beside the catcher and umpire."),
            TimelineChapterSuggestions.Caption(seconds: 10, text: "The batter adjusts stance near home plate beside the catcher and umpire."),
            TimelineChapterSuggestions.Caption(seconds: 20, text: "A family gathers around a birthday cake at the dinner table with candles."),
            TimelineChapterSuggestions.Caption(seconds: 30, text: "The family smiles as candles glow on the birthday cake at the dinner table."),
            TimelineChapterSuggestions.Caption(seconds: 40, text: "The family cuts the birthday cake while candles sit on the dinner table.")
        ]

        let chapters = TimelineChapterSuggestions.propose(
            fileID: 7,
            sourceRevision: "100:123456",
            frameModelVersion: "timeline-frame-v1/test-model",
            duration: 50,
            captions: captions
        )

        #expect(chapters.count == 2)
        #expect(chapters.map(\.startSeconds) == [0, 20])
        #expect(chapters.first?.endSeconds == 19.999)
        #expect(chapters.last?.endSeconds == 50)
        #expect(chapters.allSatisfy { !$0.userEdited && !$0.stale && $0.confidence == 0.2 })
        #expect(chapters.allSatisfy { $0.modelVersion.hasPrefix("timeline-chapter-suggestion-v1/") })
        #expect(chapters[0].title.hasPrefix("A batter waits"))
        #expect(chapters[1].title.hasPrefix("A family gathers"))
    }

    @Test func abstainsWhenSamplesDoNotConfirmAChange() {
        let captions = [
            TimelineChapterSuggestions.Caption(seconds: 0, text: "A golden retriever rests on the living room rug beside a sofa."),
            TimelineChapterSuggestions.Caption(seconds: 10, text: "The dog rests on the rug near the living room sofa."),
            TimelineChapterSuggestions.Caption(seconds: 20, text: "A retriever lies on the rug in the living room beside the sofa.")
        ]

        #expect(TimelineChapterSuggestions.propose(
            fileID: 1,
            sourceRevision: "1:2",
            frameModelVersion: "timeline-frame-v1/test-model",
            duration: 30,
            captions: captions
        ).isEmpty)
    }

    @Test func abstainsWhenThereAreTooFewSamples() {
        #expect(TimelineChapterSuggestions.propose(
            fileID: 1,
            sourceRevision: "1:2",
            frameModelVersion: "timeline-frame-v1/test-model",
            duration: 30,
            captions: [.init(seconds: 0, text: "A child riding a bicycle.")]
        ).isEmpty)
    }

    @Test func persistsSuggestionsAndKeepsAcceptedEditsAcrossRegeneration() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(11,'/internal/party.mov',11,100,0,'video','mov')")
        }
        let sourceRevision = try await database.pool.read { db in try CatalogStore.revision(db, fileID: 11) }
        let modelVersion = "timeline-frame-v1/model@test-pin"
        let captions = [
            (0.0, "A batter waits near home plate beside the catcher and umpire."),
            (10.0, "The batter adjusts stance near home plate beside the catcher and umpire."),
            (20.0, "A family gathers around a birthday cake at the dinner table with candles."),
            (30.0, "The family smiles as candles glow on the birthday cake at the dinner table."),
            (40.0, "The family cuts the birthday cake while candles sit on the dinner table.")
        ]
        try await database.pool.write { db in
            for (index, caption) in captions.enumerated() {
                try db.execute(sql: "INSERT INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES(?,?,?, ?,?,?,?,0)", arguments: ["frame-\(index)", 11, caption.0, caption.0, caption.1, sourceRevision, modelVersion])
            }
        }

        try await TimelineAnalysis.shared.persistChapterSuggestions(
            fileID: 11,
            sourceRevision: sourceRevision,
            frameModelVersion: modelVersion,
            duration: 50,
            database: database
        )
        var chapters = try await database.pool.read { db in try CatalogStore.chapters(db, fileID: 11) }
        #expect(chapters.count == 2)
        let acceptedID = try #require(chapters.first(where: { $0.startSeconds == 20 })?.id)
        try await database.pool.write { db in
            try db.execute(sql: "UPDATE catalog_chapters SET title='User-approved chapter',model_version='user',user_edited=1,confidence=1 WHERE id=?", arguments: [acceptedID])
        }

        try await TimelineAnalysis.shared.persistChapterSuggestions(
            fileID: 11,
            sourceRevision: sourceRevision,
            frameModelVersion: modelVersion,
            duration: 50,
            database: database
        )
        chapters = try await database.pool.read { db in try CatalogStore.chapters(db, fileID: 11) }
        #expect(chapters.count == 2)
        #expect(chapters.first(where: { $0.id == acceptedID })?.title == "User-approved chapter")
        #expect(chapters.first(where: { $0.id == acceptedID })?.userEdited == true)
        try database.pool.close()
    }
}
