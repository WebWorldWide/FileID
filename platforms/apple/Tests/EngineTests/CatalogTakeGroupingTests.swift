import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Library-wide related takes")
struct CatalogTakeGroupingTests {
    private typealias Grouping = CatalogTakeGrouping

    @Test func groupsNeedSimilarContentAndNameTheRelation() async throws {
        let (database, directory) = try makeDatabase()
        defer { try? FileManager.default.removeItem(at: directory) }
        try await database.pool.write { db in try library(db) }
        try await rerun(database)
        let rows = try await stored(database)
        #expect(rows.map(\.0) == ["repeatedAttempts", "alternateViews", "uncertain"])
        #expect(rows.map(\.1) == ["[1,2]", "[4,5]", "[6,7]"])
        #expect(rows[0].2 == #"["captureSession","visualSimilarity","sameCamera"]"#)
        #expect(rows[1].2 == #"["captureSession","visualSimilarity","simultaneousCameras"]"#)
        #expect(rows[2].2 == #"["captureSession","visualSimilarity","cameraDetailsMissing"]"#)
        let response = await CatalogStore.handle(CatalogRequest(requestID: "read", action: "takeGroupProposals"), database: database)
        #expect(response.takeGroups?.count == 3)
        #expect(response.takeGroups?.allSatisfy { $0.status == "open" && $0.kind == "photos" } == true)
        #expect(response.jobs.map(\.state) == ["completed"])
        let events = try await database.pool.read { db in try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM catalog_events") }
        #expect(events == 0)
    }

    @Test func rerunsAreIdempotentAndOnlyTouchChangedSessions() async throws {
        let (database, directory) = try makeDatabase()
        defer { try? FileManager.default.removeItem(at: directory) }
        try await database.pool.write { db in try library(db) }
        try await rerun(database)
        let before = try await stamps(database)
        try await rerun(database)
        #expect(try await stamps(database) == before)
        try await database.pool.write { db in
            try file(db, id: 8, path: "/shoot/a4.jpg", time: 1090, camera: "Phone A", vector: (0.98, 0.2))
            try db.execute(sql: "DELETE FROM files WHERE id=6")
        }
        try await rerun(database)
        #expect(try await stored(database).map(\.1) == ["[1,2,8]", "[4,5]"])
        let party = try await stamps(database).first { $0.hasPrefix(CatalogTakeAuto.key([4, 5])) }
        #expect(party.map(before.contains) == true)
        let tracked = try await database.pool.read { db in try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM take_proposal_files") }
        #expect(tracked == 7)
    }

    @Test func userChoicesEventsAndPeopleAreRespected() async throws {
        let (database, directory) = try makeDatabase()
        defer { try? FileManager.default.removeItem(at: directory) }
        try await database.pool.write { db in
            try library(db)
            try file(db, id: 9, path: "/game/d1.jpg", time: 40000, camera: "Phone A", vector: (1, 0))
            try file(db, id: 10, path: "/game/d2.jpg", time: 40020, camera: "Phone A", vector: (1, 0))
            try file(db, id: 11, path: "/game/d3.jpg", time: 40040, camera: "Phone A", vector: (0.87, 0.493))
            try file(db, id: 12, path: "/game/d4.jpg", time: 40060, camera: "Phone A", vector: (0.87, 0.493))
            try db.execute(sql: "INSERT INTO persons(id,first_name,created_at) VALUES(1,'Sam',0),(2,'Alex',0),(3,'Person 7',0)")
            for (id, fileID, person) in [(1, 9, 1), (2, 10, 2), (3, 11, 1), (4, 12, 3)] {
                try db.execute(sql: "INSERT INTO catalog_observations(id,file_id,person_id,source_revision,model_version,confidence,user_edited) VALUES(?,?,?,?,'user',1,1)",
                               arguments: ["o\(id)", fileID, person, Grouping.revision(size: 100, modified: 10)])
            }
            try db.execute(sql: "INSERT INTO catalog_events(id,title,user_edited) VALUES('e','Saved',1)")
            try db.execute(sql: "INSERT INTO catalog_event_files(event_id,file_id) VALUES('e',6)")
        }
        try await rerun(database)
        let rows = try await stored(database)
        #expect(rows.map(\.1) == ["[1,2]", "[4,5]", "[9,11]"])
        #expect(rows.last?.2.contains("sharedSubject") == true)
        let keep = CatalogTakeDecision(groupKey: CatalogTakeAuto.key([1, 2]), decision: "keepAll", fileIDs: [1, 2])
        let skip = CatalogTakeDecision(groupKey: CatalogTakeAuto.key([4, 5]), decision: "skip", fileIDs: [4, 5])
        for decision in [keep, skip] {
            let response = await CatalogStore.handle(CatalogRequest(requestID: "decide", action: "decideTakeGroup", takeDecision: decision), database: database)
            #expect(response.status == "ok")
        }
        try await rerun(database)
        let groups = try await proposals(database)
        #expect(groups.count == 2)
        #expect(groups.last?.status == "skipped")
        #expect(groups.last?.relation == "alternateViews")
        try await database.pool.write { db in try db.execute(sql: "INSERT INTO catalog_event_files(event_id,file_id) VALUES('e',9)") }
        #expect(try await proposals(database).count == 1)
    }

    @Test func interruptedJobsPauseAndResumeWithoutRedoingSessions() async throws {
        let (database, directory) = try makeDatabase()
        defer { try? FileManager.default.removeItem(at: directory) }
        let total = try await database.pool.write { db -> Int in
            try library(db)
            _ = try Grouping.enqueue(db, requestID: "start")
            try db.execute(sql: "UPDATE catalog_jobs SET state='running' WHERE id=?", arguments: [Grouping.jobID])
            let plan = try Grouping.plan(db)
            try Grouping.process(db, session: plan.dirty[0], processed: 1, total: plan.dirty.count)
            try Grouping.recover(db)
            #expect(throws: Grouping.Interrupted.self) {
                try Grouping.process(db, session: plan.dirty[1], processed: 2, total: plan.dirty.count)
            }
            return plan.dirty.count
        }
        let state = try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [Grouping.jobID]) }
        #expect(state == "paused")
        #expect(try await database.pool.read { db in try Grouping.plan(db).dirty.count } == total - 1)
        try await rerun(database)
        #expect(try await stored(database).count == 3)
        #expect(try await database.pool.read { db in try Grouping.plan(db).dirty.isEmpty })
    }

    private func makeDatabase() throws -> (FileIDEngine.Database, URL) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        return (database, directory)
    }

    private func rerun(_ database: FileIDEngine.Database) async throws {
        try await database.pool.write { db in
            _ = try Grouping.enqueue(db, requestID: "run")
            try db.execute(sql: "UPDATE catalog_jobs SET state='running' WHERE id=? AND state='queued'", arguments: [Grouping.jobID])
            let plan = try Grouping.plan(db)
            for (index, session) in plan.dirty.enumerated() {
                try Grouping.process(db, session: session, processed: index + 1, total: plan.dirty.count)
            }
            try Grouping.finish(db, keys: plan.keys)
            try db.execute(sql: "UPDATE catalog_jobs SET state='completed',progress=1 WHERE id=? AND state='running'", arguments: [Grouping.jobID])
        }
    }

    private func proposals(_ database: FileIDEngine.Database) async throws -> [CatalogTakeAutoGroup] {
        let response = await CatalogStore.handle(CatalogRequest(requestID: "read", action: "takeGroupProposals"), database: database)
        return response.takeGroups ?? []
    }

    private func stored(_ database: FileIDEngine.Database) async throws -> [(String, String, String)] {
        try await database.pool.read { db in
            try Row.fetchAll(db, sql: "SELECT relation,file_ids_json,evidence_json FROM take_proposals ORDER BY file_ids_json")
                .map { ($0[0], $0[1], $0[2]) }
        }
    }

    private func stamps(_ database: FileIDEngine.Database) async throws -> [String] {
        try await database.pool.read { db in
            try Row.fetchAll(db, sql: "SELECT group_key,proposed_at FROM take_proposals ORDER BY group_key").map { row in
                let key: String = row[0]
                let stamp: Double = row[1]
                return "\(key)|\(stamp.bitPattern)"
            }
        }
    }

    private func library(_ db: GRDB.Database) throws {
        try file(db, id: 1, path: "/shoot/a1.jpg", time: 1000, camera: "Phone A", vector: (1, 0))
        try file(db, id: 2, path: "/shoot/a2.jpg", time: 1030, camera: "Phone A", vector: (0.96, 0.28))
        try file(db, id: 3, path: "/shoot/a3.jpg", time: 1060, camera: "Phone A", vector: (0, 1))
        try file(db, id: 4, path: "/party/b1.jpg", time: 9000, camera: "Phone A", vector: (1, 0))
        try file(db, id: 5, path: "/party/b2.jpg", time: 9001, camera: "Camera B", vector: (0.96, 0.28))
        try file(db, id: 6, path: "/walk/c1.jpg", time: 20000, camera: nil, vector: (1, 0))
        try file(db, id: 7, path: "/walk/c2.jpg", time: 20040, camera: "Phone A", vector: (0.96, 0.28))
    }

    private func file(_ db: GRDB.Database, id: Int64, path: String, time: Double, camera: String?, vector: (Float, Float)) throws {
        try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension,camera_model,content_hash) VALUES(?,?,?,100,?,10,0,'image','jpg',?,?)",
                       arguments: [id, path, id, time, camera, Data([UInt8(id)])])
        try db.execute(sql: "INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?,?,?)",
                       arguments: [id, blob(vector), CLIPEmbeddingSpace.modelID])
    }

    private func blob(_ leading: (Float, Float)) -> Data {
        var vector = [Float](repeating: 0, count: CLIPEmbeddingSpace.dimension)
        vector[0] = leading.0
        vector[1] = leading.1
        var result = Data()
        for value in vector {
            var bits = value.bitPattern.littleEndian
            withUnsafeBytes(of: &bits) { result.append(contentsOf: $0) }
        }
        return result
    }
}
