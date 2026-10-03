import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Timeline speech transcription")
struct TimelineSpeechTranscriptionTests {
    @Test func splitsLongRecordingsIntoBoundedChunksWithOverlap() {
        let chunks = TimelineSpeechTranscription.chunks(duration: 101)

        #expect(chunks.count == 3)
        #expect(chunks[0].extractStart == 0)
        #expect(chunks[0].extractEnd == 46.5)
        #expect(chunks[1].extractStart == 43.5)
        #expect(chunks[1].extractEnd == 91.5)
        #expect(chunks[2].extractStart == 88.5)
        #expect(chunks[2].extractEnd == 101)
        #expect(zip(chunks, chunks.dropFirst()).allSatisfy { $0.0.ownedEnd == $0.1.ownedStart })
    }

    @Test func groupsTimedWordsIntoSearchablePassages() {
        let chunk = TimelineSpeechTranscription.Chunk(
            extractStart: 0,
            extractEnd: 45,
            ownedStart: 0,
            ownedEnd: 45
        )
        let segments = [
            TimelineSpeechTranscription.Segment(text: "My", timestamp: 1, duration: 0.2, confidence: 0.8),
            TimelineSpeechTranscription.Segment(text: "kid", timestamp: 1.3, duration: 0.3, confidence: 0.9),
            TimelineSpeechTranscription.Segment(text: "hits", timestamp: 1.8, duration: 0.2, confidence: 0.95),
            TimelineSpeechTranscription.Segment(text: "!", timestamp: 2, duration: 0.1, confidence: 0.9)
        ]

        let passages = TimelineSpeechTranscription.passages(from: segments, chunk: chunk, mediaDuration: 45)

        #expect(passages.count == 1)
        #expect(passages[0].startSeconds == 1)
        #expect(passages[0].endSeconds == 2.1)
        #expect(passages[0].text == "My kid hits!")
        #expect(abs(passages[0].confidence - 0.8875) < 0.000001)
    }

    @Test func overlapOnlyKeepsSpeechInItsOwningChunkAndClampsAtVideoEnd() {
        let chunk = TimelineSpeechTranscription.Chunk(
            extractStart: 43.5,
            extractEnd: 60,
            ownedStart: 45,
            ownedEnd: 60
        )
        let segments = [
            TimelineSpeechTranscription.Segment(text: "overlap duplicate", timestamp: 0.2, duration: 0.4, confidence: 0.8),
            TimelineSpeechTranscription.Segment(text: "boundary word", timestamp: 1.2, duration: 0.8, confidence: 0.9),
            TimelineSpeechTranscription.Segment(text: "last word", timestamp: 15.8, duration: 1, confidence: 0.7)
        ]

        let passages = TimelineSpeechTranscription.passages(from: segments, chunk: chunk, mediaDuration: 60)

        #expect(passages.map(\.text) == ["boundary word", "last word"])
        #expect(passages[0].startSeconds == 44.7)
        #expect(passages[1].endSeconds == 60)
    }

    @Test func rejectsInvalidChunkAndInvalidTimingInputs() {
        #expect(TimelineSpeechTranscription.chunks(duration: .infinity).isEmpty)
        #expect(TimelineSpeechTranscription.chunks(duration: 10, maximumCoreDuration: 0).isEmpty)

        let chunk = TimelineSpeechTranscription.Chunk(extractStart: 0, extractEnd: 10, ownedStart: 0, ownedEnd: 10)
        let segments = [
            TimelineSpeechTranscription.Segment(text: " ", timestamp: 1, duration: 0.1, confidence: 1),
            TimelineSpeechTranscription.Segment(text: "bad time", timestamp: .nan, duration: 0.1, confidence: 1),
            TimelineSpeechTranscription.Segment(text: "bad confidence", timestamp: 2, duration: 0.1, confidence: .infinity)
        ]
        #expect(TimelineSpeechTranscription.passages(from: segments, chunk: chunk, mediaDuration: 10).isEmpty)
    }

    @Test func sweepRemovesOnlyOldUuidNamedAudioTemps() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        let stale = directory.appendingPathComponent("fileid-speech-\(UUID().uuidString).m4a")
        let recent = directory.appendingPathComponent("fileid-speech-\(UUID().uuidString).m4a")
        let unrelated = directory.appendingPathComponent("family-recording.m4a")
        for url in [stale, recent, unrelated] { try Data([1, 2, 3]).write(to: url) }
        try FileManager.default.setAttributes([.modificationDate: Date(timeIntervalSince1970: 400)], ofItemAtPath: stale.path)
        try FileManager.default.setAttributes([.modificationDate: Date(timeIntervalSince1970: 800)], ofItemAtPath: recent.path)

        TimelineSpeechTranscription.sweepAbandonedAudio(directory: directory, now: Date(timeIntervalSince1970: 1_000))

        #expect(!FileManager.default.fileExists(atPath: stale.path))
        #expect(FileManager.default.fileExists(atPath: recent.path))
        #expect(FileManager.default.fileExists(atPath: unrelated.path))
    }

    @Test func persistsTimestampedPassagesAndInvalidatesChangedSources() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(11,'/internal/party.mov',11,100,0,'video','mov')")
        }
        let sourceRevision = try await database.pool.read { db in try CatalogStore.revision(db, fileID: 11) }
        let chunk = TimelineSpeechTranscription.Chunk(extractStart: 0, extractEnd: 45, ownedStart: 0, ownedEnd: 45)
        let passage = TimelineSpeechTranscription.Passage(
            startSeconds: 12.25,
            endSeconds: 14.75,
            text: "The batter hit a line drive.",
            confidence: 0.91
        )

        try await TimelineAnalysis.shared.persistSpeechChunk(
            fileID: 11,
            sourceRevision: sourceRevision,
            modelVersion: "apple-speech-test",
            chunkID: "speech-test-chunk",
            chunk: chunk,
            passages: [passage],
            database: database
        )

        let stored = try await database.pool.read { db in
            let row = try Row.fetchOne(db, sql: "SELECT start_seconds,end_seconds,text,confidence FROM catalog_passages WHERE file_id=11")
            return (
                row?["start_seconds"] as Double?,
                row?["end_seconds"] as Double?,
                row?["text"] as String?,
                row?["confidence"] as Double?,
                try String.fetchOne(db, sql: "SELECT status FROM catalog_coverage WHERE id='speech-test-chunk'"),
                try Int.fetchOne(db, sql: "SELECT count(*) FROM catalog_evidence_fts WHERE catalog_evidence_fts MATCH 'batter'")
            )
        }
        #expect(stored.0 == 12.25)
        #expect(stored.1 == 14.75)
        #expect(stored.2 == "The batter hit a line drive.")
        #expect(stored.3 == 0.91)
        #expect(stored.4 == "verified")
        #expect(stored.5 == 1)

        try await database.pool.write { db in
            try db.execute(sql: "UPDATE files SET size_bytes=101 WHERE id=11")
        }
        let invalidated = try await database.pool.read { db in
            (
                try Int.fetchOne(db, sql: "SELECT stale FROM catalog_passages WHERE file_id=11"),
                try String.fetchOne(db, sql: "SELECT status FROM catalog_coverage WHERE id='speech-test-chunk'")
            )
        }
        #expect(invalidated.0 == 1)
        #expect(invalidated.1 == "stale")
        try database.pool.close()
    }
}
