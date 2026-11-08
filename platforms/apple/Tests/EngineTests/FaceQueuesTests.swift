import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

private typealias Database = FileIDEngine.Database

@Suite("Face review queues", .serialized)
struct FaceQueuesTests {
    private func makeDB() throws -> Database {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("FaceQueues-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return try Database(at: dir.appendingPathComponent("t.sqlite"))
    }

    private func vector(_ x: Float, _ y: Float) -> Data {
        var values = [Float](repeating: 0, count: 128)
        values[0] = x; values[1] = y; values[2] = 0.05
        return FloatBlob.encode(values)
    }

    private func file(_ d: GRDB.Database, _ id: Int64) throws {
        try d.execute(sql: """
            INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(?,?,?,1,1,0,'image','jpg')
            """, arguments: [id, "/fixture/\(id).jpg", id])
        try d.execute(sql: "INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?,'r','p',0)", arguments: [id])
    }

    private func person(_ d: GRDB.Database, _ id: Int64, name: String? = nil, unknown: Bool = false) throws {
        try d.execute(sql: "INSERT INTO persons(id,name,is_unknown,created_at,file_count) VALUES(?,?,?,0,0)", arguments: [id, name, unknown ? 1 : 0])
    }

    private func face(_ d: GRDB.Database, _ id: Int64, file: Int64, person: Int64?, _ embedding: Data, excluded: Bool = false) throws {
        try d.execute(sql: """
            INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,face_quality,excluded,embedding_model,processing_version,source_revision)
            VALUES(?,?,?,X'00',?,?,0.9,?,'m','p','r')
            """, arguments: [id, file, person, "box\(id)", embedding, excluded ? 1 : 0])
        if let person {
            try d.execute(sql: "UPDATE persons SET representative_face_id=COALESCE(representative_face_id,?) WHERE id=?", arguments: [id, person])
        }
    }

    private func owner(_ db: Database, _ face: Int64) async throws -> Int64? {
        try await db.pool.read { try Int64.fetchOne($0, sql: "SELECT person_id FROM face_prints WHERE id=?", arguments: [face]) }
    }

    private func refresh(_ db: Database) async throws -> (merges: Int, splits: Int) {
        try await db.pool.write { try FaceQueues.refresh($0, model: "m", processing: "p", settings: .init(), now: 10) }
    }

    private func pairs(_ db: Database) async throws -> [[Int64]] {
        try await db.pool.read { d in
            try Row.fetchAll(d, sql: "SELECT person_id, other_person_id FROM face_merge_suggestions ORDER BY person_id, other_person_id")
                .map { [$0["person_id"], $0["other_person_id"]] }
        }
    }

    private func act(_ db: Database, _ person: Int64, _ other: Int64?, _ resolution: String) async throws -> FaceReviewPage {
        try await db.pool.write { try FaceReview.resolve($0, requestID: "r", faceID: nil, personID: person, resolution: resolution, otherPersonID: other) }
    }

    private func undo(_ db: Database, _ operation: String?) async throws -> FaceReviewPage {
        try await db.pool.write { try FaceReview.undo($0, requestID: "r", operationID: operation ?? "") }
    }

    private func pair() async throws -> Database {
        let db = try makeDB()
        try await db.pool.write { d in
            for id: Int64 in 1...20 { try file(d, id) }
            try person(d, 1)
            try person(d, 2, name: "Alex")
            try face(d, 1, file: 1, person: 1, vector(1, 0))
            try face(d, 2, file: 2, person: 2, vector(0.95, 0.1))
            try face(d, 3, file: 3, person: 2, vector(0.97, 0.05))
        }
        return db
    }

    private func mixed() async throws -> Database {
        let db = try makeDB()
        let points: [(Float, Float)] = [(1, 0), (0.98, 0.05), (0.97, 0), (0.99, 0.02), (0, 1), (0.05, 0.98), (0.02, 0.97)]
        try await db.pool.write { d in
            for id: Int64 in 1...20 { try file(d, id) }
            try person(d, 1, name: "Kim")
            for (index, point) in points.enumerated() {
                try face(d, Int64(index + 1), file: Int64(index + 1), person: 1, vector(point.0, point.1))
            }
        }
        return db
    }

    // MARK: - Quality-gated new faces

    @Test func gatedNewFacesAreHeldOutAndOnlyEverUncertain() async throws {
        let db = try makeDB()
        try await db.pool.write { d in
            for id: Int64 in 1...4 { try file(d, id) }
            try person(d, 1, name: "Ana")
            try face(d, 11, file: 1, person: 1, vector(1, 0))
            try face(d, 21, file: 2, person: nil, vector(1, 0), excluded: true)
        }
        let held = try await db.pool.read { try FaceReview.holdout($0, model: "m", processing: "p", minQuality: 0) }
        #expect(held.contains(21))
        let auto = FaceReview.Settings(autoAssign: true)
        try await db.pool.write { d in
            try FaceReview.assess(d, faceIDs: held, model: "m", processing: "p", settings: auto, now: 5)
        }
        #expect(try await owner(db, 21) == nil)
        let reason = try await db.pool.read { try String.fetchOne($0, sql: "SELECT reason FROM face_review_candidates WHERE face_id=21") }
        #expect(reason == "uncertain")
        let page = try await db.pool.read { try FaceReview.list($0, requestID: "r", personID: nil, offset: 0, limit: 10) }
        #expect(page.items.first?.lowQuality == true)
    }

    @Test func gatedFacesInAFreshSpaceStayInThePass() async throws {
        let db = try makeDB()
        try await db.pool.write { d in
            try file(d, 1)
            try face(d, 21, file: 1, person: nil, vector(1, 0), excluded: true)
        }
        let held = try await db.pool.read { try FaceReview.holdout($0, model: "m", processing: "p", minQuality: 0) }
        #expect(held.isEmpty)
    }

    // MARK: - Accept all

    @Test func acceptAllAcceptsSuggestedSkipsPhotosWithThePersonAndUndoes() async throws {
        let db = try makeDB()
        try await db.pool.write { d in
            for id: Int64 in 1...6 { try file(d, id) }
            try person(d, 1, name: "Ana")
            try person(d, 2)
            try face(d, 1, file: 1, person: 1, vector(1, 0))
            try face(d, 2, file: 2, person: 2, vector(0, 1))
            try face(d, 3, file: 3, person: nil, vector(1, 0))
            try face(d, 4, file: 4, person: nil, vector(0.95, 0.05))
            try FaceReview.assess(d, faceIDs: [3, 4], model: "m", processing: "p", settings: .init(), now: 5)
            try face(d, 6, file: 4, person: 1, vector(1, 0))
        }
        let reply = try await act(db, 1, nil, "acceptAll")
        #expect(reply.status == "ok")
        #expect(reply.acceptedFaceIDs == [3])
        #expect(reply.skippedFaceIDs == [4])
        #expect(try await owner(db, 3) == 1)
        let kind = try await db.pool.read { try String.fetchOne($0, sql: "SELECT kind FROM catalog_corrections WHERE id=?", arguments: [reply.operationID]) }
        #expect(kind == "face_review_bulk")
        let undone = try await undo(db, reply.operationID)
        #expect(undone.status == "ok")
        #expect(undone.operationID == reply.operationID)
        #expect(try await owner(db, 3) == nil)
        #expect(try await undo(db, reply.operationID).status == "refused")
        let none = try await act(db, 2, nil, "acceptAll")
        #expect(none.message == FaceReview.nothingToAcceptMessage)
    }

    // MARK: - Merges

    @Test func mergesPairCloseNamedFirstAndSkipUnknownSharedAndFar() async throws {
        let db = try await pair()
        try await db.pool.write { d in
            try person(d, 3)
            try face(d, 4, file: 4, person: 3, vector(0, 1))
            try person(d, 4, unknown: true)
            try face(d, 5, file: 5, person: 4, vector(1, 0))
            try person(d, 5, name: "Sam")
            try face(d, 6, file: 1, person: 5, vector(1, 0.02))
            try face(d, 7, file: 2, person: 5, vector(1, 0.02))
            try face(d, 8, file: 3, person: 5, vector(1, 0.02))
        }
        _ = try await refresh(db)
        let found = try await pairs(db)
        #expect(found.contains([2, 1]))
        #expect(!found.contains { $0.contains(3) || $0.contains(4) || $0.contains(5) })
        let page = try await db.pool.read { try FaceReview.list($0, requestID: "r", personID: nil, offset: nil, limit: nil, queue: "merges") }
        #expect(page.queue == "merges")
        #expect(page.merges?.first?.personName == "Alex")
        #expect(page.merges?.first?.faceCount == 2)
    }

    @Test func mergeJournalsAndUndoBringsThePersonBack() async throws {
        let db = try await pair()
        _ = try await refresh(db)
        #expect(try await act(db, 1, 2, "merge").message == FaceReview.staleMessage)
        let reply = try await act(db, 2, 1, "merge")
        #expect(reply.status == "ok")
        #expect(reply.otherPersonID == 1)
        #expect(try await owner(db, 1) == 2)
        let verdict = try await db.pool.read { try String.fetchOne($0, sql: "SELECT vlm_model FROM face_verifications WHERE person_a=1 AND person_b=2 AND same_person=1") }
        #expect(verdict == "user-merged")
        let undone = try await undo(db, reply.operationID)
        #expect(undone.status == "ok")
        #expect(undone.queue == "merges")
        #expect(try await owner(db, 1) == 1)
        #expect(try await pairs(db) == [[2, 1]])
        let verdicts = try await db.pool.read { try Int.fetchOne($0, sql: "SELECT COUNT(*) FROM face_verifications") }
        #expect(verdicts == 0)
    }

    @Test func mergeIsRefusedWhenThePeopleShareAPhoto() async throws {
        let db = try await pair()
        _ = try await refresh(db)
        try await db.pool.write { d in try face(d, 9, file: 2, person: 1, vector(1, 0)) }
        let reply = try await act(db, 2, 1, "merge")
        #expect(reply.message == FaceQueues.sharedPhotoMessage)
        #expect(reply.queue == "merges")
    }

    @Test func notSamePersonKeepsThePairOutUntilUndone() async throws {
        let db = try await pair()
        _ = try await refresh(db)
        let reply = try await act(db, 2, 1, "notSamePerson")
        #expect(reply.status == "ok")
        let faces = try await db.pool.read { d in
            try Row.fetchOne(d, sql: "SELECT face_a, face_b FROM face_verifications WHERE person_a=1 AND person_b=2 AND same_person=0")
                .map { [$0["face_a"] as Int64, $0["face_b"] as Int64] }
        }
        #expect(faces == [1, 2])
        _ = try await refresh(db)
        #expect(try await pairs(db).isEmpty)
        #expect(try await undo(db, reply.operationID).status == "ok")
        _ = try await refresh(db)
        #expect(try await pairs(db) == [[2, 1]])
    }

    @Test func dismissalFollowsTheFacesAndUndoForgetsIt() async throws {
        let db = try await pair()
        _ = try await refresh(db)
        let reply = try await act(db, 2, 1, "dismiss")
        #expect(reply.status == "ok")
        try await db.pool.write { d in
            try person(d, 10)
            try d.execute(sql: "UPDATE face_prints SET person_id=10 WHERE person_id=1")
            try d.execute(sql: "DELETE FROM persons WHERE id=1")
        }
        _ = try await refresh(db)
        #expect(try await pairs(db).isEmpty)
        #expect(try await undo(db, reply.operationID).status == "ok")
        _ = try await refresh(db)
        #expect(try await pairs(db) == [[2, 10]])
    }

    // MARK: - Splits

    @Test func splitFlagsSeparatedGroupsAndUndoRestores() async throws {
        let db = try await mixed()
        #expect(try await refresh(db).splits == 1)
        let page = try await db.pool.read { try FaceReview.list($0, requestID: "r", personID: nil, offset: nil, limit: nil, queue: "splits") }
        let split = try #require(page.splits?.first)
        #expect(split.keepCount == 4)
        #expect(split.splitCount == 3)
        #expect(split.keepFaceIDs.contains(1))
        let reply = try await act(db, 1, nil, "split")
        #expect(reply.status == "ok")
        let created = try #require(reply.otherPersonID)
        #expect(try await owner(db, 5) == created)
        #expect(try await refresh(db) == (0, 0))
        #expect(try await undo(db, reply.operationID).status == "ok")
        #expect(try await owner(db, 5) == 1)
        let remaining = try await db.pool.read { try FaceQueues.splitCount($0) }
        #expect(remaining == 1)
    }

    @Test func splitUndoIsRefusedOnceTheNewPersonIsNamed() async throws {
        let db = try await mixed()
        _ = try await refresh(db)
        let reply = try await act(db, 1, nil, "split")
        try await db.pool.write { try $0.execute(sql: "UPDATE persons SET name='Lee' WHERE id=?", arguments: [reply.otherPersonID]) }
        #expect(try await undo(db, reply.operationID).message == FaceQueues.renamedMessage)
    }

    @Test func splitRefusedWhenFacesMovedAndDismissHidesIt() async throws {
        let db = try await mixed()
        _ = try await refresh(db)
        try await db.pool.write { try $0.execute(sql: "UPDATE face_prints SET person_id=NULL WHERE id=6") }
        #expect(try await act(db, 1, nil, "split").message == FaceReview.staleMessage)
        try await db.pool.write { try $0.execute(sql: "UPDATE face_prints SET person_id=1 WHERE id=6") }
        let reply = try await act(db, 1, nil, "dismiss")
        #expect(reply.status == "ok")
        #expect(try await refresh(db).splits == 0)
        #expect(try await undo(db, reply.operationID).status == "ok")
        #expect(try await refresh(db).splits == 1)
    }

    @Test func twoGroupsSeparatesClearGroups() {
        let a: [Float] = [1, 0], b: [Float] = [0, 1]
        let result = FaceQueues.twoGroups([a, a, b, b])
        #expect(result?.groups[0] == result?.groups[1])
        #expect(result?.groups[0] != result?.groups[2])
        #expect(FaceQueues.twoGroups([a, a]) == nil)
    }
}

@Suite("Face refresh backlog", .serialized)
struct FaceRefreshJobTests {
    struct Fake: FaceRefreshJob.Backend {
        let model: String
        func embed(path: String, inputs: [FaceAnalysisCache.Input]) async -> [Int64: (embedding: Data, jpeg: Data?)] {
            var values = [Float](repeating: 0, count: 128)
            values[0] = 1
            var out: [Int64: (embedding: Data, jpeg: Data?)] = [:]
            for input in inputs where !input.bbox.hasPrefix("bad") { out[input.id] = (FloatBlob.encode(values), nil) }
            return out
        }
    }

    private struct Library {
        let db: Database
        let dir: URL
    }

    private func library() async throws -> Library {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("FaceRefresh-\(UUID().uuidString)").resolvingSymlinksInPath()
        let root = dir.appendingPathComponent("library")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let present = root.appendingPathComponent("a.jpg")
        try Data("jpeg".utf8).write(to: present)
        let values = try present.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey])
        let size = Int64(values.fileSize ?? 0)
        let stamp = values.contentModificationDate?.timeIntervalSince1970 ?? 0
        let db = try Database(at: dir.appendingPathComponent("t.sqlite"))
        let gone = dir.appendingPathComponent("gone")
        try await db.pool.write { d in
            try d.execute(sql: "INSERT INTO library_roots(path,display,added_at) VALUES(?,'library',0)", arguments: [root.path])
            try d.execute(sql: "INSERT INTO library_roots(path,display,added_at) VALUES(?,'gone',0)", arguments: [gone.path])
            let files: [(Int64, String, Int64, Double)] = [
                (1, present.path, size, stamp),
                (2, root.appendingPathComponent("deleted.jpg").path, 100, 10),
                (3, gone.appendingPathComponent("b.jpg").path, 100, 10),
            ]
            for (id, path, size, modified) in files {
                try d.execute(sql: """
                    INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(?,?,?,?,?,0,'image','jpg')
                    """, arguments: [id, path, id, size, modified])
                try d.execute(sql: "INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?,'rev','file-stat-v1',0)", arguments: [id])
            }
            let faces: [(Int64, Int64, String, String?)] = [(1, 1, "ok-1", "old"), (2, 1, "bad-2", nil), (3, 2, "ok-3", "old"), (4, 3, "ok-4", "old")]
            for (id, fileID, bbox, model) in faces {
                try d.execute(sql: """
                    INSERT INTO face_prints(id,file_id,print_data,bbox,arcface_embedding,embedding_model,processing_version,source_revision)
                    VALUES(?,?,X'00',?,?,?,'p','rev')
                    """, arguments: [id, fileID, bbox, model == nil ? nil : Data(count: 512), model])
            }
        }
        return Library(db: db, dir: dir)
    }

    private func states(_ db: Database) async throws -> [String] {
        try await db.pool.read { try String.fetchAll($0, sql: "SELECT state FROM face_refresh_backlog ORDER BY face_id") }
    }

    private func start(_ db: Database, processing: String = FaceAnalysisCache.processingVersion) async throws {
        try await db.pool.write { d in
            try FaceRefreshJob.seed(d, model: "m", processing: processing, now: 1)
            try d.execute(sql: "UPDATE catalog_jobs SET state='running' WHERE id=?", arguments: [FaceRefreshJob.jobID])
        }
    }

    @Test func drainingRecordsEveryStateAndTheNextSeedRetries() async throws {
        let lib = try await library()
        defer { try? FileManager.default.removeItem(at: lib.dir) }
        let processing = FaceAnalysisCache.processingVersion
        try await start(lib.db)
        let status = try await lib.db.pool.read { try FaceRefreshJob.status($0, model: "m", processing: processing) }
        #expect(status.total == 4)
        #expect(status.pending == 4)
        try await FaceRefreshJob.drain(database: lib.db, backend: Fake(model: "m"))
        #expect(try await states(lib.db) == ["done", "failed", "missing", "offline"])
        let saved = try await lib.db.pool.read { try String.fetchOne($0, sql: "SELECT embedding_model FROM face_prints WHERE id=1") }
        #expect(saved == "m")
        try await lib.db.pool.write { try FaceRefreshJob.seed($0, model: "m", processing: processing, now: 2) }
        #expect(try await states(lib.db) == ["done", "pending", "pending", "pending"])
    }

    @Test func aPausedJobStopsAndAnotherModelFails() async throws {
        let lib = try await library()
        defer { try? FileManager.default.removeItem(at: lib.dir) }
        try await start(lib.db)
        await #expect(throws: FaceRefreshJob.Failure.self) {
            try await FaceRefreshJob.drain(database: lib.db, backend: Fake(model: "other"))
        }
        try await lib.db.pool.write { try $0.execute(sql: "UPDATE catalog_jobs SET state='paused'") }
        await #expect(throws: FaceRefreshJob.Interrupted.self) {
            try await FaceRefreshJob.drain(database: lib.db, backend: Fake(model: "m"))
        }
        #expect(try await states(lib.db).allSatisfy { $0 == "pending" })
    }

    @Test func anInterruptedJobComesBackPaused() async throws {
        let lib = try await library()
        defer { try? FileManager.default.removeItem(at: lib.dir) }
        try await start(lib.db)
        await FaceRefreshJob.recover(database: lib.db)
        let job = try await lib.db.pool.read { d in
            try Row.fetchOne(d, sql: "SELECT state, error FROM catalog_jobs WHERE id=?", arguments: [FaceRefreshJob.jobID])
                .map { [$0["state"] as String? ?? "", $0["error"] as String? ?? ""] }
        }
        #expect(job == ["paused", FaceRefreshJob.interruptedMessage])
    }

    @Test func enqueueNeedsAModel() async throws {
        let lib = try await library()
        defer { try? FileManager.default.removeItem(at: lib.dir) }
        #expect(throws: FaceRefreshJob.Failure.self) {
            try lib.db.pool.write { try FaceRefreshJob.enqueue($0, requestID: "r", model: nil) }
        }
        let response = try await lib.db.pool.write { try FaceRefreshJob.enqueue($0, requestID: "r", model: "m") }
        #expect(response.jobs.count == 1)
    }

    @Test func classifyTellsMissingFromOffline() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("Classify-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        try Data("x".utf8).write(to: dir.appendingPathComponent("here.jpg"))
        let roots = [dir.path, dir.appendingPathComponent("unplugged").path]
        #expect(FaceRefreshJob.classify(dir.appendingPathComponent("here.jpg").path, roots: roots) == .available)
        #expect(FaceRefreshJob.classify(dir.appendingPathComponent("gone.jpg").path, roots: roots) == .missing)
        #expect(FaceRefreshJob.classify(dir.appendingPathComponent("unplugged/a.jpg").path, roots: roots) == .offline)
        #expect(FaceRefreshJob.classify("/Volumes/FileID-test-volume-absent/a.jpg", roots: []) == .offline)
    }
}
