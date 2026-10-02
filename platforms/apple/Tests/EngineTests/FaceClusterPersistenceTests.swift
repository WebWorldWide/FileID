import Foundation
import GRDB
import Testing
@testable import FileIDEngine

@Suite("Stable person persistence")
struct FaceClusterPersistenceTests {
    private var vector: Data { ArcFaceService.embeddingToBlob([1] + [Float](repeating: 0, count: 127)) }

    private func fixture() async throws -> (FileIDEngine.Database, URL) {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-stable-people-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let database = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        let blob = vector
        try await database.pool.write { db in
            try db.execute(sql: """
                INSERT INTO persons(id,name,first_name,created_at,is_unknown) VALUES(7,'Confirmed','Confirmed',123,0),(8,NULL,NULL,123,1),(9,'Offline','Offline',123,0)
                """)
            for id in 1...4 {
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?,?,?,1,0,'image','jpg')", arguments: [id,root.appendingPathComponent("\(id).jpg").path,id])
                try db.execute(sql: "INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?,'rev','fixture',0)", arguments: [id])
                let person: Int64? = id < 3 ? 7 : (id == 3 ? 8 : nil)
                try db.execute(sql: """
                    INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,embedding_model,processing_version,source_revision)
                    VALUES(?,?,?,X'','0,0,1,1',?,'fixture-sface','fixture-align','rev')
                    """, arguments: [id,id,person,blob])
            }
            try db.execute(sql: "UPDATE persons SET representative_face_id=2 WHERE id=7")
            try db.execute(sql: """
                INSERT INTO catalog_observations(id,file_id,person_id,source_revision,model_version,confidence,user_edited)
                VALUES('manual-person',1,7,'rev','manual',1,1)
                """)
        }
        return (database,root)
    }

    @Test func repeatedClusteringRetainsIdentitiesAndReferences() async throws {
        let (database,root) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        let blob = vector
        for time in [456.0,457.0] {
            let count = try await database.pool.write { db in
                try FaceClusterPersistence.apply(db, clusters: [.init(personID: 7, faceIDs: [1,2], representative: 1, centroid: blob, radius: 0.8)], pool: [1,2,3], preserved: [8], now: time)
            }
            #expect(count == 3)
        }
        try await database.pool.read { db in
            let person = try Row.fetchOne(db, sql: "SELECT name,created_at,file_count FROM persons WHERE id=7")!
            let ignoredOwner = try Int64.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id=3")
            let manualOwner = try Int64.fetchOne(db, sql: "SELECT person_id FROM catalog_observations WHERE id='manual-person'")
            let offline = try String.fetchOne(db, sql: "SELECT name FROM persons WHERE id=9")
            #expect(person["name"] as String == "Confirmed")
            #expect(person["created_at"] as Double == 123)
            #expect(person["file_count"] as Int == 2)
            #expect(ignoredOwner == 8 && manualOwner == 7 && offline == "Offline")
        }
    }

    @Test func newClusterDoesNotClearOtherPeople() async throws {
        let (database,root) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        let blob = vector
        let count = try await database.pool.write { db in
            try FaceClusterPersistence.apply(db, clusters: [.init(personID: nil, faceIDs: [4], representative: 4, centroid: blob, radius: 0.8)], pool: [4], preserved: [8], now: 456)
        }
        #expect(count == 4)
        try await database.pool.read { db in
            let original = try Int64.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id=1")
            let created = try Int64.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id=4")
            #expect(original == 7 && created != nil && created! > 9)
        }
    }

    @Test func failedPublicationRollsBackAssignmentsAndAnalysis() async throws {
        let (database,root) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        try await database.pool.write { db in
            try db.execute(sql: "CREATE TRIGGER fixture_reject_person BEFORE INSERT ON persons BEGIN SELECT RAISE(ABORT,'fixture failure'); END")
        }
        let blob = vector
        await #expect(throws: (any Error).self) {
            try await database.pool.write { db in
                try FaceClusterPersistence.apply(db, clusters: [
                    .init(personID: 7, faceIDs: [1], representative: 1, centroid: blob, radius: 0.8),
                    .init(personID: nil, faceIDs: [2], representative: 2, centroid: blob, radius: 0.8)
                ], pool: [1,2], preserved: [8], now: 999)
            }
        }
        try await database.pool.read { db in
            let owners = try Int64.fetchAll(db, sql: "SELECT person_id FROM face_prints WHERE id IN (1,2) ORDER BY id")
            let representative = try Int64.fetchOne(db, sql: "SELECT representative_face_id FROM persons WHERE id=7")
            let count = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM persons")
            #expect(owners == [7,7] && representative == 2 && count == 3)
        }
    }

    @Test func personLimitPreservesExistingPeople() async throws {
        let (database,root) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        let blob = vector
        await #expect(throws: FaceClusterPersistence.Failure.self) {
            try await database.pool.write { db in
                try FaceClusterPersistence.apply(db, clusters: [.init(personID: nil, faceIDs: [4], representative: 4, centroid: blob, radius: 0.8)], pool: [4], preserved: [8], now: 456, maximumPersons: 3)
            }
        }
        try await database.pool.read { db in
            let count = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM persons")
            let owner = try Int64.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id=4")
            #expect(count == 3 && owner == nil)
        }
    }
}
