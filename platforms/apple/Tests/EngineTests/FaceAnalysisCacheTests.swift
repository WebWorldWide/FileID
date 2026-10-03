import Foundation
import GRDB
import Testing
@testable import FileIDEngine

@Suite("Versioned face cache")
struct FaceAnalysisCacheTests {
    private func database() throws -> DatabaseQueue {
        let queue = try DatabaseQueue()
        try FileIDEngine.Database.migrator.migrate(queue)
        try queue.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,'/internal/portrait.png',1,100,10,0,'image','png')")
            try db.execute(sql: "INSERT INTO persons(id,name,created_at) VALUES(7,'Confirmed Person',0)")
            try db.execute(sql: "INSERT INTO face_prints(id,file_id,print_data,bbox,person_id) VALUES(4,1,X'00','0.1,0.1,0.2,0.2',7)")
        }
        return queue
    }

    private var vector: Data {
        var values = [Float](repeating: 0, count: 128)
        values[0] = 1
        return values.withUnsafeBytes { Data($0) }
    }

    @Test func sourceIdentityRejectsAChangedInternalFixture() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let source = directory.appendingPathComponent("portrait.fixture")
        try Data([1,2,3]).write(to: source)
        let values = try source.resourceValues(forKeys: [.contentModificationDateKey])
        let input = FaceAnalysisCache.Input(id: 1, fileID: 1, bbox: "0,0,1,1", path: source.path, size: 3, modified: values.contentModificationDate?.timeIntervalSince1970, previousProcessingVersion: nil)
        #expect(input.currentIdentity() != nil)
        try Data([4,5,6,7]).write(to: source)
        #expect(input.currentIdentity() == nil)
    }

    @Test func refreshPreservesIdentityAndSeparatesModelEvidence() throws {
        let queue = try database()
        try queue.write { db in
            let input = try #require(FaceAnalysisCache.pending(db, modelVersion: "weights-a", limit: 10).first)
            #expect(try FaceAnalysisCache.persist(db, input: input, embedding: vector, modelVersion: "weights-a"))
            #expect(try FaceAnalysisCache.pending(db, modelVersion: "weights-a", limit: 10).isEmpty)
            #expect(try Int.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id=4") == 7)
            #expect(try String.fetchOne(db, sql: "SELECT name FROM persons WHERE id=7") == "Confirmed Person")
            #expect(try Int.fetchOne(db, sql: "SELECT person_id FROM catalog_observations WHERE id='faceprint:4'") == 7)
            #expect(try Double.fetchOne(db, sql: "SELECT confidence FROM catalog_observations WHERE id='faceprint:4'") == 0)
            let changed = try #require(FaceAnalysisCache.pending(db, modelVersion: "weights-b", limit: 10).first)
            #expect(try FaceAnalysisCache.persist(db, input: changed, embedding: vector, modelVersion: "weights-b"))
            #expect(try Int.fetchOne(db, sql: "SELECT COUNT(DISTINCT model) FROM catalog_embeddings") == 2)
            try db.execute(sql: "UPDATE face_prints SET person_id=NULL WHERE id=4")
            #expect(try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM catalog_observations WHERE person_id IS NULL") == 1)
        }
    }

    @Test func staleCompletionAndMalformedVectorCannotOverwriteCorrections() throws {
        let queue = try database()
        try queue.write { db in
            let input = try #require(FaceAnalysisCache.pending(db, modelVersion: "weights-a", limit: 10).first)
            #expect(try !FaceAnalysisCache.persist(db, input: input, embedding: Data(repeating: 0, count: 512), modelVersion: "weights-a"))
            #expect(try !FaceAnalysisCache.persist(db, input: input, embedding: Data(repeating: 0, count: 2048), modelVersion: "weights-a"))
            var invalid = vector
            invalid.replaceSubrange(0..<4, with: [0,0,128,127])
            #expect(try !FaceAnalysisCache.persist(db, input: input, embedding: invalid, modelVersion: "weights-a"))
            try db.execute(sql: "UPDATE face_prints SET excluded=1 WHERE id=4")
            #expect(try !FaceAnalysisCache.persist(db, input: input, embedding: vector, modelVersion: "weights-a"))
            try db.execute(sql: "UPDATE face_prints SET excluded=0,bbox='0.2,0.2,0.2,0.2' WHERE id=4")
            #expect(try !FaceAnalysisCache.persist(db, input: input, embedding: vector, modelVersion: "weights-a"))
            #expect(try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM catalog_revisions") == 0)
            try db.execute(sql: "UPDATE files SET size_bytes=101 WHERE id=1")
            #expect(try !FaceAnalysisCache.persist(db, input: input, embedding: vector, modelVersion: "weights-a"))
        }
    }

    @Test func exclusionDoesNotReviveChangedSourceAndManualObservationSurvivesDeletion() throws {
        let queue = try database()
        try queue.write { db in
            let input = try #require(FaceAnalysisCache.pending(db, modelVersion: "weights-a", limit: 10).first)
            #expect(try FaceAnalysisCache.persist(db, input: input, embedding: vector, modelVersion: "weights-a"))
            try db.execute(sql: "UPDATE face_prints SET excluded=1 WHERE id=4")
            #expect(try Int.fetchOne(db, sql: "SELECT stale FROM catalog_observations") == 1)
            #expect(try FaceAnalysisCache.pending(db, modelVersion: "weights-b", limit: 10).isEmpty)
            try db.execute(sql: "UPDATE files SET modified_at=11 WHERE id=1")
            try db.execute(sql: "UPDATE face_prints SET excluded=0 WHERE id=4")
            #expect(try Int.fetchOne(db, sql: "SELECT stale FROM catalog_observations") == 1)
            try db.execute(sql: "UPDATE catalog_observations SET user_edited=1,region_json='manual marker' WHERE id='faceprint:4'")
            let changed = try #require(FaceAnalysisCache.pending(db, modelVersion: "weights-b", limit: 10).first)
            #expect(try FaceAnalysisCache.persist(db, input: changed, embedding: vector, modelVersion: "weights-b"))
            #expect(try String.fetchOne(db, sql: "SELECT region_json FROM catalog_observations") == "manual marker")
            #expect(try Int.fetchOne(db, sql: "SELECT stale FROM catalog_observations") == 1)
            try db.execute(sql: "DELETE FROM face_prints WHERE id=4")
            #expect(try String.fetchOne(db, sql: "SELECT region_json FROM catalog_observations") == "manual marker")
            #expect(try Int.fetchOne(db, sql: "SELECT stale FROM catalog_observations") == 1)
            #expect(try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM catalog_embeddings") == 0)
        }
    }
}
