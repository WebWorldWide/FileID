// Face clustering — runs after each scan and on demand.
//
// Phase 1 — extract Vision face prints lazily for any rows that only
//           have a bbox. Bounded concurrency avoids ANE thrash.
// Phase 2 — greedy HNSW clustering + rebuild of the persons table.
//
// Memory: ~2 KB per face print; 50K faces ≈ 100 MB peak.
import Foundation
import GRDB
import Vision
import CoreImage
import ImageIO
import FileIDShared

public enum FaceClustering {

    /// Tight bootstrap threshold — over-clusters on purpose so the VLM
    /// pass can confidently merge. A higher value (0.50+) false-merges
    /// similar-looking different people, which can't be undone without
    /// re-scanning.
    public static let distanceThreshold: Float = 0.30

    /// Centroid distance below which two clusters are auto-merged with
    /// no VLM check, regardless of size. At < 0.40 it is virtually
    /// always the same person (siblings start at ~0.50).
    public static let tightAutoMergeL2: Float = 0.40

    /// Looser threshold used only when at least one cluster is a single
    /// face — those are almost always fragments of an existing person,
    /// not a real distinct identity.
    public static let smallClusterAutoMergeL2: Float = 0.50

    /// Cap so a corrupt DB can't spawn arbitrarily many person rows.
    public static let maxPersons: Int = 8000

    /// Cap on faces per run. Clustering is idempotent — caller re-runs
    /// to pick up overflow on libraries with more faces than this.
    public static let maxFacesPerRun: Int = 200_000

    /// Run a clustering pass. Returns a summary the engine emits over IPC.
    public static func runClustering(
        database: Database,
        sink: IPCSink
    ) async -> FaceClusteringResult {
        let started = Date()

        // PHASE 1 — extract any pending face prints (and ArcFace embeddings
        // if the model is loaded). Idempotent — re-running just picks up
        // new rows from prior scans.
        await extractPendingPrints(database: database, sink: sink)

        // PHASE 2 — load every face_prints row that has an embedding AND
        // wasn't excluded by the quality filter. Prefer ArcFace; fall
        // back to the legacy Vision feature print when ArcFace is empty
        // (model not installed yet, or migration hasn't run).
        struct FaceRow: Sendable {
            let id: Int64
            let arcFace: Data?
            let visionPrint: Data
        }
        let rows: [FaceRow]
        do {
            rows = try await database.pool.read { db in
                let r = try GRDB.Row.fetchAll(db, sql: """
                    SELECT id, print_data, arcface_embedding
                    FROM face_prints
                    WHERE excluded = 0
                      AND (LENGTH(print_data) > 0 OR LENGTH(arcface_embedding) > 0)
                    ORDER BY id ASC LIMIT \(maxFacesPerRun)
                    """)
                return r.map { row in
                    FaceRow(id: row["id"] ?? 0,
                            arcFace: row["arcface_embedding"],
                            visionPrint: row["print_data"] ?? Data())
                }
            }
        } catch {
            JSONLog.shared.error(ev: "face_cluster_query_failed", error: "\(error)")
            await sink.emit(.error(EngineError(
                kind: "face_cluster_failed",
                message: "Could not load face prints: \(error)"
            )))
            return FaceClusteringResult(personCount: 0, faceCount: 0,
                                        unmatchedFaces: 0,
                                        durationSeconds: Date().timeIntervalSince(started))
        }

        guard !rows.isEmpty else {
            JSONLog.shared.info(ev: "face_cluster_empty")
            return FaceClusteringResult(personCount: 0, faceCount: 0,
                                        unmatchedFaces: 0,
                                        durationSeconds: Date().timeIntervalSince(started))
        }

        // Decide which embedding family to cluster on. If MOST rows have
        // ArcFace, use ArcFace (drop Vision-only rows from this pass —
        // they'll get embedded on the next migration run). Otherwise fall
        // back to Vision feature prints. Mixing the two is forbidden:
        // they live in different metric spaces, so cosine similarity
        // between them is meaningless.
        let arcFaceCount = rows.reduce(0) { $0 + (($1.arcFace?.count ?? 0) > 0 ? 1 : 0) }
        let useArcFace = arcFaceCount * 2 >= rows.count   // ≥ 50% threshold

        struct DecodedFace { let id: Int64; let vec: [Float] }
        var decoded: [DecodedFace] = []
        decoded.reserveCapacity(rows.count)
        for row in rows {
            if useArcFace, let blob = row.arcFace, blob.count > 0 {
                let vec = ArcFaceService.blobToEmbedding(blob)
                if !vec.isEmpty { decoded.append(DecodedFace(id: row.id, vec: vec)) }
            } else if !useArcFace, row.visionPrint.count > 0 {
                if let vec = decodePrint(row.visionPrint) {
                    decoded.append(DecodedFace(id: row.id, vec: vec))
                }
            }
        }
        guard let firstDim = decoded.first?.vec.count else {
            JSONLog.shared.warn(ev: "face_cluster_no_decodable_prints",
                                error: "all \(rows.count) embeddings failed to decode")
            return FaceClusteringResult(personCount: 0, faceCount: 0,
                                        unmatchedFaces: rows.count,
                                        durationSeconds: Date().timeIntervalSince(started))
        }
        decoded = decoded.filter { $0.vec.count == firstDim }
        JSONLog.shared.info(ev: "face_cluster_decoded",
                            extra: ["raw": AnyCodable(rows.count),
                                    "decoded": AnyCodable(decoded.count),
                                    "dim": AnyCodable(firstDim),
                                    "embedder": AnyCodable(useArcFace ? "arcface" : "vision_print")])

        // PHASE 3 — Chinese Whispers over a kNN cosine graph.
        // Build an HNSW index for the kNN search; each face becomes a
        // dense node index (insert order). HNSW returns L2 distances;
        // for L2-normalized embeddings cosine_sim = 1 - L2² / 2.
        let index = HNSWIndex(dim: firstDim, M: 16, efConstruction: 200, efSearch: 50)
        var denseToFaceID: [Int64] = []
        var vecsByDense: [[Float]] = []
        denseToFaceID.reserveCapacity(decoded.count)
        vecsByDense.reserveCapacity(decoded.count)
        var unmatched = 0
        for face in decoded {
            let hnswID = index.insert(face.vec)
            guard hnswID >= 0 else { unmatched += 1; continue }
            denseToFaceID.append(face.id)
            vecsByDense.append(face.vec)
        }
        let n = denseToFaceID.count
        guard n > 0 else {
            JSONLog.shared.warn(ev: "face_cluster_no_inserts",
                                error: "HNSW rejected every embedding")
            return FaceClusteringResult(personCount: 0, faceCount: decoded.count,
                                        unmatchedFaces: rows.count,
                                        durationSeconds: Date().timeIntervalSince(started))
        }

        let cwParams = ChineseWhispers.Hyperparameters(
            kNN: 20, cosineThreshold: 0.40, maxIter: 20)
        let adjacency = ChineseWhispers.buildKNNGraph(
            nodeCount: n,
            params: cwParams
        ) { idx -> [(neighbor: Int, similarity: Float)] in
            let hits = index.search(vecsByDense[idx], k: cwParams.kNN + 1)
            return hits.compactMap { (rawID, l2dist) -> (neighbor: Int, similarity: Float)? in
                let nID = Int(rawID)
                guard nID >= 0 && nID < n && nID != idx else { return nil }
                let cosine = 1.0 - (l2dist * l2dist) / 2.0
                return (neighbor: nID, similarity: cosine)
            }
        }

        let cwResult = ChineseWhispers.cluster(adjacency: adjacency, params: cwParams)
        // Group dense nodes by cluster id.
        var byCluster: [Int: [Int]] = [:]
        for (denseIdx, cid) in cwResult.clusterIDs.enumerated() {
            byCluster[cid, default: []].append(denseIdx)
        }
        // Cap at maxPersons. If CW produced more clusters than the cap,
        // keep the largest N and absorb the rest as "unmatched". Real
        // libraries rarely hit this — caps catch corruption.
        var truncatedAtCap = 0
        if byCluster.count > maxPersons {
            let sorted = byCluster.sorted { $0.value.count > $1.value.count }
            let kept = Dictionary(uniqueKeysWithValues: sorted.prefix(maxPersons).map { ($0.key, $0.value) })
            for entry in sorted.dropFirst(maxPersons) {
                truncatedAtCap += entry.value.count
                unmatched += entry.value.count
            }
            byCluster = kept
        }
        if truncatedAtCap > 0 {
            JSONLog.shared.warn(ev: "face_cluster_truncated",
                                error: "Chinese Whispers produced \(byCluster.count + truncatedAtCap) clusters > maxPersons (\(maxPersons)); \(truncatedAtCap) faces unclustered.")
        }

        JSONLog.shared.info(ev: "face_cluster_built",
                            extra: ["faces": AnyCodable(decoded.count),
                                    "clusters": AnyCodable(byCluster.count),
                                    "unmatched": AnyCodable(unmatched),
                                    "iterations": AnyCodable(cwResult.iterations),
                                    "buildSeconds": AnyCodable(Date().timeIntervalSince(started))])

        // PHASE 4 — Persist persons + face_prints.person_id assignment.
        struct ClusterPersist: Sendable {
            let repFaceID: Int64
            let faceIDs: [Int64]
            let count: Int
        }
        let personsList: [ClusterPersist] = byCluster.map { _, denseIdxs in
            // Pick the first dense index as representative — CW preserves
            // insert order within a cluster, so this is stable across
            // re-runs (with the same seed).
            let faceIDs = denseIdxs.map { denseToFaceID[$0] }
            return ClusterPersist(
                repFaceID: faceIDs.first ?? 0,
                faceIDs: faceIDs,
                count: faceIDs.count
            )
        }
        do {
            try await database.pool.write { db in
                try db.execute(sql: "UPDATE face_prints SET person_id = NULL")
                try db.execute(sql: "DELETE FROM persons")

                let now = Date().timeIntervalSinceReferenceDate
                for p in personsList {
                    try db.execute(sql: """
                        INSERT INTO persons (name, representative_face_id, file_count, created_at)
                        VALUES (NULL, ?, ?, ?)
                        """, arguments: [p.repFaceID, p.count, now])
                    let personID = db.lastInsertedRowID
                    for chunk in stride(from: 0, to: p.faceIDs.count, by: 500).map({
                        Array(p.faceIDs[$0..<min($0 + 500, p.faceIDs.count)])
                    }) {
                        let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                        var args: [DatabaseValueConvertible] = [personID]
                        args.append(contentsOf: chunk.map { Int($0) })
                        try db.execute(
                            sql: "UPDATE face_prints SET person_id = ? WHERE id IN (\(placeholders))",
                            arguments: StatementArguments(args)
                        )
                    }
                }
                try db.execute(sql: """
                    UPDATE persons SET file_count = (
                        SELECT COUNT(DISTINCT file_id)
                        FROM face_prints
                        WHERE face_prints.person_id = persons.id
                    )
                    """)
            }
        } catch {
            JSONLog.shared.error(ev: "face_cluster_persist_failed", error: "\(error)")
            await sink.emit(.error(EngineError(
                kind: "face_cluster_persist_failed",
                message: "Could not write clusters: \(error)"
            )))
            return FaceClusteringResult(personCount: 0, faceCount: decoded.count,
                                        unmatchedFaces: unmatched,
                                        durationSeconds: Date().timeIntervalSince(started))
        }
        let postCWPersonCount = byCluster.count

        // PHASE 4 — centroid-only auto-merge polish. CW with cosine ≥ 0.40
        // is already conservative; this pass catches any residual
        // fragmentation (1-photo clusters whose embeddings happen to land
        // just below the kNN threshold). Cheap insurance.
        let autoMergedSources = await tightPairAutoMerge(database: database)

        let finalPersonCount = max(0, postCWPersonCount - autoMergedSources)
        let dur = Date().timeIntervalSince(started)
        JSONLog.shared.info(ev: "face_cluster_done",
                            extra: ["persons": AnyCodable(finalPersonCount),
                                    "personsBeforeAutoMerge": AnyCodable(postCWPersonCount),
                                    "autoMerged": AnyCodable(autoMergedSources),
                                    "faces": AnyCodable(decoded.count),
                                    "unmatched": AnyCodable(unmatched),
                                    "embedder": AnyCodable(useArcFace ? "arcface" : "vision_print"),
                                    "seconds": AnyCodable(dur)])
        return FaceClusteringResult(
            personCount: finalPersonCount,
            faceCount: decoded.count,
            unmatchedFaces: unmatched,
            durationSeconds: dur
        )
    }

    // MARK: - Phase 3: centroid-only auto-merge

    /// Cap on persons considered. O(N²) pairwise centroid math; at
    /// 5000 persons this is ~12.5M comparisons of 512-d L2 — single-digit
    /// seconds in pure Swift. Above this we skip the pass to avoid a
    /// pathological wall-time hit on corrupted DBs.
    private static let autoMergePersonCap = 5000

    /// Read centroids per person, find pairs that pass the
    /// tight-or-small-cluster predicate, union-find chain them, apply
    /// in one transaction. Returns the number of source persons absorbed.
    static func tightPairAutoMerge(database: Database) async -> Int {
        struct PrintRow: Sendable { let personID: Int64; let blob: Data; let fileCount: Int }
        let rows: [PrintRow]
        do {
            rows = try await database.pool.read { db in
                let r = try GRDB.Row.fetchAll(db, sql: """
                    SELECT fp.person_id AS pid, fp.print_data AS blob,
                           p.file_count AS fc
                    FROM face_prints fp
                    INNER JOIN persons p ON p.id = fp.person_id
                    WHERE fp.person_id IS NOT NULL
                      AND LENGTH(fp.print_data) > 0
                    """)
                return r.map { PrintRow(personID: $0["pid"] ?? 0,
                                         blob: $0["blob"] ?? Data(),
                                         fileCount: $0["fc"] ?? 0) }
            }
        } catch {
            JSONLog.shared.warn(ev: "face_auto_merge_query_failed", error: "\(error)")
            return 0
        }
        guard !rows.isEmpty else { return 0 }

        // Group prints by person, decode, compute centroid.
        struct Cluster { let id: Int64; let centroid: [Float]; let fileCount: Int }
        var byPerson: [Int64: (vecs: [[Float]], fileCount: Int)] = [:]
        var firstDim = 0
        for row in rows {
            guard let v = decodePrint(row.blob) else { continue }
            if firstDim == 0 { firstDim = v.count }
            guard v.count == firstDim else { continue }
            byPerson[row.personID, default: ([], row.fileCount)].vecs.append(v)
            byPerson[row.personID]?.fileCount = row.fileCount
        }
        guard firstDim > 0, byPerson.count >= 2 else { return 0 }
        if byPerson.count > autoMergePersonCap {
            JSONLog.shared.info(ev: "face_auto_merge_skipped",
                                extra: ["persons": AnyCodable(byPerson.count),
                                        "cap": AnyCodable(autoMergePersonCap)])
            return 0
        }

        var clusters: [Cluster] = []
        clusters.reserveCapacity(byPerson.count)
        for (pid, payload) in byPerson {
            var sum = [Float](repeating: 0, count: firstDim)
            for v in payload.vecs {
                for i in 0..<firstDim { sum[i] += v[i] }
            }
            let n = Float(payload.vecs.count)
            clusters.append(Cluster(id: pid,
                                     centroid: sum.map { $0 / n },
                                     fileCount: payload.fileCount))
        }

        // O(N²) pairwise. Cluster i is "small" if file_count <= 1.
        // Auto-merge predicate:
        //   dist < tightAutoMergeL2          (always)  OR
        //   dist < smallClusterAutoMergeL2 AND (i.small OR j.small)
        var parent: [Int64: Int64] = [:]
        for c in clusters { parent[c.id] = c.id }
        func find(_ x: Int64) -> Int64 {
            var r = x
            while let p = parent[r], p != r { r = p }
            var cur = x
            while let p = parent[cur], p != r { parent[cur] = r; cur = p }
            return r
        }
        // Prefer the larger-fileCount cluster as the merge target so the
        // dominant person absorbs fragments (not the other way round).
        let countByID = Dictionary(uniqueKeysWithValues: clusters.map { ($0.id, $0.fileCount) })
        func union(_ a: Int64, _ b: Int64) {
            let ra = find(a), rb = find(b)
            if ra == rb { return }
            let ca = countByID[ra] ?? 0
            let cb = countByID[rb] ?? 0
            if ca >= cb { parent[rb] = ra } else { parent[ra] = rb }
        }

        let started = Date()
        var pairCount = 0
        for i in 0..<clusters.count {
            let ci = clusters[i]
            let smallI = ci.fileCount <= 1
            for j in (i + 1)..<clusters.count {
                let cj = clusters[j]
                let smallJ = cj.fileCount <= 1
                let d = l2(ci.centroid, cj.centroid)
                let isTight = d < tightAutoMergeL2
                let isSmallPair = d < smallClusterAutoMergeL2 && (smallI || smallJ)
                if isTight || isSmallPair {
                    union(ci.id, cj.id)
                    pairCount += 1
                }
            }
        }

        // Build per-target source list from the union-find roots.
        var byTarget: [Int64: [Int64]] = [:]
        for c in clusters {
            let root = find(c.id)
            if c.id != root { byTarget[root, default: []].append(c.id) }
        }
        guard !byTarget.isEmpty else {
            JSONLog.shared.info(ev: "face_auto_merge_done",
                                extra: ["persons": AnyCodable(clusters.count),
                                        "pairsFound": AnyCodable(0),
                                        "merged": AnyCodable(0),
                                        "seconds": AnyCodable(Date().timeIntervalSince(started))])
            return 0
        }

        // Snapshot to immutable lets so the Sendable write closure captures
        // by value (no concurrent-mutation warnings).
        let byTargetSnapshot: [(target: Int64, sources: [Int64])] =
            byTarget.map { (target: $0.key, sources: $0.value) }
        let targetIDs: [Int64] = byTargetSnapshot.map(\.target)
        let totalSources = byTargetSnapshot.reduce(0) { $0 + $1.sources.count }

        do {
            try await database.pool.write { db in
                for entry in byTargetSnapshot {
                    let target = entry.target
                    let sources = entry.sources
                    for chunk in stride(from: 0, to: sources.count, by: 500).map({
                        Array(sources[$0..<min($0 + 500, sources.count)])
                    }) {
                        let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                        var args: [DatabaseValueConvertible] = [target]
                        args.append(contentsOf: chunk.map { Int($0) })
                        try db.execute(
                            sql: "UPDATE face_prints SET person_id = ? WHERE person_id IN (\(placeholders))",
                            arguments: StatementArguments(args)
                        )
                        try db.execute(
                            sql: "DELETE FROM persons WHERE id IN (\(placeholders))",
                            arguments: StatementArguments(chunk.map { Int($0) })
                        )
                    }
                }
                for chunk in stride(from: 0, to: targetIDs.count, by: 500).map({
                    Array(targetIDs[$0..<min($0 + 500, targetIDs.count)])
                }) {
                    let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                    try db.execute(sql: """
                        UPDATE persons SET file_count = (
                            SELECT COUNT(DISTINCT file_id)
                            FROM face_prints
                            WHERE face_prints.person_id = persons.id
                        )
                        WHERE id IN (\(placeholders))
                        """, arguments: StatementArguments(chunk.map { Int($0) }))
                }
            }
        } catch {
            JSONLog.shared.error(ev: "face_auto_merge_persist_failed", error: "\(error)")
            return 0
        }

        JSONLog.shared.info(ev: "face_auto_merge_done",
                            extra: ["persons": AnyCodable(clusters.count),
                                    "pairsFound": AnyCodable(pairCount),
                                    "merged": AnyCodable(totalSources),
                                    "seconds": AnyCodable(Date().timeIntervalSince(started))])
        return totalSources
    }

    private static func l2(_ a: [Float], _ b: [Float]) -> Float {
        guard a.count == b.count else { return .infinity }
        var sum: Float = 0
        for i in 0..<a.count {
            let d = a[i] - b[i]
            sum += d * d
        }
        return sum.squareRoot()
    }

    // MARK: - Phase 1: lazy print extraction

    /// Hard cap on prints extracted per clustering run. Bounds wall time:
    /// at ~50 ms per file × 4 concurrent extractions, 5000 prints ≈ 60 s
    /// extraction phase. Re-run clustering if more prints accumulate.
    public static let maxExtractionsPerRun: Int = 5000

    /// Bounded GCD queue so we don't reproduce the inline-tagging ANE
    /// thrash that killed scan throughput. 4 concurrent Vision extractions
    /// is enough to keep ANE busy without saturating; tested safe.
    private static let extractionConcurrency = 4

    /// One face_prints row that's missing an embedding. Each row knows
    /// which of the two embeddings it still needs so `extractOneFile` only
    /// runs the work that hasn't been done — important on persisted DBs
    /// where Vision feature prints exist from earlier runs and only the
    /// new ArcFace embedding is missing.
    fileprivate struct PendingRow: Sendable {
        let id: Int64
        let bbox: String
        let path: String
        let needsVision: Bool
        let needsArcFace: Bool
    }

    /// Extract face prints for any face_prints row that's missing EITHER
    /// the legacy Vision feature print OR the ArcFace embedding. Excluded
    /// rows are skipped entirely. Idempotent.
    static func extractPendingPrints(database: Database, sink: IPCSink) async {
        let pending: [PendingRow]
        do {
            pending = try await database.pool.read { db in
                let rows = try GRDB.Row.fetchAll(db, sql: """
                    SELECT face_prints.id, face_prints.bbox,
                           files.path_text AS path,
                           CASE WHEN LENGTH(face_prints.print_data) = 0
                                THEN 1 ELSE 0 END AS needs_vision,
                           CASE WHEN LENGTH(COALESCE(face_prints.arcface_embedding, X'')) = 0
                                THEN 1 ELSE 0 END AS needs_arcface
                    FROM face_prints
                    INNER JOIN files ON files.id = face_prints.file_id
                    WHERE files.failed = 0
                      AND face_prints.excluded = 0
                      AND (LENGTH(face_prints.print_data) = 0
                           OR LENGTH(COALESCE(face_prints.arcface_embedding, X'')) = 0)
                    ORDER BY face_prints.id ASC
                    LIMIT \(maxExtractionsPerRun)
                    """)
                return rows.map { r in
                    PendingRow(id: r["id"] ?? 0,
                               bbox: r["bbox"] ?? "",
                               path: r["path"] ?? "",
                               needsVision: ((r["needs_vision"] as Int?) ?? 0) != 0,
                               needsArcFace: ((r["needs_arcface"] as Int?) ?? 0) != 0)
                }
            }
        } catch {
            JSONLog.shared.warn(ev: "face_print_pending_query_failed", error: "\(error)")
            return
        }
        guard !pending.isEmpty else {
            JSONLog.shared.info(ev: "face_print_no_pending")
            return
        }
        JSONLog.shared.info(ev: "face_print_extract_start",
                            extra: ["pending": AnyCodable(pending.count)])
        let start = Date()

        // Group rows by source file so we open each image once and run all
        // its face prints in a single Vision pass.
        var byPath: [String: [PendingRow]] = [:]
        byPath.reserveCapacity(pending.count / 3)
        for row in pending { byPath[row.path, default: []].append(row) }

        // Bounded concurrency via TaskGroup + semaphore. We use a simple
        // counting actor instead of DispatchSemaphore to avoid blocking
        // cooperative threads.
        let limiter = AsyncSemaphore(value: extractionConcurrency)
        // Each extracted face yields (id, Vision-feature-print blob, ArcFace blob).
        // ArcFace blob is nil when the .mlpackage isn't installed yet; the
        // Stage D clustering then falls back to the Vision feature print.
        let extracted: [PendingExtract] = await withTaskGroup(of: [PendingExtract].self,
                                                               returning: [PendingExtract].self) { group in
            for (path, rows) in byPath {
                group.addTask {
                    await limiter.wait()
                    defer { Task { await limiter.signal() } }
                    return await Self.extractOneFile(path: path, rows: rows)
                }
            }
            var out: [PendingExtract] = []
            for await chunk in group { out.append(contentsOf: chunk) }
            return out
        }

        // Persist extracted prints in chunks. Only update columns we
        // actually computed for each row — leaves existing values alone
        // when the row already had them.
        let extractedSnapshot = extracted   // Sendable capture
        let arcFaceCount = extractedSnapshot.reduce(0) { $0 + ($1.arcFace != nil ? 1 : 0) }
        let visionCount = extractedSnapshot.reduce(0) { $0 + ($1.visionPrint != nil ? 1 : 0) }
        do {
            try await database.pool.write { db in
                for face in extractedSnapshot {
                    switch (face.visionPrint, face.arcFace) {
                    case let (vp?, af?):
                        try db.execute(
                            sql: "UPDATE face_prints SET print_data = ?, arcface_embedding = ? WHERE id = ?",
                            arguments: [vp, af, face.id]
                        )
                    case let (vp?, nil):
                        try db.execute(
                            sql: "UPDATE face_prints SET print_data = ? WHERE id = ?",
                            arguments: [vp, face.id]
                        )
                    case let (nil, af?):
                        try db.execute(
                            sql: "UPDATE face_prints SET arcface_embedding = ? WHERE id = ?",
                            arguments: [af, face.id]
                        )
                    case (nil, nil):
                        break
                    }
                }
            }
            JSONLog.shared.info(ev: "face_print_extract_done",
                                extra: ["pending": AnyCodable(pending.count),
                                        "extracted": AnyCodable(extractedSnapshot.count),
                                        "vision_extracted": AnyCodable(visionCount),
                                        "arcface_extracted": AnyCodable(arcFaceCount),
                                        "files": AnyCodable(byPath.count),
                                        "seconds": AnyCodable(Date().timeIntervalSince(start))])
        } catch {
            JSONLog.shared.error(ev: "face_print_persist_failed", error: "\(error)")
            await sink.emit(.error(EngineError(
                kind: "face_print_persist_failed",
                message: "Could not persist extracted prints: \(error)"
            )))
        }
    }

    /// Open one image and produce only the embeddings each row is missing.
    /// Vision feature print runs only for rows where `needsVision == true`;
    /// ArcFace runs only for rows where `needsArcFace == true` AND the
    /// .mlpackage is loaded. Always saves a face crop for downstream VLM
    /// use (idempotent on disk).
    private static func extractOneFile(
        path: String, rows: [PendingRow]
    ) async -> [PendingExtract] {
        return await withCheckedContinuation { (cont: CheckedContinuation<[PendingExtract], Never>) in
            DispatchQueue.global(qos: .userInitiated).async {
                let result = autoreleasepool { () -> [PendingExtract] in
                    let url = URL(fileURLWithPath: path)
                    guard let cg = loadCGImage(url: url) else { return [] }

                    // Run Vision feature-print extraction only for rows
                    // that need it. Aligned by index with `visionRows`.
                    let visionRows = rows.filter { $0.needsVision }
                    var visionByID: [Int64: Data] = [:]
                    if !visionRows.isEmpty {
                        let handler = VNImageRequestHandler(cgImage: cg, options: [:])
                        var requests: [VNGenerateImageFeaturePrintRequest] = []
                        requests.reserveCapacity(visionRows.count)
                        for row in visionRows {
                            guard let roi = parseBBox(row.bbox) else { continue }
                            let req = VNGenerateImageFeaturePrintRequest()
                            req.imageCropAndScaleOption = .scaleFill
                            req.regionOfInterest = roi
                            requests.append(req)
                        }
                        if !requests.isEmpty {
                            do { try handler.perform(requests) } catch { /* best-effort */ }
                            for (i, req) in requests.enumerated() {
                                guard i < visionRows.count, let fp = req.results?.first else { continue }
                                if let data = try? NSKeyedArchiver.archivedData(
                                    withRootObject: fp, requiringSecureCoding: true
                                ) {
                                    visionByID[visionRows[i].id] = data
                                }
                            }
                        }
                    }

                    let arcFaceReady = ArcFaceService.shared.isReady
                    var out: [PendingExtract] = []
                    out.reserveCapacity(rows.count)
                    for row in rows {
                        // Crop once per face; saves the JPEG (idempotent)
                        // and feeds ArcFace if needed.
                        let crop = cropFaceCGImage(cgImage: cg, bboxString: row.bbox)
                        if let crop {
                            saveFaceCrop(faceID: row.id, croppedCGImage: crop)
                        }
                        var arcFaceBlob: Data? = nil
                        if row.needsArcFace, arcFaceReady, let crop,
                           let vec = ArcFaceService.shared.embed(crop) {
                            arcFaceBlob = ArcFaceService.embeddingToBlob(vec)
                        }
                        let visionBlob = visionByID[row.id]
                        // Skip rows where neither computation produced
                        // anything new — no DB write needed.
                        if visionBlob == nil && arcFaceBlob == nil { continue }
                        out.append(PendingExtract(id: row.id,
                                                  visionPrint: visionBlob,
                                                  arcFace: arcFaceBlob))
                    }
                    return out
                }
                cont.resume(returning: result)
            }
        }
    }

    fileprivate struct PendingExtract: Sendable {
        let id: Int64
        let visionPrint: Data?     // nil = the row already had this; don't overwrite
        let arcFace: Data?         // nil = the row already had this OR model unavailable
    }

    /// Crop the bbox region (with padding) out of the source CGImage and
    /// return the cropped CGImage. Vision bboxes are normalized with
    /// bottom-left origin; CGImage cropping uses top-left, so we flip Y.
    /// Returns nil if the resulting crop is too small to be useful.
    static func cropFaceCGImage(cgImage: CGImage, bboxString: String) -> CGImage? {
        guard let roi = parseBBox(bboxString) else { return nil }
        let imgW = CGFloat(cgImage.width)
        let imgH = CGFloat(cgImage.height)
        let pixelRect = CGRect(
            x: roi.origin.x * imgW,
            y: (1.0 - roi.origin.y - roi.size.height) * imgH,
            width: roi.size.width * imgW,
            height: roi.size.height * imgH
        ).integral
        guard pixelRect.width >= 32, pixelRect.height >= 32 else { return nil }
        return cgImage.cropping(to: pixelRect)
    }

    /// Save a pre-cropped face CGImage as a JPEG to face_crops/<id>.jpg.
    /// Idempotent — overwrites if the file already exists.
    private static func saveFaceCrop(faceID: Int64, croppedCGImage cropped: CGImage) {
        let url = faceCropURL(faceID: faceID)
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        guard let dest = CGImageDestinationCreateWithURL(
            url as CFURL, "public.jpeg" as CFString, 1, nil
        ) else { return }
        // 0.85 quality — good enough for VLM face matching, ~5-15 KB/face.
        let options: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: 0.85]
        CGImageDestinationAddImage(dest, cropped, options as CFDictionary)
        CGImageDestinationFinalize(dest)
    }

    /// Path on disk for a given face_prints row's crop JPEG.
    public static func faceCropURL(faceID: Int64) -> URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID/face_crops", isDirectory: true)
            .appendingPathComponent("\(faceID).jpg")
    }

    /// Parse "x,y,w,h" normalized → CGRect with 15% padding (matches the
    /// historical v1 padding for face prints).
    private static func parseBBox(_ s: String) -> CGRect? {
        let parts = s.split(separator: ",").compactMap { Double($0) }
        guard parts.count == 4 else { return nil }
        let pad: CGFloat = 0.15
        let bx = CGFloat(parts[0]); let by = CGFloat(parts[1])
        let bw = CGFloat(parts[2]); let bh = CGFloat(parts[3])
        let x = max(0, bx - bw * pad)
        let y = max(0, by - bh * pad)
        let w = min(1 - x, bw * (1 + 2 * pad))
        let h = min(1 - y, bh * (1 + 2 * pad))
        guard w > 0.001, h > 0.001 else { return nil }
        return CGRect(x: x, y: y, width: w, height: h)
    }

    /// Higher-resolution loader than `Tagging.loadCGImage`. We need
    /// 2048px (vs the per-file scan's 512px) so that when we crop a
    /// face out of the source for VLM comparison, the face is large
    /// enough for Qwen to make a confident verdict. A face at 10% of
    /// the image is ~50px at 512 (unusable) but ~200px at 2048 (great).
    private static func loadCGImage(url: URL) -> CGImage? {
        if let attrs = try? FileManager.default.attributesOfItem(atPath: url.path),
           let size = attrs[.size] as? Int, size < 256 {
            return nil
        }
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        let opts: [CFString: Any] = [
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceCreateThumbnailFromImageIfAbsent: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 2048
        ]
        return CGImageSourceCreateThumbnailAtIndex(src, 0, opts as CFDictionary)
    }

    // MARK: - Decoding

    /// Decode a face_prints.print_data BLOB back to a Float vector. Returns
    /// nil if the data isn't a valid VNFeaturePrintObservation archive or
    /// the embedded vector is too small/large to be sane.
    private static func decodePrint(_ data: Data) -> [Float]? {
        guard let obs = try? NSKeyedUnarchiver.unarchivedObject(
            ofClass: VNFeaturePrintObservation.self, from: data
        ) else { return nil }
        let count = obs.elementCount
        // Vision face prints are 512-d for the current revision. Sanity-clamp.
        guard count >= 64, count <= 4096 else { return nil }
        let requiredBytes = count * MemoryLayout<Float>.size
        guard obs.data.count >= requiredBytes else { return nil }
        var out = [Float](repeating: 0, count: count)
        obs.data.withUnsafeBytes { ptr in
            let fp = ptr.bindMemory(to: Float.self)
            let n = min(count, fp.count)
            for i in 0..<n { out[i] = fp[i] }
        }
        return out
    }
}
