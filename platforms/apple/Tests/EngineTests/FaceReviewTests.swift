import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

private typealias Database = FileIDEngine.Database

@Suite("Face review queue", .serialized)
struct FaceReviewTests {
    private let model = "m"
    private let processing = "p"

    private func makeDB() throws -> Database {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("FaceReview-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return try Database(at: dir.appendingPathComponent("t.sqlite"))
    }

    private func vector(_ x: Float, _ y: Float, _ z: Float = 0) -> Data {
        var values = [Float](repeating: 0, count: 128)
        values[0] = x; values[1] = y; values[2] = z
        return FloatBlob.encode(values)
    }

    private func file(_ d: GRDB.Database, _ id: Int64) throws {
        try d.execute(sql: """
            INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(?,?,?,1,1,0,'image','jpg')
            """, arguments: [id, "/fixture/\(id).jpg", id])
        try d.execute(sql: "INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?,'r','p',0)", arguments: [id])
    }

    private func person(_ d: GRDB.Database, _ id: Int64, name: String? = nil, unknown: Bool = false) throws {
        try d.execute(sql: "INSERT INTO persons(id,name,is_unknown,created_at) VALUES(?,?,?,0)", arguments: [id, name, unknown ? 1 : 0])
    }

    private func face(_ d: GRDB.Database, _ id: Int64, file: Int64, person: Int64? = nil, _ embedding: Data,
                      bbox: String = "0,0,1,1", quality: Double = 0.9) throws {
        try d.execute(sql: """
            INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,face_quality,embedding_model,processing_version,source_revision)
            VALUES(?,?,?,X'00',?,?,?,'m','p','r')
            """, arguments: [id, file, person, bbox, embedding, quality])
    }

    private func seedPeople(_ db: Database) async throws {
        try await db.pool.write { d in
            for id: Int64 in 1...9 { try file(d, id) }
            try person(d, 1, name: "Ana")
            try person(d, 2, name: "Ben")
            try face(d, 11, file: 1, person: 1, vector(1, 0))
            try face(d, 12, file: 2, person: 2, vector(0, 1))
        }
    }

    private func assess(_ db: Database, _ ids: Set<Int64>, settings: FaceReview.Settings = .init()) async throws {
        try await db.pool.write { d in
            try FaceReview.assess(d, faceIDs: ids, model: model, processing: processing, settings: settings, now: 100)
        }
    }

    private func candidate(_ db: Database, _ face: Int64) async throws -> (person: Int64, reason: String, score: Double, runner: Int64?)? {
        try await db.pool.read { d in
            try Row.fetchOne(d, sql: "SELECT person_id,reason,score,runner_up_person_id FROM face_review_candidates WHERE face_id=?", arguments: [face])
                .map { ($0["person_id"], $0["reason"], $0["score"], $0["runner_up_person_id"]) }
        }
    }

    private func outcome(_ db: Database, _ face: Int64) async throws -> String? {
        try await db.pool.read { try String.fetchOne($0, sql: "SELECT outcome FROM face_assignment_assessed WHERE face_id=?", arguments: [face]) }
    }

    private func owner(_ db: Database, _ face: Int64) async throws -> Int64? {
        try await db.pool.read { try Int64.fetchOne($0, sql: "SELECT person_id FROM face_prints WHERE id=?", arguments: [face]) }
    }

    @Test func migrationBackfillsExistingFacesAsBaseline() async throws {
        let db = try makeDB()
        try await db.pool.write { d in
            try file(d, 1)
            try face(d, 5, file: 1, vector(1, 0))
            try d.execute(sql: "DELETE FROM face_assignment_assessed")
            try d.execute(sql: CatalogSchema.v32.components(separatedBy: "\n").last { $0.hasPrefix("INSERT") }!)
        }
        #expect(try await outcome(db, 5) == "baseline")
        let ns = try await db.pool.read { try String.fetchOne($0, sql: "SELECT namespace FROM face_assignment_assessed WHERE face_id=5") }
        #expect(ns == "m|p")
    }

    @Test func holdoutTakesNewFacesOnlyOnceSomeoneIsAssigned() async throws {
        let db = try makeDB()
        try await db.pool.write { d in
            for id: Int64 in 1...3 { try file(d, id) }
            try face(d, 21, file: 1, vector(1, 0))
            try face(d, 22, file: 2, vector(1, 0), quality: 0.1)
        }
        let first = try await db.pool.read { try FaceReview.holdout($0, model: model, processing: processing, minQuality: 0.5) }
        #expect(first.isEmpty)
        try await db.pool.write { d in
            try person(d, 1)
            try face(d, 23, file: 3, person: 1, vector(1, 0))
            try FaceReview.upsertOutcome(d, faceID: 22, namespace: "m|p", outcome: "suggested", now: 0)
        }
        let second = try await db.pool.read { try FaceReview.holdout($0, model: model, processing: processing, minQuality: 0) }
        #expect(second == [21, 22])
        let gated = try await db.pool.read { try FaceReview.holdout($0, model: model, processing: processing, minQuality: 0.5) }
        #expect(gated == [21])
        try await db.pool.write { try FaceReview.upsertOutcome($0, faceID: 21, namespace: "m|p", outcome: "unmatched", now: 0) }
        let third = try await db.pool.read { try FaceReview.holdout($0, model: model, processing: processing, minQuality: 0) }
        #expect(third == [22])
    }

    @Test func assessmentClassifiesFaces() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in
            try face(d, 31, file: 3, vector(0.9, 0.1))
            try face(d, 32, file: 4, vector(0.5, 0.0, 0.8660254))
            try face(d, 33, file: 5, vector(0.7, 0.7))
            try face(d, 34, file: 6, vector(0, 0, 1))
        }
        try await assess(db, [31, 32, 33, 34])
        let suggested = try #require(try await candidate(db, 31))
        #expect(suggested.person == 1 && suggested.reason == "suggested" && suggested.runner == 2)
        #expect(suggested.score == FaceReview.round4(Double(Float(0.9) / (Float(0.82).squareRoot()))))
        #expect(try await candidate(db, 32)?.reason == "uncertain")
        let ambiguous = try #require(try await candidate(db, 33))
        #expect(ambiguous.reason == "ambiguous" && ambiguous.person == 1 && ambiguous.runner == 2)
        #expect(try await candidate(db, 34) == nil)
        #expect(try await outcome(db, 34) == "unmatched")
        #expect(try await outcome(db, 31) == "suggested")
        #expect(try await owner(db, 31) == nil)
    }

    @Test func unknownBestStaysUnmatchedAndBarsCount() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in
            try person(d, 3, unknown: true)
            try face(d, 13, file: 7, person: 3, vector(0, 0, 1))
            try face(d, 41, file: 3, vector(0, 0.1, 1))
            try face(d, 42, file: 2, vector(0.6, 0.8), bbox: "0.5,0.5,0.1,0.1")
        }
        try await assess(db, [41, 42])
        #expect(try await candidate(db, 41) == nil)
        #expect(try await outcome(db, 41) == "unmatched")
        let samePhoto = try #require(try await candidate(db, 42))
        #expect(samePhoto.person == 1 && samePhoto.reason == "ambiguous")
    }

    @Test func constraintsAndVerdictsBarPeople() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in
            try face(d, 51, file: 3, vector(1, 0.05))
            try face(d, 52, file: 4, vector(1, 0.05))
            try d.execute(sql: "INSERT INTO face_identity_constraints(file_id,bbox,anchor_file_id,anchor_bbox,source,created_at) VALUES(3,'0,0,1,1',1,'0,0,1,1','review',0)")
            try d.execute(sql: """
                INSERT INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,face_a,face_b)
                VALUES(1,2,0,1,'user',0,52,11)
                """)
            try FaceReview.upsertOutcome(d, faceID: 51, namespace: "m|p", outcome: "rejected", now: 0)
        }
        try await assess(db, [51, 52])
        #expect(try await candidate(db, 51) == nil)
        #expect(try await outcome(db, 51) == "rejected")
        #expect(try await candidate(db, 52) == nil)
        #expect(try await outcome(db, 52) == "unmatched")
    }

    @Test func bestFirstClaimsPersonPerPhoto() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in
            try face(d, 61, file: 3, vector(0.75, 0.66), bbox: "a")
            try face(d, 62, file: 3, vector(1, 0.01), bbox: "b")
        }
        try await assess(db, [61, 62])
        #expect(try await candidate(db, 62)?.person == 1)
        let second = try #require(try await candidate(db, 61))
        #expect(second.person == 2 && second.reason == "ambiguous")
    }

    @Test func autoAssignIsOptIn() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in try face(d, 71, file: 3, vector(1, 0.1)) }
        var settings = FaceReview.Settings()
        settings.autoAssign = true
        try await assess(db, [71], settings: settings)
        #expect(try await owner(db, 71) == 1)
        #expect(try await outcome(db, 71) == "assigned")
        #expect(try await candidate(db, 71) == nil)
        let count = try await db.pool.read { try Int.fetchOne($0, sql: "SELECT file_count FROM persons WHERE id=1") }
        #expect(count == 2)
    }

    @Test func finishMarksPoolAndCountsQueue() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in try face(d, 81, file: 3, vector(1, 0.1)) }
        let queued = await FaceReview.finish(database: db, pool: [11, 12], holdout: [81], model: model, processing: processing,
                                             settings: .init())
        #expect(queued == 1)
        #expect(try await outcome(db, 11) == "clustered")
    }

    @Test func listPagesFiltersAndRefuses() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in
            try face(d, 91, file: 3, vector(1, 0.1))
            try face(d, 92, file: 4, vector(1, 0.3))
            try face(d, 93, file: 5, vector(0.1, 1))
        }
        try await assess(db, [91, 92, 93])
        let page = try await db.pool.read { try FaceReview.list($0, requestID: "r", personID: nil, offset: 0, limit: 2) }
        #expect(page.status == "ok" && page.total == 3 && page.items.map(\.faceID) == [91, 93])
        #expect(page.items.first?.personName == "Ana" && page.items.first?.runnerUpName == "Ben")
        let ana = try await db.pool.read { try FaceReview.list($0, requestID: "r", personID: 1, offset: 1, limit: nil) }
        #expect(ana.total == 2 && ana.items.map(\.faceID) == [92] && ana.offset == 1)
        let refused = try await db.pool.read { try FaceReview.list($0, requestID: "r", personID: nil, offset: 0, limit: 201) }
        #expect(refused.status == "refused" && refused.message == FaceReview.pageRefusal && refused.items.isEmpty)
    }

    private func resolve(_ db: Database, _ face: Int64, _ person: Int64, _ resolution: String) async throws -> FaceReviewPage {
        try await db.pool.write { try FaceReview.resolve($0, requestID: "r", faceID: face, personID: person, resolution: resolution) }
    }

    private func undo(_ db: Database, _ operation: String) async throws -> FaceReviewPage {
        try await db.pool.write { try FaceReview.undo($0, requestID: "r", operationID: operation) }
    }

    @Test func acceptAssignsAndUndoRestoresSuggestion() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in try face(d, 101, file: 3, vector(1, 0.1)) }
        try await assess(db, [101])
        #expect(try await resolve(db, 101, 2, "accept").message == FaceReview.staleMessage)
        let accepted = try await resolve(db, 101, 1, "accept")
        #expect(accepted.status == "ok" && accepted.personID == 1 && accepted.total == 0)
        #expect(try await owner(db, 101) == 1)
        #expect(try await outcome(db, 101) == "accepted")
        let journal = try await db.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM catalog_corrections WHERE kind='face_review'") }
        #expect(journal == 1)
        let undone = try await undo(db, try #require(accepted.operationID))
        #expect(undone.status == "ok" && undone.total == 1)
        #expect(try await owner(db, 101) == nil)
        #expect(try await candidate(db, 101)?.person == 1)
        #expect(try await outcome(db, 101) == "suggested")
        #expect(try await undo(db, accepted.operationID!).message == FaceReview.nothingToUndoMessage)
    }

    @Test func acceptRefusesWhenPersonAlreadyInPhoto() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in
            try face(d, 111, file: 3, vector(1, 0.1))
            try d.execute(sql: "INSERT INTO face_review_candidates(face_id,person_id,score,reason,namespace,created_at) VALUES(111,1,0.9,'suggested','m|p',0)")
            try face(d, 112, file: 3, person: 1, vector(1, 0), bbox: "x")
        }
        let page = try await resolve(db, 111, 1, "accept")
        #expect(page.status == "refused" && page.message == FaceReview.samePhotoMessage)
        #expect(try await owner(db, 111) == nil)
    }

    @Test func rejectStoresConstraintAndUndoRemovesIt() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in
            try d.execute(sql: "UPDATE persons SET representative_face_id=11 WHERE id=1")
            try face(d, 121, file: 3, vector(1, 0.1))
        }
        try await assess(db, [121])
        let rejected = try await resolve(db, 121, 1, "reject")
        #expect(rejected.status == "ok" && rejected.personID == nil)
        let constraints = try await db.pool.read {
            try Row.fetchAll($0, sql: "SELECT file_id,anchor_file_id,source FROM face_identity_constraints").map { [$0["file_id"] as Int64, $0["anchor_file_id"] as Int64] }
        }
        #expect(constraints == [[3, 1]])
        #expect(try await outcome(db, 121) == "rejected")
        try await assess(db, [121])
        #expect(try await candidate(db, 121) == nil)
        _ = try await undo(db, try #require(rejected.operationID))
        let left = try await db.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM face_identity_constraints") }
        #expect(left == 0)
        #expect(try await candidate(db, 121)?.person == 1)
    }

    @Test func ignoreFilesUnderNewUnknownAndUndoDeletesIt() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in try face(d, 131, file: 3, vector(1, 0.1)) }
        try await assess(db, [131])
        let ignored = try await resolve(db, 131, 1, "ignore")
        let unknown = try #require(ignored.personID)
        let flag = try await db.pool.read { try Int.fetchOne($0, sql: "SELECT is_unknown FROM persons WHERE id=?", arguments: [unknown]) }
        #expect(flag == 1)
        #expect(try await owner(db, 131) == unknown)
        try await db.pool.write { try $0.execute(sql: "UPDATE face_prints SET person_id=2 WHERE id=131") }
        #expect(try await undo(db, ignored.operationID!).message == FaceReview.movedMessage)
        try await db.pool.write { try $0.execute(sql: "UPDATE face_prints SET person_id=? WHERE id=131", arguments: [unknown]) }
        #expect(try await undo(db, ignored.operationID!).status == "ok")
        let gone = try await db.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM persons WHERE id=?", arguments: [unknown]) }
        #expect(gone == 0)
        #expect(try await candidate(db, 131)?.person == 1)
    }

    @Test func reassignRecordsCorrectionAndConstraint() async throws {
        let db = try makeDB()
        try await seedPeople(db)
        try await db.pool.write { d in
            try face(d, 141, file: 3, person: 1, vector(1, 0.1), quality: 0.1)
            try d.execute(sql: "UPDATE persons SET representative_face_id=11 WHERE id=1")
        }
        let result = await FileIDEngineMain.reassignFace(database: db, faceID: 141, destinationPersonID: 2, createNewPerson: false)
        #expect(result.succeeded == 1)
        let corrections = try await db.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM catalog_corrections WHERE kind='face_reassign'") }
        let anchor = try await db.pool.read { try Int64.fetchOne($0, sql: "SELECT anchor_file_id FROM face_identity_constraints WHERE source='reassign'") }
        #expect(corrections == 1)
        #expect(anchor == 1)
        #expect(try await outcome(db, 141) == "manual")
    }

    @Test func invalidRequestIDsAreRejected() async throws {
        let reply = await FaceReview.reply(.list(requestID: "x", personID: nil, offset: nil, limit: nil), database: nil)
        #expect(reply.status == "failed" && reply.message == FaceReview.unavailableMessage)
        #expect(!HealthCheckResult.isValidRequestID("bad id"))
    }
}
