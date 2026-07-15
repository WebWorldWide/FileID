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

    @Test("search overload leaves every person unchanged")
    func searchOverloadDoesNotPersistPartialPlan() async throws {
        let (db, dir) = try makeDB(); defer { try? FileManager.default.removeItem(at: dir) }
        let vector = l2norm([1, 0, 0])
        try await insertPerson(db, embedding: vector)
        try await insertPerson(db, embedding: vector)
        try await insertPerson(db, embedding: vector)

        let merged = await FaceClustering.tightPairAutoMerge(
            database: db,
            searchLimits: .init(distanceEvaluations: 1, edges: 100,
                                directPairLimit: .max))
        #expect(merged == 0)
        let remaining = try await personIDs(db)
        #expect(remaining.count == 3,
                "a rejected complete-search plan must not mutate the database")
    }

    @Test("one oversized embedding rejects auto-merge before decoding")
    func oversizedEmbeddingFailsClosed() async throws {
        let (db, dir) = try makeDB(); defer { try? FileManager.default.removeItem(at: dir) }
        let vector = l2norm([1, 0, 0])
        try await insertPerson(db, embedding: vector)
        try await insertPerson(db, embedding: vector)
        try await insertPerson(db, embedding: [Float](repeating: 1, count: 4_097))

        let merged = await FaceClustering.tightPairAutoMerge(database: db)
        #expect(merged == 0)
        let remaining = try await personIDs(db)
        #expect(remaining.count == 3)
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

    // R3-10 — a person named ONLY via a structured field other than first/last/
    // name (title / middle_name / suffix) must still count as NAMED, so two such
    // persons never auto-merge (the merge would delete one identity). The `named`
    // SQL predicate previously checked only first_name/last_name/name.
    @Test("a title/middle/suffix-only-named person is treated as named")
    func structuredNameOnlyCountsAsNamed() async throws {
        let (db, dir) = try makeDB(); defer { try? FileManager.default.removeItem(at: dir) }
        let v = l2norm([1, 0, 0])
        func insertTitled(title: String?, middle: String?, suffix: String?) async throws -> Int64 {
            try await db.pool.write { d -> Int64 in
                try d.execute(sql: """
                    INSERT INTO persons (name, representative_face_id, file_count, created_at,
                                         title, middle_name, suffix, is_unknown)
                    VALUES (NULL, NULL, 5, ?, ?, ?, ?, 0)
                    """, arguments: [Date().timeIntervalSince1970, title, middle, suffix])
                let pid = d.lastInsertedRowID
                let blob = ArcFaceService.embeddingToBlob(v)
                try d.execute(sql: """
                    INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension)
                    VALUES (?, ?, 1, ?, 'image', 'jpg')
                    """, arguments: ["/titled\(pid).jpg", pid * 7000, Date().timeIntervalSince1970])
                let fileID = d.lastInsertedRowID
                try d.execute(sql: """
                    INSERT INTO face_prints (file_id, person_id, print_data, bbox, arcface_embedding)
                    VALUES (?, ?, ?, '0,0,1,1', ?)
                    """, arguments: [fileID, pid, Data(), blob])
                return pid
            }
        }
        let a = try await insertTitled(title: "Dr.", middle: nil, suffix: nil)
        let b = try await insertTitled(title: nil, middle: "Quincy", suffix: "Jr.")

        let merged = await FaceClustering.tightPairAutoMerge(database: db)
        #expect(merged == 0, "two persons named via title/middle/suffix must not merge")
        let ids = try await personIDs(db)
        #expect(ids.contains(a) && ids.contains(b), "neither structured-named identity is deleted")
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

@Suite("Exact face centroid threshold join")
struct ExactCosineJoinTests {
    private struct Generator {
        var state: UInt64

        mutating func next() -> Float {
            state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return Float(Double(state >> 11) / Double(UInt64.max >> 11) * 2 - 1)
        }
    }

    private func corpus(seed: UInt64, count: Int, dimension: Int) -> [[Float]] {
        var generator = Generator(state: seed)
        return (0..<count).map { _ in
            l2norm((0..<dimension).map { _ in generator.next() })
        }
    }

    @Test("person, embedding-row, and embedding-byte preflight caps are independent")
    func inputCaps() {
        // Precompute each Bool into a typed Int64 call so the #expect macro sees
        // a plain Bool — the multi-arg integer arithmetic inside the macro
        // autoclosure otherwise blows the Swift type-checker's time budget.
        let allWithin = FaceClustering.autoMergeInputWithinLimits(
            personCount: 20_000, embeddingCount: 250_000,
            embeddingBytes: Int64(768) * 1024 * 1024, maxEmbeddingBytes: 16 * 1024)
        #expect(allWithin)
        let rowsOver = FaceClustering.autoMergeInputWithinLimits(
            personCount: 1, embeddingCount: 250_001,
            embeddingBytes: Int64(250_001) * 2_048, maxEmbeddingBytes: 2_048)
        #expect(!rowsOver)
        let bytesOver = FaceClustering.autoMergeInputWithinLimits(
            personCount: 1, embeddingCount: 2,
            embeddingBytes: Int64(768) * 1024 * 1024 + 1, maxEmbeddingBytes: 2_048)
        #expect(!bytesOver)
        let perRowOver = FaceClustering.autoMergeInputWithinLimits(
            personCount: 1, embeddingCount: 1,
            embeddingBytes: Int64(16) * 1024 + 1, maxEmbeddingBytes: 16 * 1024 + 1)
        #expect(!perRowOver)
    }

    @Test("VP-tree edges equal the deterministic direct sweep")
    func randomizedEquivalence() {
        for seed in UInt64(1)...UInt64(8) {
            let vectors = corpus(seed: seed, count: 120, dimension: 16)
            let small = vectors.indices.map { $0.isMultiple(of: 7) }
            let direct = ExactCosineJoin.edges(
                vectors: vectors, small: small,
                tightThreshold: 0.65, smallThreshold: 0.55,
                limits: .init(distanceEvaluations: 1_000_000, edges: 100_000,
                              directPairLimit: .max))
            let indexed = ExactCosineJoin.edges(
                vectors: vectors, small: small,
                tightThreshold: 0.65, smallThreshold: 0.55,
                limits: .init(distanceEvaluations: 1_000_000, edges: 100_000,
                              directPairLimit: 0))
            guard case let .success(directEdges, _) = direct,
                  case let .success(indexedEdges, _) = indexed else {
                Issue.record("both exact paths must complete")
                continue
            }
            #expect(indexedEdges == directEdges)
        }
    }

    @Test("high-dimensional ULP boundaries and singleton rules match the direct predicate")
    func boundariesAndSingletons() {
        let tight: Float = 0.65
        let loose: Float = 0.55
        let values = [tight.nextUp, tight, tight.nextDown,
                      loose.nextUp, loose, loose.nextDown]
        func vector(_ cosine: Float) -> [Float] {
            var result = [Float](repeating: 0, count: 512)
            result[0] = cosine
            result[1] = max(0, 1 - cosine * cosine).squareRoot()
            return result
        }
        var base = [Float](repeating: 0, count: 512)
        base[0] = 1
        var vectors = [base] + values.map(vector)
        for dimension in 2..<34 {
            var distractor = [Float](repeating: 0, count: 512)
            distractor[dimension] = 1
            vectors.append(distractor)
        }
        var small = Array(repeating: false, count: vectors.count)
        small[4] = true
        small[5] = true
        small[6] = true
        let direct = ExactCosineJoin.edges(
            vectors: vectors, small: small,
            tightThreshold: tight, smallThreshold: loose,
            limits: .init(distanceEvaluations: 100_000, edges: 10_000,
                          directPairLimit: .max))
        let indexed = ExactCosineJoin.edges(
            vectors: vectors, small: small,
            tightThreshold: tight, smallThreshold: loose,
            limits: .init(distanceEvaluations: 100_000, edges: 10_000,
                          directPairLimit: 0))
        guard case let .success(directEdges, _) = direct,
              case let .success(indexedEdges, _) = indexed else {
            Issue.record("boundary searches must complete")
            return
        }
        #expect(indexedEdges == directEdges)
    }

    @Test("dense and adversarial searches fail before returning a partial graph")
    func limitsFailClosed() {
        let dense = Array(repeating: l2norm([1, 0, 0, 0]), count: 100)
        let edgeLimited = ExactCosineJoin.edges(
            vectors: dense, small: Array(repeating: false, count: dense.count),
            tightThreshold: 0.65, smallThreshold: 0.55,
            limits: .init(distanceEvaluations: 100_000, edges: 10,
                          directPairLimit: 0))
        guard case let .limitExceeded(reason, _) = edgeLimited else {
            Issue.record("dense graph must reject the whole result")
            return
        }
        #expect(reason == "qualifying_edges")

        let spread = corpus(seed: 99, count: 100, dimension: 16)
        let workLimited = ExactCosineJoin.edges(
            vectors: spread, small: Array(repeating: false, count: spread.count),
            tightThreshold: 0.65, smallThreshold: 0.55,
            limits: .init(distanceEvaluations: 10, edges: 10_000,
                          directPairLimit: 0))
        guard case let .limitExceeded(workReason, _) = workLimited else {
            Issue.record("work budget must reject the whole result")
            return
        }
        #expect(workReason == "distance_evaluations")
    }
}

// Junk-cluster suppression: a 1–2 face cluster of only low-quality faces is
// dropped (left unclustered) so it can't spawn a spurious singleton person,
// while size≥minSize clusters and any cluster with a good face survive. Pure
// function — no DB needed. (face-quality gate; calibrated 407→~285 on the
// 991-face reference library)
@Suite("Face clustering low-quality suppression")
struct FaceQualitySuppressionTests {
    // dense idx → face id; face id → quality
    private static let denseToFaceID: [Int64] = [10, 11, 12, 13, 14, 15, 16]
    private static let quality: [Int64: Double] = [
        10: 0.05, 11: 0.06,   // junk pair
        12: 0.40,             // good singleton
        13: 0.05,             // junk singleton
        14: 0.04, 15: 0.03, 16: 0.02,  // junk TRIPLE (corroborated by size)
    ]
    private static let byCluster: [Int: [Int]] = [
        100: [0, 1],     // doubleton, maxQ 0.06 → suppress
        101: [2],        // singleton, q 0.40 → keep
        102: [3],        // singleton, q 0.05 → suppress
        103: [4, 5, 6],  // size 3, all low → keep (size wins)
    ]

    @Test("low-quality 1–2 face clusters are suppressed; size≥3 and good faces survive")
    func suppressesJunkMicroClusters() {
        let r = FaceClustering.suppressLowQualityClusters(
            Self.byCluster, denseToFaceID: Self.denseToFaceID,
            faceQualityByID: Self.quality, minSize: 3, qualityFloor: 0.12)
        #expect(Set(r.kept.keys) == [101, 103], "kept the good singleton + the size-3 cluster")
        #expect(r.suppressedClusters == 2, "the junk pair + junk singleton are dropped")
        #expect(r.suppressedFaces == 3, "2 faces from the pair + 1 from the singleton")
    }

    @Test("a doubleton keeps the cluster when its BEST face clears the floor")
    func maxQualityRuleKeepsMixedPair() {
        let r = FaceClustering.suppressLowQualityClusters(
            [7: [0, 2]],  // faces 10 (0.05) + 12 (0.40) → maxQ 0.40
            denseToFaceID: Self.denseToFaceID, faceQualityByID: Self.quality,
            minSize: 3, qualityFloor: 0.12)
        #expect(r.kept.keys.contains(7), "one good face rescues the pair")
        #expect(r.suppressedClusters == 0)
    }

    @Test("qualityFloor <= 0 disables suppression (keeps every cluster)")
    func floorZeroIsNoOp() {
        let r = FaceClustering.suppressLowQualityClusters(
            Self.byCluster, denseToFaceID: Self.denseToFaceID,
            faceQualityByID: Self.quality, minSize: 3, qualityFloor: 0.0)
        #expect(r.kept.count == Self.byCluster.count, "no-op keeps all 4 clusters")
        #expect(r.suppressedClusters == 0 && r.suppressedFaces == 0)
    }

    @Test("env overrides parse + clamp")
    func envKnobsClampAndDefault() {
        // Defaults when unset (the validated 407→~285 operating point).
        #expect(FaceClustering.minClusterSizeToKeep == 3)
        #expect(abs(FaceClustering.soloQualityFloor - 0.12) < 1e-9)
    }
}
