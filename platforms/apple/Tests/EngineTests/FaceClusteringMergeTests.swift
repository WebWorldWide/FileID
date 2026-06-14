// DB-backed correctness for the auto-merge + persist guards: unknown persons
// never merge (F-C3-003), "different people" verdicts block a merge (F-C3-004),
// a bridge singleton can't transitively merge two named persons (F-C3-005), the
// persist re-reads identity under the writer lock (F-C3-002), a dangling
// representative_face_id is reconciled (F-C3-041), and permanently-failing
// extraction rows are skipped so newer faces progress (F-C3-033).
import Testing
import Foundation
import GRDB
@testable import FileIDEngine
// Disambiguate from GRDB.Database (both modules export `Database`).
private typealias Database = FileIDEngine.Database

private func l2norm(_ v: [Float]) -> [Float] {
    var n: Float = 0
    for x in v { n += x * x }
    let inv = Float(1) / max(.leastNonzeroMagnitude, n.squareRoot())
    return v.map { $0 * inv }
}

@Suite("Face clustering auto-merge + persist guards")
struct FaceClusteringMergeTests {

    private func makeDB() throws -> (Database, URL) {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("FaceMerge-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return (try Database(at: dir.appendingPathComponent("t.sqlite")), dir)
    }

    @discardableResult
    private func insertPerson(
        _ db: Database, firstName: String? = nil, isUnknown: Bool = false,
        fileCount: Int = 5, embedding: [Float], faces: Int = 1
    ) async throws -> (person: Int64, faceIDs: [Int64]) {
        try await db.pool.write { d -> (Int64, [Int64]) in
            try d.execute(sql: """
                INSERT INTO persons (name, representative_face_id, file_count, created_at,
                                     first_name, is_unknown)
                VALUES (NULL, NULL, ?, ?, ?, ?)
                """, arguments: [fileCount, Date().timeIntervalSince1970,
                                 firstName, isUnknown ? 1 : 0])
            let pid = d.lastInsertedRowID
            let blob = ArcFaceService.embeddingToBlob(embedding)
            var faceIDs: [Int64] = []
            for k in 0..<faces {
                try d.execute(sql: """
                    INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension)
                    VALUES (?, ?, 1, ?, 'image', 'jpg')
                    """, arguments: ["/p\(pid)_f\(k).jpg", pid * 1000 + Int64(k),
                                     Date().timeIntervalSince1970])
                let fileID = d.lastInsertedRowID
                try d.execute(sql: """
                    INSERT INTO face_prints (file_id, person_id, print_data, bbox, arcface_embedding)
                    VALUES (?, ?, ?, '0,0,1,1', ?)
                    """, arguments: [fileID, pid, Data(), blob])
                faceIDs.append(d.lastInsertedRowID)
            }
            return (pid, faceIDs)
        }
    }

    private func personIDs(_ db: Database) async throws -> Set<Int64> {
        try await db.pool.read { d in Set(try Int64.fetchAll(d, sql: "SELECT id FROM persons")) }
    }

    // F-C3-003 — an is_unknown person is excluded from auto-merge entirely; the
    // "don't identify these" verdict is never overwritten by a cosine match.
    @Test("an is_unknown person is never auto-merged")
    func unknownNeverMerged() async throws {
        let (db, dir) = try makeDB(); defer { try? FileManager.default.removeItem(at: dir) }
        let v = l2norm([1, 0, 0])
        let (u, _) = try await insertPerson(db, isUnknown: true, embedding: v)
        try await insertPerson(db, embedding: v)   // unnamed, identical centroid
        try await insertPerson(db, embedding: v)   // unnamed, identical centroid

        let merged = await FaceClustering.tightPairAutoMerge(database: db)
        #expect(merged == 1, "the two unnamed persons collapse; the unknown stays out of it")
        let ids = try await personIDs(db)
        #expect(ids.contains(u), "the unknown person row survives untouched")
        #expect(ids.count == 2, "unknown + one survivor of the two unnamed clusters")
        let stillUnknown = try await db.pool.read { d in
            try Int.fetchOne(d, sql: "SELECT is_unknown FROM persons WHERE id = ?", arguments: [u])
        }
        #expect(stillUnknown == 1)
    }

    // F-C3-004 — a face_verifications "different people" verdict blocks the merge
    // even when the two centroids are identical.
    @Test("a 'different' verdict pair is never auto-merged")
    func verdictBlocksMerge() async throws {
        let (db, dir) = try makeDB(); defer { try? FileManager.default.removeItem(at: dir) }
        let v = l2norm([1, 0, 0])
        let (a, fa) = try await insertPerson(db, embedding: v)
        let (b, fb) = try await insertPerson(db, embedding: v)
        try await db.pool.write { d in
            try d.execute(sql: """
                INSERT INTO face_verifications
                    (person_a, person_b, same_person, confidence, vlm_model, verified_at, face_a, face_b)
                VALUES (?, ?, 0, 0.9, 'test', ?, ?, ?)
                """, arguments: [a, b, Date().timeIntervalSince1970, fa[0], fb[0]])
        }
        let merged = await FaceClustering.tightPairAutoMerge(database: db)
        #expect(merged == 0, "the user-refused pair must not be force-merged")
        let ids = try await personIDs(db)
        #expect(ids.contains(a) && ids.contains(b))
    }

    // F-C3-005 — a bridge singleton high-cosine to two distinct NAMED persons
    // must not chain them into one identity (which would delete a name).
    @Test("a bridge singleton cannot transitively merge two named persons")
    func namedBridgeStaysSeparate() async throws {
        let (db, dir) = try makeDB(); defer { try? FileManager.default.removeItem(at: dir) }
        let v = l2norm([1, 0, 0])
        let (a, _) = try await insertPerson(db, firstName: "Adam", fileCount: 5, embedding: v)
        let (b, _) = try await insertPerson(db, firstName: "Bob", fileCount: 5, embedding: v)
        try await insertPerson(db, firstName: nil, fileCount: 1, embedding: v)  // bridge

        let merged = await FaceClustering.tightPairAutoMerge(database: db)
        #expect(merged == 1, "only the bridge is absorbed; the named pair stays apart")
        let ids = try await personIDs(db)
        #expect(ids.contains(a) && ids.contains(b), "neither named identity is deleted")
        let names = Set(try await db.pool.read { d in
            try String.fetchAll(d, sql: "SELECT first_name FROM persons WHERE first_name IS NOT NULL")
        })
        #expect(names.contains("Adam") && names.contains("Bob"))
    }

    // F-C3-002 — the persist's identity carry-forward re-reads persons UNDER the
    // writer lock, so an edit committed during the lock-free clustering window
    // survives. `priorAnchors(from:)` is that under-lock read; it must reflect a
    // change made earlier in the same transaction (not a pre-captured snapshot).
    @Test("persist re-reads identity under the writer lock")
    func underLockReReadSeesInTxnEdit() async throws {
        let (db, dir) = try makeDB(); defer { try? FileManager.default.removeItem(at: dir) }
        let (p1, _) = try await insertPerson(db, firstName: "Old", embedding: l2norm([1, 0, 0]))
        let observed: [String?] = try await db.pool.write { d -> [String?] in
            try d.execute(sql: "UPDATE persons SET first_name = 'New' WHERE id = ?", arguments: [p1])
            return try FaceClustering.priorAnchors(from: d).map { $0.firstName }
        }
        #expect(observed.contains("New"), "under-lock read reflects the committed-in-txn rename")
        #expect(!observed.contains("Old"), "the stale pre-edit name is gone")
    }

    // F-C3-041 — a representative_face_id that points at a missing/foreign face
    // (e.g. cascade-deleted mid-pass) is repaired to a surviving member face, or
    // NULL when none remain — never left dangling.
    @Test("a dangling representative_face_id is reconciled")
    func reconcileDanglingRepFace() async throws {
        let (db, dir) = try makeDB(); defer { try? FileManager.default.removeItem(at: dir) }
        let (p, faces) = try await insertPerson(db, embedding: l2norm([1, 0, 0]), faces: 1)
        try await db.pool.write { d in
            try d.execute(sql: "UPDATE persons SET representative_face_id = 999999 WHERE id = ?",
                          arguments: [p])
        }
        try await db.pool.write { d in try FaceClustering.repairDanglingRepresentativeFaces(d) }
        let rep = try await db.pool.read { d in
            try Int64.fetchOne(d, sql: "SELECT representative_face_id FROM persons WHERE id = ?",
                               arguments: [p])
        }
        #expect(rep == faces[0], "rep is repaired to the surviving member face")

        let empty = try await db.pool.write { d -> Int64 in
            try d.execute(sql: """
                INSERT INTO persons (representative_face_id, file_count, created_at)
                VALUES (888888, 0, ?)
                """, arguments: [Date().timeIntervalSince1970])
            return d.lastInsertedRowID
        }
        try await db.pool.write { d in try FaceClustering.repairDanglingRepresentativeFaces(d) }
        let repEmpty = try await db.pool.read { d in
            try Int64.fetchOne(d, sql: "SELECT representative_face_id FROM persons WHERE id = ?",
                               arguments: [empty])
        }
        #expect(repEmpty == nil, "a person with no surviving faces gets NULL, not a dangle")
    }

    // F-C3-033 — a row that keeps failing extraction drops out of the pending
    // window after the attempt budget, so it can't sit at the front of
    // `ORDER BY id ASC LIMIT` forever and starve newer faces. A later success
    // rehabilitates it (the skip is in-memory, never a DB exclusion).
    @Test("a permanently-failing extraction row is skipped so newer rows progress")
    func extractionStarvationSkip() async {
        FaceClustering.resetExtractionFailuresForTesting()
        defer { FaceClustering.resetExtractionFailuresForTesting() }
        let fid: Int64 = 4242
        #expect(!FaceClustering.permanentlyFailedExtractions().contains(fid))
        FaceClustering.recordExtractionOutcomes(attempted: [fid], succeeded: [])
        FaceClustering.recordExtractionOutcomes(attempted: [fid], succeeded: [])
        #expect(!FaceClustering.permanentlyFailedExtractions().contains(fid),
                "two misses is within the retry budget")
        FaceClustering.recordExtractionOutcomes(attempted: [fid], succeeded: [])
        #expect(FaceClustering.permanentlyFailedExtractions().contains(fid),
                "past the budget the row is skipped from the window")
        FaceClustering.recordExtractionOutcomes(attempted: [fid], succeeded: [fid])
        #expect(!FaceClustering.permanentlyFailedExtractions().contains(fid),
                "a later success rehabilitates the row")
    }
}
