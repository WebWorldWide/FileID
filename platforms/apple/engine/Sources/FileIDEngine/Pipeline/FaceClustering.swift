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

    /// Memory cap on faces clustered per run (~2 KB/embedding + HNSW). This is a
    /// HARD bound, not a window: clustering wipes + recreates the persons table
    /// every run, so a re-run cannot incrementally "pick up overflow" without
    /// destroying the prior run's clusters. On a library with more than this many
    /// embedded faces the lowest-id `maxFacesPerRun` are clustered and the tail is
    /// left unassigned (a `face_cluster_overflow` warning is logged). True
    /// >maxFacesPerRun support needs a window-aware persist (tracked separately).
    /// (audit F-C3-033)
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
                // Canonical cross-platform face-clustering failure kind (the
                // Windows form); was `face_cluster_failed`. The app's gate
                // release keys on the `face_cluster` prefix, so this still
                // releases it. (audit F-C2-003)
                kind: "face_clustering_failed",
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
        // Hitting the cap means there are likely embedded faces past it that
        // this wipe-recluster run leaves unassigned. Surface it instead of the
        // old (false) "a re-run picks up overflow" promise. (audit F-C3-033)
        if rows.count >= maxFacesPerRun {
            JSONLog.shared.warn(ev: "face_cluster_overflow",
                                error: "embedded faces reached the \(maxFacesPerRun) per-run cap; faces past it stay unassigned until a window-aware persist lands.")
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
            params: icParams,
            // Poll the engine's existing sync cancel mirror (set on .shutdown via
            // ScanCoordinator.requestCancel) so a mid-flight pass aborts at a safe
            // boundary instead of being killed by _exit(0) behind its persist
            // transaction. The cancelled result is discarded — nothing persisted.
            shouldCancel: { ScanCoordinator.isCancelledSync() }
        )

        // A cancellation mid-pass discards the (partial) clustering result: we
        // persist nothing so the next run re-clusters from a clean slate. (F-C3-042)
        if icResult.cancelled {
            JSONLog.shared.info(ev: "face_cluster_cancelled",
                                extra: ["faces": AnyCodable(decoded.count)])
            return FaceClusteringResult(personCount: 0, faceCount: decoded.count,
                                        unmatchedFaces: unmatched,
                                        durationSeconds: Date().timeIntervalSince(started))
        }

        // Group dense nodes by cluster id (IdentityClustering returns dense IDs
        // from 0). Iterate by sorted cluster id below so person-row creation
        // order — and the IDs the People tab shows — is stable across runs.
        // (audit F-C3-007)
        var byCluster: [Int: [Int]] = [:]
        for (denseIdx, cid) in icResult.clusterIDs.enumerated() {
            byCluster[cid, default: []].append(denseIdx)
        }
        // Cap at maxPersons (catches DB corruption, not normal libraries).
        var truncatedAtCap = 0
        if byCluster.count > maxPersons {
            // Largest clusters survive; tie → smaller cluster id, so the cap is
            // deterministic across runs (audit F-C3-007).
            let sorted = byCluster.sorted {
                $0.value.count != $1.value.count
                    ? $0.value.count > $1.value.count
                    : $0.key < $1.key
            }
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
        let nextClusters: [(centroid: [Float], radius: Float, faceIDs: [Int64])] =
            byCluster.sorted { $0.key < $1.key }.map { (_, denseIdxs) in
                let centroid = computeNormalizedCentroid(
                    denseIdxs: denseIdxs, vecsByDense: vecsByDense, dim: firstDim
                )
                let radius = computeAnchorRadius(
                    denseIdxs: denseIdxs, vecsByDense: vecsByDense, centroid: centroid
                )
                return (centroid, radius, denseIdxs.map { denseToFaceID[$0] })
            }

        struct PersistStats: Sendable { let inherited: Int; let lostNames: Int; let priors: Int }
        let stats: PersistStats
        do {
            stats = try await database.pool.write { db -> PersistStats in
                // RE-READ the identity snapshot HERE — under the persist lock,
                // inside the transaction, BEFORE the DELETE below — not from the
                // PHASE-0 capture. Re-clustering drops + re-creates persons on
                // every run, so a rename / merge / mark-unknown the user committed
                // during the lock-free clustering window (it had to take this same
                // writer lock) is carried forward instead of being silently
                // clobbered by a stale snapshot. The PHASE-0 read still drives the
                // extraction/clustering pool filtering; only the name carry-forward
                // moves under the lock. (audit F-C3-002 / Windows S0)
                let freshPriors = try Self.priorAnchors(from: db)

                // Unknown anchors are excluded from name-inheritance matching:
                // their face_ids weren't in the clustering pool, so their faces
                // stay attached to the existing unknown person row, and a Wave-2
                // cosine match must not transfer is_unknown=true onto a new cluster.
                let inheritanceCandidates = freshPriors.filter { !$0.isUnknown }
                let matches = matchClustersToPriorAnchors(
                    newClusters: nextClusters.map { ($0.centroid, $0.faceIDs) },
                    priorAnchors: inheritanceCandidates
                )
                let priorsWithNames = inheritanceCandidates.filter { $0.hasName }.count
                let claimedPriorIDs = Set(matches.compactMap { $0?.priorPersonID })
                let lostAnchorCount = max(0, priorsWithNames - claimedPriorIDs.count)
                let unknownPriorIDs = freshPriors.filter { $0.isUnknown }.map { $0.id }

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

                // Preserve unknown persons in place. Their face_ids stay bound to
                // their existing row; only non-unknown rows get wiped + recreated.
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

                let now = Date().timeIntervalSince1970
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

                // If app-side Cleanup cascade-deleted member faces mid-pass, a
                // freshly-inserted person's representative_face_id (= the cluster's
                // first face) can now point at a deleted row. Repair the dangle in
                // the same transaction so we never re-introduce the reference
                // reconcilePersons exists to fix. (audit F-C3-041)
                try repairDanglingRepresentativeFaces(db)

                return PersistStats(inherited: matches.compactMap { $0 }.count,
                                    lostNames: lostAnchorCount,
                                    priors: freshPriors.count)
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
        if stats.inherited > 0 || stats.lostNames > 0 {
            JSONLog.shared.info(ev: "face_cluster_anchor_match",
                                extra: ["priors": AnyCodable(stats.priors),
                                        "inherited": AnyCodable(stats.inherited),
                                        "lostNames": AnyCodable(stats.lostNames)])
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
    ///
    /// Three user-verdict guards, enforced transitively through the union-find
    /// (a bridge singleton can never co-locate a forbidden pair even via a chain):
    ///   • is_unknown persons are excluded entirely — the "don't identify these"
    ///     verdict is never overwritten by a cosine merge. (audit F-C3-003)
    ///   • face_verifications "different people" verdicts block the affected
    ///     person pair. (audit F-C3-004)
    ///   • two user-named persons never merge — that would delete one name.
    ///     (audit F-C3-005)
    static func tightPairAutoMerge(database: Database) async -> Int {
        struct PrintRow: Sendable { let personID: Int64; let blob: Data; let fileCount: Int; let named: Bool }
        struct ReadData: Sendable {
            let rows: [PrintRow]
            // "Different people" verdicts projected onto the persons that own
            // the anchor faces RIGHT NOW (after the phase-4 persist).
            let verdictPersonPairs: [(Int64, Int64)]
        }
        let data: ReadData
        do {
            data = try await database.pool.read { db -> ReadData in
                // `named` = the user gave this cluster an identity (structured or
                // legacy name). is_unknown persons are excluded by the WHERE so
                // their "don't identify" verdict can never be merged away.
                let r = try GRDB.Row.fetchAll(db, sql: """
                    SELECT fp.person_id AS pid, fp.arcface_embedding AS blob,
                           p.file_count AS fc,
                           (COALESCE(p.first_name,'') != ''
                             OR COALESCE(p.last_name,'') != ''
                             OR COALESCE(p.name,'') != '') AS named
                    FROM face_prints fp
                    INNER JOIN persons p ON p.id = fp.person_id
                    WHERE fp.person_id IS NOT NULL
                      AND LENGTH(fp.arcface_embedding) > 0
                      AND COALESCE(p.is_unknown, 0) = 0
                    """)
                let rows = r.map { PrintRow(personID: $0["pid"] ?? 0,
                                            blob: $0["blob"] ?? Data(),
                                            fileCount: $0["fc"] ?? 0,
                                            named: ($0["named"] ?? 0) != 0) }

                // "Different people" verdicts are stored face-anchored (face_a/
                // face_b, the cross-platform v13 form the Windows engine reads).
                // Re-project each onto the persons that own those faces now.
                let vrows = try GRDB.Row.fetchAll(db, sql: """
                    SELECT face_a, face_b FROM face_verifications
                    WHERE same_person = 0 AND face_a IS NOT NULL AND face_b IS NOT NULL
                    """)
                var rawPairs: [(Int64, Int64)] = []
                var verdictFaces = Set<Int64>()
                for vr in vrows {
                    let a: Int64 = vr["face_a"] ?? 0
                    let b: Int64 = vr["face_b"] ?? 0
                    if a != 0, b != 0 { rawPairs.append((a, b)); verdictFaces.insert(a); verdictFaces.insert(b) }
                }
                var facePerson: [Int64: Int64] = [:]
                if !verdictFaces.isEmpty {
                    let ph = verdictFaces.map { _ in "?" }.joined(separator: ",")
                    let fpRows = try GRDB.Row.fetchAll(db, sql: """
                        SELECT id, person_id FROM face_prints
                        WHERE person_id IS NOT NULL AND id IN (\(ph))
                        """, arguments: StatementArguments(verdictFaces.map { Int($0) }))
                    for fr in fpRows {
                        let fid: Int64 = fr["id"] ?? 0
                        let pid: Int64 = fr["person_id"] ?? 0
                        if fid != 0, pid != 0 { facePerson[fid] = pid }
                    }
                }
                var pairs: [(Int64, Int64)] = []
                for (a, b) in rawPairs {
                    if let pa = facePerson[a], let pb = facePerson[b], pa != pb { pairs.append((pa, pb)) }
                }
                return ReadData(rows: rows, verdictPersonPairs: pairs)
            }
        } catch {
            JSONLog.shared.warn(ev: "face_auto_merge_query_failed", error: "\(error)")
            return 0
        }
        let rows = data.rows
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

        // Deterministic cluster order (sorted by person id) so the edge sweep,
        // union targets, and persist are stable across runs. (audit F-C3-007)
        var clusters: [Cluster] = []
        clusters.reserveCapacity(byPerson.count)
        for (pid, payload) in byPerson.sorted(by: { $0.key < $1.key }) {
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
        let idxOf: [Int64: Int] = Dictionary(
            uniqueKeysWithValues: clusters.enumerated().map { ($0.element.id, $0.offset) }
        )

        // Index-based union-find over the centroid array (mirrors the Windows
        // consolidate(): edges strongest-first, blocked pairs checked at EVERY
        // union step so a forbidden pair can never share a person transitively).
        var parent = Array(0..<clusters.count)
        func find(_ x: Int) -> Int {
            var r = x
            while parent[r] != r { r = parent[r] }
            var cur = x
            while parent[cur] != r { let next = parent[cur]; parent[cur] = r; cur = next }
            return r
        }
        // Carried up to each root: true iff the set already contains a named
        // person. Rejecting a union of two named roots blocks named↔named merges
        // transitively, where the old per-pair `ci.named && cj.named` guard was
        // defeated by a bridge singleton chaining them. (audit F-C3-005)
        var hasNamed = clusters.map { $0.named }
        // Explicit "different people" verdicts as index pairs. (audit F-C3-004)
        let blockedIdx: [(Int, Int)] = data.verdictPersonPairs.compactMap { (pa, pb) in
            guard let ia = idxOf[pa], let ib = idxOf[pb] else { return nil }
            return (ia, ib)
        }

        // O(N²) pairwise cosine → candidate edges. Cluster is "small" if
        // file_count <= 1. Merge predicate:
        //   cos(ci, cj) >= tightAutoMergeCos                    (always)  OR
        //   cos(ci, cj) >= smallClusterAutoMergeCos AND (i.small OR j.small)
        let started = Date()
        var edges: [(cos: Float, i: Int, j: Int)] = []
        for i in 0..<clusters.count {
            let ci = clusters[i]
            let smallI = ci.fileCount <= 1
            for j in (i + 1)..<clusters.count {
                let cj = clusters[j]
                let smallJ = cj.fileCount <= 1
                let cos = dotProduct(ci.centroid, cj.centroid)
                let isTight = cos >= tightAutoMergeCos
                let isSmallPair = cos >= smallClusterAutoMergeCos && (smallI || smallJ)
                if isTight || isSmallPair { edges.append((cos, i, j)) }
            }
        }
        // Strongest merges first; ties broken by index so the result is stable.
        edges.sort { $0.cos != $1.cos ? $0.cos > $1.cos : ($0.i != $1.i ? $0.i < $1.i : $0.j < $1.j) }
        let pairCount = edges.count

        for edge in edges {
            let ri = find(edge.i), rj = find(edge.j)
            if ri == rj { continue }
            if hasNamed[ri] && hasNamed[rj] { continue }
            let conflict = blockedIdx.contains { (a, b) in
                let ra = find(a), rb = find(b)
                return (ra == ri && rb == rj) || (ra == rj && rb == ri)
            }
            if conflict { continue }
            parent[ri] = rj
            hasNamed[rj] = hasNamed[rj] || hasNamed[ri]
        }

        // Resolve each union group to a survivor: the named member (≤1 by the
        // guard above) wins so its name + row survive; else the largest
        // file_count, tie → smallest person id (determinism).
        func isPreferred(_ a: Int, over b: Int) -> Bool {
            if clusters[a].named != clusters[b].named { return clusters[a].named }
            if clusters[a].fileCount != clusters[b].fileCount {
                return clusters[a].fileCount > clusters[b].fileCount
            }
            return clusters[a].id < clusters[b].id
        }
        var groups: [Int: [Int]] = [:]
        for idx in 0..<clusters.count { groups[find(idx), default: []].append(idx) }
        var byTarget: [Int64: [Int64]] = [:]
        for (_, members) in groups where members.count > 1 {
            var canon = members[0]
            for m in members.dropFirst() where isPreferred(m, over: canon) { canon = m }
            byTarget[clusters[canon].id] = members.filter { $0 != canon }.map { clusters[$0].id }
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

    /// Repoint any persons.representative_face_id that is NULL or references a
    /// face no longer assigned to that person (e.g. cascade-deleted mid-pass) at
    /// a surviving member face, else NULL — mirrors `reconcilePersons`. Must run
    /// inside the caller's write transaction. (audit F-C3-041)
    static func repairDanglingRepresentativeFaces(_ db: GRDB.Database) throws {
        try db.execute(sql: """
            UPDATE persons
            SET representative_face_id =
                (SELECT id FROM face_prints
                  WHERE person_id = persons.id ORDER BY id LIMIT 1)
            WHERE representative_face_id IS NULL
               OR representative_face_id NOT IN
                  (SELECT id FROM face_prints WHERE person_id = persons.id)
            """)
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
    /// inheritance after a re-cluster. Internal (not fileprivate) so the
    /// under-lock re-read seam can be unit-tested. (audit F-C3-002)
    struct PriorAnchor: Sendable {
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
            return try await database.pool.read { db in try priorAnchors(from: db) }
        } catch {
            JSONLog.shared.warn(ev: "face_cluster_anchor_snapshot_failed",
                                error: "\(error)")
            return []
        }
    }

    /// Synchronous identity snapshot from a live `db` handle — usable inside the
    /// persist write transaction so the name carry-forward reads the state under
    /// the lock, not a stale PHASE-0 capture. (audit F-C3-002)
    static func priorAnchors(from db: GRDB.Database) throws -> [PriorAnchor] {
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

    /// After this many failed extraction attempts a row is treated as
    /// permanently failing and skipped for the rest of the engine session, so a
    /// corrupt/missing image at a low id can't sit at the front of the
    /// `ORDER BY id ASC LIMIT` window forever and starve newer faces. (F-C3-033)
    private static let maxExtractionAttempts = 3

    /// Process-lifetime extraction-attempt tally (face_id → consecutive misses).
    /// In-memory only — never marks a row excluded in the DB, so a transient
    /// failure can still recover after an engine restart.
    private static let extractionFailureLock = NSLock()
    private nonisolated(unsafe) static var extractionAttempts: [Int64: Int] = [:]

    static func permanentlyFailedExtractions() -> Set<Int64> {
        extractionFailureLock.lock(); defer { extractionFailureLock.unlock() }
        return Set(extractionAttempts.filter { $0.value >= maxExtractionAttempts }.keys)
    }

    static func recordExtractionOutcomes(attempted: [Int64], succeeded: Set<Int64>) {
        extractionFailureLock.lock(); defer { extractionFailureLock.unlock() }
        for id in attempted {
            if succeeded.contains(id) { extractionAttempts[id] = nil }
            else { extractionAttempts[id, default: 0] += 1 }
        }
    }

    /// Test seam: reset the in-memory extraction-failure tally.
    static func resetExtractionFailuresForTesting() {
        extractionFailureLock.lock(); defer { extractionFailureLock.unlock() }
        extractionAttempts.removeAll()
    }

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
        let permanentlyFailed = permanentlyFailedExtractions()
        let pending: [PendingRow]
        do {
            pending = try await database.pool.read { db in
                // Fetch a window wide enough that even if every skipped row
                // (unknown faces + permanently-failing rows) lands at the front,
                // we still surface `maxExtractionsPerRun` fresh rows past them —
                // the front-of-window starvation fix. (F-C3-033)
                let fetchLimit = maxExtractionsPerRun + skipFaceIDs.count + permanentlyFailed.count
                let rows = try GRDB.Row.fetchAll(db, sql: """
                    SELECT face_prints.id, face_prints.bbox,
                           files.path_text AS path
                    FROM face_prints
                    INNER JOIN files ON files.id = face_prints.file_id
                    WHERE files.failed = 0
                      AND face_prints.excluded = 0
                      AND LENGTH(COALESCE(face_prints.arcface_embedding, X'')) = 0
                    ORDER BY face_prints.id ASC
                    LIMIT \(fetchLimit)
                    """)
                let filtered = rows.compactMap { r -> PendingRow? in
                    let id: Int64 = r["id"] ?? 0
                    if skipFaceIDs.contains(id) || permanentlyFailed.contains(id) { return nil }
                    return PendingRow(id: id,
                                       bbox: r["bbox"] ?? "",
                                       path: r["path"] ?? "")
                }
                return Array(filtered.prefix(maxExtractionsPerRun))
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
        // Tally which attempted rows produced no embedding so a row that keeps
        // failing drops out of future windows instead of blocking newer faces.
        // (F-C3-033)
        let succeeded = Set(extractedSnapshot.map { $0.id })
        recordExtractionOutcomes(attempted: pending.map { $0.id }, succeeded: succeeded)
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
                                        "failed": AnyCodable(pending.count - extractedSnapshot.count),
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
