// Face clustering — runs after each scan and on demand.
//
// Phase 1 — extract ArcFace embeddings lazily for any face_prints rows
//           that only have a bbox + crop. Bounded concurrency avoids
//           ANE thrash.
// Phase 2 — load every embedded, non-excluded row.
// Phase 3 — IdentityClustering: two-pass density + Pass 3 quality
//           validation. Replaces Chinese Whispers.
// Phase 4 — persist persons + face_prints.person_id assignments.
// Phase 5 — tightPairAutoMerge centroid polish on ArcFace cosine.
//
// Memory: ~2 KB per ArcFace embedding; 50K faces ≈ 100 MB peak.
import Foundation
import GRDB
import ImageIO
import CoreGraphics
import FileIDShared

public enum FaceClustering {

    /// Centroid cosine ≥ this triggers auto-merge regardless of cluster
    /// size — same person with very high confidence. Compare to ArcFace
    /// verification literature: 0.40 is the FAR=10⁻⁴ threshold for
    /// individual face pairs; centroid-to-centroid is denoised, so we
    /// can pull this stricter without losing recall.
    public static let tightAutoMergeCos: Float = 0.65

    /// Looser cosine threshold used only when at least one cluster is
    /// a single face — those are almost always fragments of an existing
    /// person, not a distinct identity.
    public static let smallClusterAutoMergeCos: Float = 0.55

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

        // ArcFace is a hard requirement — no Vision-print fallback. Vision
        // feature prints aren't face-identity-trained; clustering on them
        // produces mega-clusters at scale (the bug we're fixing). If the
        // model isn't installed we surface an actionable error and exit.
        if !ArcFaceService.shared.isReady {
            for kind in FaceEmbedderKind.installedKinds() {
                _ = ArcFaceService.shared.load(kind)
                break
            }
        }
        guard ArcFaceService.shared.isReady else {
            JSONLog.shared.warn(ev: "face_cluster_skipped_no_model",
                                error: "ArcFace model not installed; cannot cluster.")
            await sink.emit(.error(EngineError(
                kind: "face_cluster_no_model",
                message: "Face-recognition model not installed. Open Settings → AI Models — face recognition to install ArcFace iResNet50 (166 MB) or MobileFace (13 MB)."
            )))
            return FaceClusteringResult(personCount: 0, faceCount: 0,
                                        unmatchedFaces: 0,
                                        durationSeconds: Date().timeIntervalSince(started))
        }

        // PHASE 0 — snapshot prior anchors. Two reasons to do this BEFORE
        // extraction + clustering: (1) we filter unknown-person face_ids
        // out of both extraction and clustering pools so they're not
        // re-embedded or re-assigned to named clusters; (2) the inheritance
        // logic later in PHASE 4 reads the same snapshot.
        let priorAnchors = await snapshotPriorAnchors(database: database)
        let unknownFaceIDs: Set<Int64> = Set(
            priorAnchors.filter { $0.isUnknown }.flatMap { $0.faceIDs }
        )

        // PHASE 1 — extract any pending ArcFace embeddings. Idempotent.
        await extractPendingPrints(database: database, sink: sink,
                                    skipFaceIDs: unknownFaceIDs)

        // PHASE 2 — load every face_prints row with an ArcFace embedding
        // and not excluded by the quality filter. Unknown-person face_ids
        // are filtered out: the user explicitly said "don't cluster these",
        // so they stay attached to their existing unknown person row and
        // never participate in a re-cluster pass.
        struct FaceRow: Sendable { let id: Int64; let arcFace: Data }
        let rows: [FaceRow]
        do {
            rows = try await database.pool.read { db in
                let r = try GRDB.Row.fetchAll(db, sql: """
                    SELECT id, arcface_embedding
                    FROM face_prints
                    WHERE excluded = 0
                      AND LENGTH(arcface_embedding) > 0
                    ORDER BY id ASC LIMIT \(maxFacesPerRun)
                    """)
                return r.compactMap { row -> FaceRow? in
                    let id: Int64 = row["id"] ?? 0
                    if unknownFaceIDs.contains(id) { return nil }
                    return FaceRow(id: id,
                                   arcFace: row["arcface_embedding"] ?? Data())
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

        struct DecodedFace { let id: Int64; let vec: [Float] }
        var decoded: [DecodedFace] = []
        decoded.reserveCapacity(rows.count)
        for row in rows {
            let vec = ArcFaceService.blobToEmbedding(row.arcFace)
            if !vec.isEmpty {
                decoded.append(DecodedFace(id: row.id, vec: vec))
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
                                    "dim": AnyCodable(firstDim)])

        // PHASE 3 — IdentityClustering: two-pass density + Pass 3 quality
        // validation. Pass 1 forms tight identity cores at cosine ≥ 0.55;
        // Pass 2 assigns outliers with a margin rule preventing bridge-face
        // collapse; Pass 3 splits any cluster whose intra-cluster variance
        // exceeds 0.05 or mean cosine to centroid drops below 0.50.
        //
        // HNSW supplies the kNN graph. Insert order = dense node index.
        // HNSW returns L2 distances; for L2-normalized embeddings
        // cosine_sim = 1 - L2²/2.
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

        let icParams = IdentityClustering.Hyperparameters()
        let icResult = IdentityClustering.cluster(
            embeddings: vecsByDense,
            searcher: { idx -> [(neighbor: Int, similarity: Float)] in
                let hits = index.search(vecsByDense[idx], k: icParams.kNN + 1)
                return hits.compactMap { (rawID, l2dist) -> (neighbor: Int, similarity: Float)? in
                    let nID = Int(rawID)
                    guard nID >= 0 && nID < n && nID != idx else { return nil }
                    let cosine = 1.0 - (l2dist * l2dist) / 2.0
                    return (neighbor: nID, similarity: cosine)
                }
            },
            params: icParams
        )

        // Group dense nodes by cluster id (IdentityClustering returns
        // dense IDs from 0).
        var byCluster: [Int: [Int]] = [:]
        for (denseIdx, cid) in icResult.clusterIDs.enumerated() {
            byCluster[cid, default: []].append(denseIdx)
        }
        // Cap at maxPersons (catches DB corruption, not normal libraries).
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
                                error: "IdentityClustering produced \(byCluster.count + truncatedAtCap) clusters > maxPersons (\(maxPersons)); \(truncatedAtCap) faces unclustered.")
        }

        JSONLog.shared.info(ev: "face_cluster_built",
                            extra: ["faces": AnyCodable(decoded.count),
                                    "clusters": AnyCodable(byCluster.count),
                                    "cores": AnyCodable(icResult.coreCount),
                                    "outliersAssigned": AnyCodable(icResult.outliersAssigned),
                                    "outliersAsSingletons": AnyCodable(icResult.outliersAsSingletons),
                                    "splitsApplied": AnyCodable(icResult.splitsApplied),
                                    "unmatched": AnyCodable(unmatched),
                                    "buildSeconds": AnyCodable(icResult.durationSeconds)])

        // PHASE 4 — Compute new-cluster centroids + anchor radii, snapshot
        // prior anchors, match new clusters to old ones, persist with
        // inherited names. Identity persistence — names survive re-clustering.
        struct ClusterPersist: Sendable {
            let repFaceID: Int64
            let faceIDs: [Int64]
            let count: Int
            let centroid: [Float]
            let anchorRadius: Float
            let inherited: PriorAnchorMatch?
        }
        let nextClusters: [(centroid: [Float], radius: Float, faceIDs: [Int64], denseIdxs: [Int])] =
            byCluster.values.map { denseIdxs in
                let centroid = computeNormalizedCentroid(
                    denseIdxs: denseIdxs, vecsByDense: vecsByDense, dim: firstDim
                )
                let radius = computeAnchorRadius(
                    denseIdxs: denseIdxs, vecsByDense: vecsByDense, centroid: centroid
                )
                return (centroid, radius,
                        denseIdxs.map { denseToFaceID[$0] }, denseIdxs)
            }

        // priorAnchors was loaded in PHASE 0; reuse it here. Unknown anchors
        // are excluded from name-inheritance matching: their face_ids weren't
        // in the clustering pool, so their faces stay attached to the
        // existing unknown person row. Wave-2 cosine matching could
        // otherwise transfer is_unknown=true onto a new cluster whose
        // centroid happened to land near the unknown anchor's centroid.
        let inheritanceCandidates = priorAnchors.filter { !$0.isUnknown }
        let matches = matchClustersToPriorAnchors(
            newClusters: nextClusters.map { ($0.centroid, $0.faceIDs) },
            priorAnchors: inheritanceCandidates
        )
        let priorsWithNames = inheritanceCandidates.filter { $0.hasName }.count
        let claimedPriorIDs = Set(matches.compactMap { $0?.priorPersonID })
        let lostAnchorCount = max(0, priorsWithNames - claimedPriorIDs.count)
        let unknownPriorIDs = priorAnchors.filter { $0.isUnknown }.map { $0.id }

        let personsList: [ClusterPersist] = nextClusters.enumerated().map { idx, c in
            ClusterPersist(
                repFaceID: c.faceIDs.first ?? 0,
                faceIDs: c.faceIDs,
                count: c.faceIDs.count,
                centroid: c.centroid,
                anchorRadius: c.radius,
                inherited: matches[idx]
            )
        }

        do {
            try await database.pool.write { db in
                // Preserve unknown persons in place. Their face_ids stay
                // bound to their existing row; only non-unknown rows get
                // wiped + recreated.
                if unknownPriorIDs.isEmpty {
                    try db.execute(sql: "UPDATE face_prints SET person_id = NULL")
                    try db.execute(sql: "DELETE FROM persons")
                } else {
                    let placeholders = unknownPriorIDs.map { _ in "?" }.joined(separator: ",")
                    let unknownArgs = StatementArguments(unknownPriorIDs.map { Int($0) })
                    try db.execute(
                        sql: """
                            UPDATE face_prints SET person_id = NULL
                            WHERE person_id IS NULL OR person_id NOT IN (\(placeholders))
                            """,
                        arguments: unknownArgs
                    )
                    try db.execute(
                        sql: "DELETE FROM persons WHERE id NOT IN (\(placeholders))",
                        arguments: unknownArgs
                    )
                }

                let now = Date().timeIntervalSinceReferenceDate
                for p in personsList {
                    let blob = ArcFaceService.embeddingToBlob(p.centroid)
                    let inherited = p.inherited
                    try db.execute(sql: """
                        INSERT INTO persons (
                            name, representative_face_id, file_count, created_at,
                            title, first_name, middle_name, last_name, suffix, is_unknown,
                            centroid, anchor_radius, last_clustered_at
                        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                        """, arguments: [
                            inherited?.legacyName,
                            p.repFaceID, p.count, now,
                            inherited?.title,
                            inherited?.firstName,
                            inherited?.middleName,
                            inherited?.lastName,
                            inherited?.suffix,
                            inherited?.isUnknown == true ? 1 : 0,
                            blob,
                            Double(p.anchorRadius),
                            now
                        ])
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
        let prePolishPersonCount = byCluster.count
        let inheritedCount = matches.compactMap { $0 }.count
        if inheritedCount > 0 || lostAnchorCount > 0 {
            JSONLog.shared.info(ev: "face_cluster_anchor_match",
                                extra: ["priors": AnyCodable(priorAnchors.count),
                                        "inherited": AnyCodable(inheritedCount),
                                        "lostNames": AnyCodable(lostAnchorCount)])
        }

        // PHASE 4 — centroid-only auto-merge polish. CW with cosine ≥ 0.40
        // is already conservative; this pass catches any residual
        // fragmentation (1-photo clusters whose embeddings happen to land
        // just below the kNN threshold). Cheap insurance.
        let autoMergedSources = await tightPairAutoMerge(database: database)

        let finalPersonCount = max(0, prePolishPersonCount - autoMergedSources)
        let dur = Date().timeIntervalSince(started)
        JSONLog.shared.info(ev: "face_cluster_done",
                            extra: ["persons": AnyCodable(finalPersonCount),
                                    "personsBeforeAutoMerge": AnyCodable(prePolishPersonCount),
                                    "autoMerged": AnyCodable(autoMergedSources),
                                    "faces": AnyCodable(decoded.count),
                                    "unmatched": AnyCodable(unmatched),
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

    /// Read ArcFace embeddings per person, build L2-normalized centroids,
    /// find centroid pairs above the cosine cutoff, union-find chain them,
    /// apply in one transaction. Returns the number of source persons
    /// absorbed. Uses ArcFace cosine consistently with the primary
    /// clustering pass — no embedding-space mismatch.
    static func tightPairAutoMerge(database: Database) async -> Int {
        struct PrintRow: Sendable { let personID: Int64; let blob: Data; let fileCount: Int; let named: Bool }
        let rows: [PrintRow]
        do {
            rows = try await database.pool.read { db in
                // `named` = the user gave this cluster an identity (structured
                // or legacy name) and didn't mark it unknown. Used below to
                // protect user-named persons from being absorbed+deleted.
                let r = try GRDB.Row.fetchAll(db, sql: """
                    SELECT fp.person_id AS pid, fp.arcface_embedding AS blob,
                           p.file_count AS fc,
                           (p.is_unknown = 0 AND (COALESCE(p.first_name,'') != ''
                             OR COALESCE(p.last_name,'') != ''
                             OR COALESCE(p.name,'') != '')) AS named
                    FROM face_prints fp
                    INNER JOIN persons p ON p.id = fp.person_id
                    WHERE fp.person_id IS NOT NULL
                      AND LENGTH(fp.arcface_embedding) > 0
                    """)
                return r.map { PrintRow(personID: $0["pid"] ?? 0,
                                         blob: $0["blob"] ?? Data(),
                                         fileCount: $0["fc"] ?? 0,
                                         named: ($0["named"] ?? 0) != 0) }
            }
        } catch {
            JSONLog.shared.warn(ev: "face_auto_merge_query_failed", error: "\(error)")
            return 0
        }
        guard !rows.isEmpty else { return 0 }

        // Group embeddings by person, build L2-normalized centroid.
        struct Cluster { let id: Int64; let centroid: [Float]; let fileCount: Int; let named: Bool }
        var byPerson: [Int64: (vecs: [[Float]], fileCount: Int, named: Bool)] = [:]
        var firstDim = 0
        for row in rows {
            let v = ArcFaceService.blobToEmbedding(row.blob)
            guard !v.isEmpty else { continue }
            if firstDim == 0 { firstDim = v.count }
            guard v.count == firstDim else { continue }
            byPerson[row.personID, default: ([], row.fileCount, row.named)].vecs.append(v)
            byPerson[row.personID]?.fileCount = row.fileCount
            byPerson[row.personID]?.named = row.named
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
            // L2-normalize so cosine = dot product downstream.
            var norm: Float = 0
            for x in sum { norm += x * x }
            let invN = Float(1) / max(.leastNonzeroMagnitude, norm.squareRoot())
            for i in 0..<firstDim { sum[i] *= invN }
            clusters.append(Cluster(id: pid, centroid: sum,
                                     fileCount: payload.fileCount,
                                     named: payload.named))
        }

        // O(N²) pairwise cosine. Cluster i is "small" if file_count <= 1.
        // Auto-merge predicate:
        //   cos(ci, cj) >= tightAutoMergeCos                    (always)  OR
        //   cos(ci, cj) >= smallClusterAutoMergeCos AND (i.small OR j.small)
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
        // dominant person absorbs fragments — UNLESS exactly one side is
        // user-named, in which case the named person is always the target so
        // its name (and row) survive the merge.
        let countByID = Dictionary(uniqueKeysWithValues: clusters.map { ($0.id, $0.fileCount) })
        let namedByID = Dictionary(uniqueKeysWithValues: clusters.map { ($0.id, $0.named) })
        func union(_ a: Int64, _ b: Int64) {
            let ra = find(a), rb = find(b)
            if ra == rb { return }
            let namedA = namedByID[ra] ?? false
            let namedB = namedByID[rb] ?? false
            if namedA != namedB {
                // Exactly one named — it absorbs the other.
                if namedA { parent[rb] = ra } else { parent[ra] = rb }
                return
            }
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
                // Never auto-merge two separately user-named persons — that
                // would silently override an explicit naming decision and
                // delete one of the names. Leave them for manual merge.
                if ci.named && cj.named { continue }
                let smallJ = cj.fileCount <= 1
                let cos = dotProduct(ci.centroid, cj.centroid)
                let isTight = cos >= tightAutoMergeCos
                let isSmallPair = cos >= smallClusterAutoMergeCos && (smallI || smallJ)
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

    @inline(__always)
    private static func dotProduct(_ a: [Float], _ b: [Float]) -> Float {
        let n = min(a.count, b.count)
        var s: Float = 0
        for i in 0..<n { s += a[i] * b[i] }
        return s
    }

    // MARK: - Phase 4: identity persistence (anchors)

    /// Names + anchor-match metadata transferred from a prior person to
    /// a new cluster.
    fileprivate struct PriorAnchorMatch: Sendable {
        let priorPersonID: Int64
        let title: String?
        let firstName: String?
        let middleName: String?
        let lastName: String?
        let suffix: String?
        let legacyName: String?
        let isUnknown: Bool
    }

    /// Snapshot of an existing persons row + the face_ids that were
    /// assigned to it in the prior clustering run. Drives name
    /// inheritance after a re-cluster.
    fileprivate struct PriorAnchor: Sendable {
        let id: Int64
        let centroid: [Float]?
        let anchorRadius: Float?
        let faceIDs: Set<Int64>
        let title: String?
        let firstName: String?
        let middleName: String?
        let lastName: String?
        let suffix: String?
        let legacyName: String?
        let isUnknown: Bool
        var hasName: Bool {
            isUnknown ||
            !(title ?? "").isEmpty ||
            !(firstName ?? "").isEmpty ||
            !(middleName ?? "").isEmpty ||
            !(lastName ?? "").isEmpty ||
            !(suffix ?? "").isEmpty ||
            !(legacyName ?? "").isEmpty
        }
    }

    /// L2-normalized mean of the embeddings indexed by `denseIdxs`.
    fileprivate static func computeNormalizedCentroid(
        denseIdxs: [Int], vecsByDense: [[Float]], dim: Int
    ) -> [Float] {
        var sum = [Float](repeating: 0, count: dim)
        for idx in denseIdxs {
            let v = vecsByDense[idx]
            for d in 0..<dim { sum[d] += v[d] }
        }
        var norm: Float = 0
        for d in 0..<dim { norm += sum[d] * sum[d] }
        let invN = Float(1) / max(.leastNonzeroMagnitude, norm.squareRoot())
        for d in 0..<dim { sum[d] *= invN }
        return sum
    }

    /// 10th percentile cosine sim from cluster members to centroid,
    /// clamped to [0.45, 0.85]. Singleton clusters use a default 0.50.
    /// See plan: anchor radius is the cosine threshold at which we
    /// believe a new face/centroid likely IS this person.
    fileprivate static func computeAnchorRadius(
        denseIdxs: [Int], vecsByDense: [[Float]], centroid: [Float]
    ) -> Float {
        guard denseIdxs.count >= 2 else { return 0.50 }
        var sims: [Float] = []
        sims.reserveCapacity(denseIdxs.count)
        for idx in denseIdxs {
            sims.append(dotProduct(vecsByDense[idx], centroid))
        }
        sims.sort()
        // 10th percentile = the least-typical member's similarity.
        let p10Index = max(0, Int((Float(sims.count) * 0.10).rounded(.down)))
        let raw = sims[p10Index]
        return min(0.85, max(0.45, raw))
    }

    /// Read every existing persons row + its face_id set + any prior
    /// anchor data. Called BEFORE we wipe the persons table.
    fileprivate static func snapshotPriorAnchors(database: Database) async -> [PriorAnchor] {
        do {
            return try await database.pool.read { db in
                let personRows = try GRDB.Row.fetchAll(db, sql: """
                    SELECT id, centroid, anchor_radius, title, first_name,
                           middle_name, last_name, suffix, name, is_unknown
                    FROM persons
                    """)
                let faceRows = try GRDB.Row.fetchAll(db, sql: """
                    SELECT id, person_id FROM face_prints
                    WHERE person_id IS NOT NULL
                    """)
                var faceIDsByPerson: [Int64: Set<Int64>] = [:]
                for r in faceRows {
                    let pid: Int64 = r["person_id"] ?? 0
                    let fid: Int64 = r["id"] ?? 0
                    if pid != 0 && fid != 0 {
                        faceIDsByPerson[pid, default: []].insert(fid)
                    }
                }
                return personRows.map { r -> PriorAnchor in
                    let pid: Int64 = r["id"] ?? 0
                    let centroid: [Float]? = (r["centroid"] as Data?).flatMap { blob in
                        let v = ArcFaceService.blobToEmbedding(blob)
                        return v.isEmpty ? nil : v
                    }
                    let radius: Float? = (r["anchor_radius"] as Double?).map { Float($0) }
                    let isUnknownInt: Int = r["is_unknown"] ?? 0
                    return PriorAnchor(
                        id: pid,
                        centroid: centroid,
                        anchorRadius: radius,
                        faceIDs: faceIDsByPerson[pid] ?? [],
                        title: r["title"], firstName: r["first_name"],
                        middleName: r["middle_name"], lastName: r["last_name"],
                        suffix: r["suffix"], legacyName: r["name"],
                        isUnknown: isUnknownInt != 0
                    )
                }
            }
        } catch {
            JSONLog.shared.warn(ev: "face_cluster_anchor_snapshot_failed",
                                error: "\(error)")
            return []
        }
    }

    /// For each new cluster, find the prior person it should inherit
    /// names from. Two-wave matching:
    ///
    ///   Wave 1 (face-id overlap): when the SAME library is re-clustered,
    ///     most face_ids carry over. Prior persons match the new cluster
    ///     containing the most of their face_ids, requiring overlap
    ///     ≥ 50% of the prior's face count. Highest priority.
    ///
    ///   Wave 2 (centroid cosine): for any prior with a stored anchor
    ///     centroid that didn't match by face IDs (e.g. the user added
    ///     entirely new photos), match new clusters whose centroid is
    ///     within the prior's anchor_radius cosine. Lower priority.
    ///
    /// Each prior person matches at most one new cluster; each new
    /// cluster gets at most one inherited identity. Conflicts resolve
    /// by larger overlap / higher cosine.
    fileprivate static func matchClustersToPriorAnchors(
        newClusters: [(centroid: [Float], faceIDs: [Int64])],
        priorAnchors: [PriorAnchor]
    ) -> [PriorAnchorMatch?] {
        var matches: [PriorAnchorMatch?] = Array(repeating: nil, count: newClusters.count)
        guard !priorAnchors.isEmpty else { return matches }

        // Only persons with a structured-name field set (or marked unknown)
        // are worth inheriting. Empty rows just bloat conflict resolution.
        let candidates = priorAnchors.filter { $0.hasName }
        guard !candidates.isEmpty else { return matches }

        let newFaceSets: [Set<Int64>] = newClusters.map { Set($0.faceIDs) }

        // Wave 1: face-id overlap. Each candidate scores all new clusters.
        var claimedByPrior = [Int64: Int]()  // priorID → newClusterIndex
        var claimedByCluster = [Int: Int64]() // newClusterIndex → priorID
        var bestOverlap = [Int: Int]()        // newClusterIndex → overlap count
        for prior in candidates where !prior.faceIDs.isEmpty {
            var bestIdx = -1
            var bestCount = 0
            for (idx, faceSet) in newFaceSets.enumerated() {
                let overlap = prior.faceIDs.intersection(faceSet).count
                if overlap > bestCount { bestCount = overlap; bestIdx = idx }
            }
            // Require ≥ 50% of the prior's faces in this cluster.
            let threshold = max(1, prior.faceIDs.count / 2)
            guard bestIdx >= 0, bestCount >= threshold else { continue }
            // Conflict: another prior already claimed this cluster?
            if let otherPriorID = claimedByCluster[bestIdx] {
                let otherOverlap = bestOverlap[bestIdx] ?? 0
                if bestCount > otherOverlap {
                    claimedByPrior.removeValue(forKey: otherPriorID)
                    claimedByPrior[prior.id] = bestIdx
                    claimedByCluster[bestIdx] = prior.id
                    bestOverlap[bestIdx] = bestCount
                }
            } else {
                claimedByPrior[prior.id] = bestIdx
                claimedByCluster[bestIdx] = prior.id
                bestOverlap[bestIdx] = bestCount
            }
        }

        // Wave 2: centroid cosine for unclaimed priors with stored anchors.
        for prior in candidates where claimedByPrior[prior.id] == nil {
            guard let priorCentroid = prior.centroid else { continue }
            let radius = prior.anchorRadius ?? 0.50
            var bestIdx = -1
            var bestSim: Float = -2
            for (idx, c) in newClusters.enumerated() where claimedByCluster[idx] == nil {
                let s = dotProduct(priorCentroid, c.centroid)
                if s >= radius && s > bestSim { bestSim = s; bestIdx = idx }
            }
            if bestIdx >= 0 {
                claimedByPrior[prior.id] = bestIdx
                claimedByCluster[bestIdx] = prior.id
            }
        }

        // Materialize matches.
        let priorByID = Dictionary(uniqueKeysWithValues: candidates.map { ($0.id, $0) })
        for (clusterIdx, priorID) in claimedByCluster {
            guard let prior = priorByID[priorID] else { continue }
            matches[clusterIdx] = PriorAnchorMatch(
                priorPersonID: prior.id,
                title: prior.title,
                firstName: prior.firstName,
                middleName: prior.middleName,
                lastName: prior.lastName,
                suffix: prior.suffix,
                legacyName: prior.legacyName,
                isUnknown: prior.isUnknown
            )
        }
        return matches
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

    /// One face_prints row that's missing its ArcFace embedding.
    fileprivate struct PendingRow: Sendable {
        let id: Int64
        let bbox: String
        let path: String
    }

    /// Extract ArcFace embeddings for any face_prints row that's missing
    /// one. Excluded rows are skipped entirely. `skipFaceIDs` lets callers
    /// pass the face_ids of unknown-person rows so we don't waste ANE
    /// inference on faces the user has explicitly opted out of clustering.
    /// Idempotent. Skips work silently if the model isn't loaded —
    /// runClustering surfaces that upstream.
    static func extractPendingPrints(
        database: Database, sink: IPCSink,
        skipFaceIDs: Set<Int64> = []
    ) async {
        guard ArcFaceService.shared.isReady else { return }
        let pending: [PendingRow]
        do {
            pending = try await database.pool.read { db in
                let rows = try GRDB.Row.fetchAll(db, sql: """
                    SELECT face_prints.id, face_prints.bbox,
                           files.path_text AS path
                    FROM face_prints
                    INNER JOIN files ON files.id = face_prints.file_id
                    WHERE files.failed = 0
                      AND face_prints.excluded = 0
                      AND LENGTH(COALESCE(face_prints.arcface_embedding, X'')) = 0
                    ORDER BY face_prints.id ASC
                    LIMIT \(maxExtractionsPerRun)
                    """)
                return rows.compactMap { r -> PendingRow? in
                    let id: Int64 = r["id"] ?? 0
                    if skipFaceIDs.contains(id) { return nil }
                    return PendingRow(id: id,
                                       bbox: r["bbox"] ?? "",
                                       path: r["path"] ?? "")
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

        // Group rows by source file so we open each image once for all
        // of its faces.
        var byPath: [String: [PendingRow]] = [:]
        byPath.reserveCapacity(pending.count / 3)
        for row in pending { byPath[row.path, default: []].append(row) }

        let limiter = AsyncSemaphore(value: extractionConcurrency)
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

        let extractedSnapshot = extracted   // Sendable capture
        do {
            try await database.pool.write { db in
                for face in extractedSnapshot {
                    try db.execute(
                        sql: "UPDATE face_prints SET arcface_embedding = ? WHERE id = ?",
                        arguments: [face.arcFace, face.id]
                    )
                }
            }
            JSONLog.shared.info(ev: "face_print_extract_done",
                                extra: ["pending": AnyCodable(pending.count),
                                        "extracted": AnyCodable(extractedSnapshot.count),
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

    /// Open one image, crop each requested face, run ArcFace on each crop.
    /// Always saves a face crop JPEG for downstream VLM use (idempotent
    /// on disk).
    private static func extractOneFile(
        path: String, rows: [PendingRow]
    ) async -> [PendingExtract] {
        return await withCheckedContinuation { (cont: CheckedContinuation<[PendingExtract], Never>) in
            DispatchQueue.global(qos: .userInitiated).async {
                let result = autoreleasepool { () -> [PendingExtract] in
                    let url = URL(fileURLWithPath: path)
                    guard let cg = loadCGImage(url: url) else { return [] }
                    var out: [PendingExtract] = []
                    out.reserveCapacity(rows.count)
                    for row in rows {
                        guard let crop = cropFaceCGImage(cgImage: cg, bboxString: row.bbox) else { continue }
                        saveFaceCrop(faceID: row.id, croppedCGImage: crop)
                        guard let vec = ArcFaceService.shared.embed(crop) else { continue }
                        out.append(PendingExtract(id: row.id,
                                                  arcFace: ArcFaceService.embeddingToBlob(vec)))
                    }
                    return out
                }
                cont.resume(returning: result)
            }
        }
    }

    fileprivate struct PendingExtract: Sendable {
        let id: Int64
        let arcFace: Data
    }

    /// Crop the bbox region (with padding) out of the source CGImage and
    /// return the cropped CGImage. Vision bboxes are normalized with
    /// bottom-left origin; CGImage cropping uses top-left, so we flip Y.
    ///
    /// Pixel minimum is 8x8 — ArcFace internally scales to 112×112, so
    /// even tiny crops produce a usable (if slightly noisier) embedding.
    /// The bbox-area filter at insertion time already drops obvious
    /// background extras; this is the catch-net for low-res source
    /// images where 0.5% area = ~30px on a 400px frame.
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
        guard pixelRect.width >= 8, pixelRect.height >= 8 else { return nil }
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

}
