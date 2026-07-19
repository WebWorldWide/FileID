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
import Vision

public enum FaceClustering {

    struct ProtectedFacePair: Hashable, Sendable {
        let first: Int64
        let second: Int64

        init(_ a: Int64, _ b: Int64) {
            first = min(a, b)
            second = max(a, b)
        }
    }

    enum FaceProtectionError: Error {
        case ambiguousVerdictAnchor
        case identicalVerdictAnchors
        case invalidPartition(String)
        case protectedClusterCap
        case verdictCap
    }

    static func currentPersonCount(database: Database) async -> Int {
        do {
            return try await database.pool.read { db in
                try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM persons") ?? 0
            }
        } catch {
            JSONLog.shared.warn(ev: "face_cluster_person_count_failed", error: "\(error)")
            return 0
        }
    }

    struct ClusteringFaceRow: Sendable {
        let id: Int64
        let arcFace: Data
        let quality: Double
    }

    static func loadClusteringFaceRows(
        from db: GRDB.Database,
        phaseZeroUnknownFaceIDs: Set<Int64>,
        minQuality: Double,
        limit: Int
    ) throws -> (rows: [ClusteringFaceRow], overflowed: Bool) {
        var cursor = try GRDB.Row.fetchCursor(db, sql: """
            SELECT face_prints.id, face_prints.arcface_embedding, face_prints.face_quality
            FROM face_prints
            LEFT JOIN persons ON persons.id = face_prints.person_id
            WHERE face_prints.excluded = 0
              AND LENGTH(face_prints.arcface_embedding) > 0
              AND COALESCE(persons.is_unknown, 0) = 0
            ORDER BY face_prints.id ASC
            """)
        var loaded: [ClusteringFaceRow] = []
        loaded.reserveCapacity(min(limit, 16_384))
        var overflowed = false
        while let row = try cursor.next() {
            let id: Int64 = row["id"] ?? 0
            if phaseZeroUnknownFaceIDs.contains(id) { continue }
            let quality: Double = row["face_quality"] ?? -1
            if minQuality > 0, quality < minQuality { continue }
            if loaded.count == limit {
                overflowed = true
                break
            }
            loaded.append(ClusteringFaceRow(
                id: id,
                arcFace: row["arcface_embedding"] ?? Data(),
                quality: quality
            ))
        }
        return (loaded, overflowed)
    }

    static func differentFacePairs(from db: GRDB.Database) throws -> Set<ProtectedFacePair> {
        let rows = try GRDB.Row.fetchAll(db, sql: """
            SELECT face_a, face_b, file_a, bbox_a, file_b, bbox_b
            FROM face_verifications
            WHERE same_person = 0
              AND ((face_a IS NOT NULL AND face_b IS NOT NULL)
                   OR (file_a IS NOT NULL AND bbox_a IS NOT NULL
                       AND file_b IS NOT NULL AND bbox_b IS NOT NULL))
            ORDER BY person_a ASC, person_b ASC
            LIMIT 100001
            """)
        guard rows.count <= 100_000 else { throw FaceProtectionError.verdictCap }
        func resolve(_ legacy: Int64?, _ fileID: Int64?, _ bbox: String?) throws -> Int64? {
            if let fileID, let bbox {
                let ids = try Int64.fetchAll(
                    db,
                    sql: "SELECT id FROM face_prints WHERE file_id = ? AND bbox = ? ORDER BY id LIMIT 2",
                    arguments: [fileID, bbox]
                )
                if ids.count > 1 { throw FaceProtectionError.ambiguousVerdictAnchor }
                if let id = ids.first { return id }
            }
            guard let legacy, legacy != 0 else { return nil }
            return try Int64.fetchOne(
                db,
                sql: "SELECT id FROM face_prints WHERE id = ?",
                arguments: [legacy]
            )
        }

        var pairs = Set<ProtectedFacePair>()
        for row in rows {
            guard let a = try resolve(row["face_a"], row["file_a"], row["bbox_a"]),
                  let b = try resolve(row["face_b"], row["file_b"], row["bbox_b"])
            else { continue }
            if a == b { throw FaceProtectionError.identicalVerdictAnchors }
            pairs.insert(ProtectedFacePair(a, b))
        }
        return pairs
    }

    static func partitionProtectedClusters(
        _ byCluster: [Int: [Int]],
        denseToFaceID: [Int64],
        bucketOwnerByFaceID: [Int64: Int64],
        differentPairs: Set<ProtectedFacePair>,
        excludedFaceIDs: Set<Int64>
    ) -> (clusters: [Int: [Int]], protectedFaceIDs: Set<Int64>) {
        let denseByFaceID = Dictionary(
            uniqueKeysWithValues: denseToFaceID.enumerated().map { ($0.element, $0.offset) }
        )
        let ownerlessEndpointSet = Set(differentPairs.flatMap { [$0.first, $0.second] })
            .filter { bucketOwnerByFaceID[$0] == nil && !excludedFaceIDs.contains($0) }
        var raw = byCluster.mapValues { denseIndexes in
            denseIndexes.filter { denseIndex in
                let faceID = denseToFaceID[denseIndex]
                return !excludedFaceIDs.contains(faceID)
                    && bucketOwnerByFaceID[faceID] == nil
                    && !ownerlessEndpointSet.contains(faceID)
            }
        }
        var ownerGroups: [Int64: [Int]] = [:]
        for (faceID, ownerID) in bucketOwnerByFaceID {
            guard !excludedFaceIDs.contains(faceID), let denseIndex = denseByFaceID[faceID] else {
                continue
            }
            ownerGroups[ownerID, default: []].append(denseIndex)
        }
        let ownerlessEndpoints = ownerlessEndpointSet.sorted()
        var singletonFaceIDsByOwner: [Int64: Set<Int64>] = [:]
        for pair in differentPairs {
            if let ownerID = bucketOwnerByFaceID[pair.first],
               bucketOwnerByFaceID[pair.second] == ownerID {
                singletonFaceIDsByOwner[ownerID, default: []].formUnion([pair.first, pair.second])
            }
        }

        var buckets: [[Int]] = []
        for ownerID in ownerGroups.keys.sorted() {
            let members = ownerGroups[ownerID, default: []]
            let singletonFaceIDs = singletonFaceIDsByOwner[ownerID, default: []]
            let remainder = members.filter { !singletonFaceIDs.contains(denseToFaceID[$0]) }
            if !remainder.isEmpty { buckets.append(remainder) }
            for faceID in singletonFaceIDs.sorted() {
                if let denseIndex = denseByFaceID[faceID] { buckets.append([denseIndex]) }
            }
        }
        for faceID in ownerlessEndpoints {
            if let denseIndex = denseByFaceID[faceID] { buckets.append([denseIndex]) }
        }
        for clusterID in raw.keys.sorted() {
            let members = raw.removeValue(forKey: clusterID) ?? []
            if !members.isEmpty { buckets.append(members) }
        }

        var clusters: [Int: [Int]] = [:]
        for (clusterID, members) in buckets.enumerated() {
            clusters[clusterID] = members.sorted { denseToFaceID[$0] < denseToFaceID[$1] }
        }
        var protectedFaceIDs = Set(bucketOwnerByFaceID.keys)
        for pair in differentPairs {
            protectedFaceIDs.insert(pair.first)
            protectedFaceIDs.insert(pair.second)
        }
        protectedFaceIDs.subtract(excludedFaceIDs)
        protectedFaceIDs.formIntersection(Set(denseByFaceID.keys))
        return (clusters, protectedFaceIDs)
    }

    static func capClusters(
        _ clusters: [Int: [Int]],
        protectedClusterIDs: Set<Int>,
        preservedPersonCount: Int,
        maxPersons: Int
    ) throws -> (kept: [Int: [Int]], truncatedFaces: Int) {
        guard preservedPersonCount <= maxPersons else {
            throw FaceProtectionError.protectedClusterCap
        }
        let availableClusterSlots = maxPersons - preservedPersonCount
        let protectedIDs = protectedClusterIDs.intersection(Set(clusters.keys))
        guard protectedIDs.count <= availableClusterSlots else {
            throw FaceProtectionError.protectedClusterCap
        }
        guard clusters.count > availableClusterSlots else { return (clusters, 0) }
        let availableUnprotected = availableClusterSlots - protectedIDs.count
        let keepUnprotected = Set(
            clusters
                .filter { !protectedIDs.contains($0.key) }
                .sorted {
                    $0.value.count != $1.value.count
                        ? $0.value.count > $1.value.count
                        : $0.key < $1.key
                }
                .prefix(availableUnprotected)
                .map(\.key)
        )
        var kept = clusters
        var truncatedFaces = 0
        for (clusterID, members) in clusters
            where !protectedIDs.contains(clusterID) && !keepUnprotected.contains(clusterID) {
            truncatedFaces += members.count
            kept[clusterID] = nil
        }
        return (kept, truncatedFaces)
    }

    static func validatePersistPlan(
        from db: GRDB.Database,
        clusterFaceIDs: [[Int64]],
        representativeFaceIDs: [Int64]
    ) throws {
        guard clusterFaceIDs.count == representativeFaceIDs.count else {
            throw FaceProtectionError.invalidPartition("persistence plan cardinality mismatch")
        }
        var plannedFaceIDs = Set<Int64>()
        var representativeSet = Set<Int64>()
        for (faceIDs, representativeFaceID) in zip(clusterFaceIDs, representativeFaceIDs) {
            guard !faceIDs.isEmpty,
                  faceIDs.contains(representativeFaceID),
                  representativeSet.insert(representativeFaceID).inserted
            else {
                throw FaceProtectionError.invalidPartition("invalid persistence anchor")
            }
            for faceID in faceIDs where !plannedFaceIDs.insert(faceID).inserted {
                throw FaceProtectionError.invalidPartition("duplicate persistence member")
            }
        }
        var currentCount = 0
        let orderedFaceIDs = plannedFaceIDs.sorted()
        for chunkStart in stride(from: 0, to: orderedFaceIDs.count, by: 900) {
            let chunk = Array(orderedFaceIDs[chunkStart..<min(chunkStart + 900, orderedFaceIDs.count)])
            let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
            currentCount += try Int.fetchOne(
                db,
                sql: "SELECT COUNT(*) FROM face_prints WHERE id IN (\(placeholders))",
                arguments: StatementArguments(chunk.map { Int($0) })
            ) ?? 0
        }
        guard currentCount == plannedFaceIDs.count else {
            throw FaceProtectionError.invalidPartition("persistence plan references stale faces")
        }
    }

    static func validateProtectedClusters(
        _ clusters: [Int: [Int]],
        denseToFaceID: [Int64],
        identityOwnerByFaceID: [Int64: Int64],
        differentPairs: Set<ProtectedFacePair>
    ) throws {
        var clusterByFaceID: [Int64: Int] = [:]
        for (clusterID, denseIndexes) in clusters {
            for denseIndex in denseIndexes { clusterByFaceID[denseToFaceID[denseIndex]] = clusterID }
        }
        var ownerByCluster: [Int: Int64] = [:]
        for (faceID, ownerID) in identityOwnerByFaceID {
            guard let clusterID = clusterByFaceID[faceID] else { continue }
            if let existing = ownerByCluster[clusterID], existing != ownerID {
                throw FaceProtectionError.invalidPartition("distinct named identities share a cluster")
            }
            ownerByCluster[clusterID] = ownerID
        }
        for pair in differentPairs {
            if let first = clusterByFaceID[pair.first],
               let second = clusterByFaceID[pair.second],
               first == second {
                throw FaceProtectionError.invalidPartition("different-people anchors share a cluster")
            }
        }
    }

    /// Centroid cosine ≥ this triggers auto-merge regardless of cluster
    /// size — same person with very high confidence. Compare to ArcFace
    /// verification literature: 0.40 is the FAR=10⁻⁴ threshold for
    /// individual face pairs; centroid-to-centroid is denoised, so we
    /// can pull this stricter without losing recall. RAISE it (e.g. 0.70)
    /// via FILEID_FACE_TIGHT_COS to reduce different-people over-merging.
    public static var tightAutoMergeCos: Float { envCos("FILEID_FACE_TIGHT_COS", 0.65) }

    /// Looser cosine threshold used only when at least one cluster is
    /// a single face — those are almost always fragments of an existing
    /// person, not a distinct identity. RAISE via FILEID_FACE_SMALL_COS to
    /// reduce over-merging of singletons into the wrong person.
    public static var smallClusterAutoMergeCos: Float { envCos("FILEID_FACE_SMALL_COS", 0.55) }

    /// Read a cosine threshold override from the environment, clamped to a sane
    /// [0,1] band; falls back to `dflt`. Lets the thresholds be swept against a
    /// real corpus (the right way to tune face clustering — never trained/guessed).
    static func envCos(_ key: String, _ dflt: Float) -> Float {
        ProcessInfo.processInfo.environment[key]
            .flatMap { Float($0) }
            .flatMap { (0.0...1.0).contains($0) ? $0 : nil } ?? dflt
    }

    /// Junk-cluster suppression knobs. A face cluster is only persisted as a
    /// person when it is CORROBORATED — either it has at least `minClusterSizeToKeep`
    /// mutually-similar faces (a real recurring identity, regardless of per-face
    /// quality), OR its best face clears `soloQualityFloor` (Apple Vision
    /// faceCaptureQuality). 1–2 face clusters built only from low-quality faces
    /// (blurry / motion-blur / tiny / heavy-profile) are LEFT UNCLUSTERED
    /// (person_id NULL — still a "candidate" the user can attach later) instead of
    /// spawning a spurious singleton person. This is what shrinks the People tab's
    /// long tail of junk one-off faces WITHOUT ever merging two identities.
    ///
    /// Calibrated on the real 991-face reference library: 268 of 407 clusters were
    /// singletons (avg quality 0.19) and 91% were 1–2 faces; crops below quality
    /// ~0.12 are visually unrecognizable (blur/profile/occlusion) while genuine
    /// distinct singletons (sunglasses, face-paint, one-off portraits) sit ≥0.25.
    /// size≥3 is protected because ≥3 faces linked at cosine ≥0.66 is a real person
    /// even when every frame is mediocre (e.g. a 20-shot low-light cluster). Defaults
    /// take 407→280 persons (127 junk micro-clusters suppressed, 0 identity merges).
    /// Set FILEID_FACE_SOLO_QUALITY=0 to disable suppression entirely.
    public static var minClusterSizeToKeep: Int { envInt("FILEID_FACE_MIN_CLUSTER_SIZE", 3, 1...10_000) }
    public static var soloQualityFloor: Double { envDouble("FILEID_FACE_SOLO_QUALITY", 0.12, 0.0...1.0) }

    /// Integer env override, clamped to `range`; falls back to `dflt`.
    static func envInt(_ key: String, _ dflt: Int, _ range: ClosedRange<Int>) -> Int {
        ProcessInfo.processInfo.environment[key]
            .flatMap { Int($0) }
            .flatMap { range.contains($0) ? $0 : nil } ?? dflt
    }

    /// Double env override, clamped to `range`; falls back to `dflt`.
    static func envDouble(_ key: String, _ dflt: Double, _ range: ClosedRange<Double>) -> Double {
        ProcessInfo.processInfo.environment[key]
            .flatMap { Double($0) }
            .flatMap { range.contains($0) ? $0 : nil } ?? dflt
    }

    /// Boolean env override: "1"/"true" (case-insensitive) → true, anything else
    /// present → false, absent → `dflt`. Used for the mutual-kNN Pass-1 gate, which
    /// stays OFF by default on macOS pending on-Mac label calibration (Apple Vision
    /// quality is a different scale than the Windows YuNet path). (parity mirror)
    // UNVERIFIED-UNTIL-MAC (2026-07-05 parity mirror)
    static func envBool(_ key: String, _ dflt: Bool) -> Bool {
        guard let v = ProcessInfo.processInfo.environment[key] else { return dflt }
        return v == "1" || v.lowercased() == "true"
    }

    /// Apply junk-cluster suppression to a dense-index cluster map: keep a cluster
    /// iff it has ≥ `minSize` members OR its best member quality ≥ `qualityFloor`.
    /// Pure + deterministic so it can be unit-tested without a DB. Returns the kept
    /// clusters plus the count of suppressed clusters/faces (for logging + the
    /// unmatched tally). `qualityFloor <= 0` disables suppression (keeps all).
    static func suppressLowQualityClusters(
        _ byCluster: [Int: [Int]],
        denseToFaceID: [Int64],
        faceQualityByID: [Int64: Double],
        minSize: Int,
        qualityFloor: Double,
        alwaysKeep: Set<Int> = []
    ) -> (kept: [Int: [Int]], suppressedClusters: Int, suppressedFaces: Int) {
        guard qualityFloor > 0 else { return (byCluster, 0, 0) }
        var kept: [Int: [Int]] = [:]
        var sClusters = 0, sFaces = 0
        for (cid, denseIdxs) in byCluster {
            if alwaysKeep.contains(cid) || denseIdxs.count >= minSize {
                kept[cid] = denseIdxs
                continue
            }
            let maxQ = denseIdxs.reduce(-Double.greatestFiniteMagnitude) { acc, d in
                max(acc, faceQualityByID[denseToFaceID[d]] ?? -1)
            }
            if maxQ >= qualityFloor {
                kept[cid] = denseIdxs
            } else {
                sClusters += 1
                sFaces += denseIdxs.count
            }
        }
        return (kept, sClusters, sFaces)
    }

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

    /// Effective mid-pass cancel for a clustering run. The SCAN cancel mirror is
    /// sticky — set on `.cancelScan` / `.shutdown` and cleared only by the next
    /// scan's `startSession()` — so a standalone (`.runFaceClustering`) or auto
    /// cluster enqueued after a cancelled scan must NOT abort merely because that
    /// prior scan was cancelled (it would hit cancel on its first poll and return
    /// before the persist txn — a silent "0 persons" no-op). We honor the mirror
    /// only when it flips true AFTER this run began (`baseline == false`) — a
    /// genuine shutdown/cancel mid-pass — preserving the F-C3-042 cooperative
    /// cancel. (audit R-07)
    static func clusterShouldCancel(baseline: Bool, current: Bool,
                                    shuttingDown: Bool = false) -> Bool {
        // A genuine shutdown always aborts (its dedicated mirror is never set by a
        // stale scan-cancel), regardless of the sticky scan-cancel baseline. (R-07)
        shuttingDown || (current && !baseline)
    }

    /// Run a clustering pass. Returns a summary the engine emits over IPC.
    public static func runClustering(
        database: Database,
        sink: IPCSink
    ) async -> FaceClusteringResult {
        let started = Date()

        // Snapshot the sticky SCAN cancel mirror at entry so this clustering job
        // has its own cancellation scope: a previously-cancelled scan can't
        // suppress a later manual/auto cluster, while a cancel/shutdown arriving
        // DURING the run still aborts it at a safe boundary. (audit R-07)
        let cancelBaseline = ScanCoordinator.isCancelledSync()

        // ArcFace is a hard requirement — no Vision-print fallback. Vision
        // feature prints aren't face-identity-trained; clustering on them
        // produces mega-clusters at scale (the bug we're fixing). If the
        // model isn't installed we surface an actionable error and exit.
        var loadOK = ArcFaceService.shared.isReady
        if !loadOK {
            for kind in FaceEmbedderKind.installedKinds() {
                loadOK = ArcFaceService.shared.load(kind)
                break
            }
        }
        guard ArcFaceService.shared.isReady else {
            // Two distinct failures, two accurate IPC errors. Both keep the
            // `face_cluster` prefix the app keys on (EngineClient.swift
            // `hasPrefix("face_cluster")`) so the clustering gate still
            // releases instead of the UI hanging "clustering…". (hardening)
            if FaceEmbedderKind.installedKinds().isEmpty {
                JSONLog.shared.warn(ev: "face_cluster_skipped_no_model",
                                    error: "SFace model not installed; cannot cluster.")
                let bytes = ModelManifest.artifacts
                    .first { $0.id == "sface_embedder" }?.approxBytes ?? 0
                let size = ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
                await sink.emit(.error(EngineError(
                    kind: "face_cluster_no_model",
                    message: "Face-recognition model not installed. Open Settings → AI Models — face recognition to install SFace (\(size))."
                )))
            } else {
                // A model IS on disk but load() couldn't bind it (execution
                // provider / corrupt-ONNX / runtime error). The "install the
                // model" prompt would be wrong here — surface the real state.
                JSONLog.shared.error(ev: "face_cluster_embedder_load_failed",
                                     error: "Face embedder installed but load() failed (loadOK=\(loadOK)); execution-provider/runtime error.")
                await sink.emit(.error(EngineError(
                    kind: "face_cluster_embedder_load_failed",
                    message: "Face-recognition model is installed but failed to load (execution-provider/runtime error). See logs."
                )))
            }
            let personCount = await currentPersonCount(database: database)
            return FaceClusteringResult(personCount: personCount, faceCount: 0,
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
        // Persons whose faces are excluded from THIS run's pool (Phase-0 unknown).
        // The persist must preserve these rows even if the user un-marks one during
        // the lock-free window: their faces are in NO new cluster, so a wipe would
        // orphan the faces and destroy the just-entered name. (R3-02)
        let poolExcludedPersonIDs: Set<Int64> = Set(
            priorAnchors.filter { $0.isUnknown }.map { $0.id }
        )

        // PHASE 1 — extract any pending ArcFace embeddings. Idempotent.
        await extractPendingPrints(database: database, sink: sink,
                                    skipFaceIDs: unknownFaceIDs)

        // PHASE 2 — load every face_prints row with an ArcFace embedding
        // and not excluded by the quality filter. Unknown-person face_ids
        // are filtered out: the user explicitly said "don't cluster these",
        // so they stay attached to their existing unknown person row and
        // never participate in a re-cluster pass.
        // Pre-clustering quality gate — parity hook for the Rust
        // FILEID_FACE_CLUSTER_MIN_QUALITY. In the Windows engine sub-threshold
        // faces embed as non-discriminative noise (same-person cosine ~= diff)
        // and chain through hub faces into low-cohesion mega-cones, so gating
        // them (left UNCLUSTERED, person_id NULL) raised precision.
        // CAVEAT: macOS `face_quality` is Apple Vision `faceCaptureQuality`, a
        // DIFFERENT 0..1 scale than the Windows YuNet det.score×geometry (whose
        // calibrated gate is 0.35) — that value does NOT transfer. So the macOS
        // DEFAULT is 0.0 = OFF (zero regression); a Mac session must calibrate it
        // against labels before enabling. Only applied when > 0, so missing/NULL
        // quality (-1 sentinel) is untouched at the default. (see MACOS_LOCKSTEP_NOTES)
        // UNVERIFIED-UNTIL-MAC (2026-07-05 parity mirror)
        let clusterMinQuality = envDouble("FILEID_FACE_CLUSTER_MIN_QUALITY", 0.0, 0.0...1.0)
        let rows: [ClusteringFaceRow]
        let rowsOverflowed: Bool
        do {
            (rows, rowsOverflowed) = try await database.pool.read { db in
                try loadClusteringFaceRows(
                    from: db,
                    phaseZeroUnknownFaceIDs: unknownFaceIDs,
                    minQuality: clusterMinQuality,
                    limit: maxFacesPerRun
                )
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
            let personCount = await currentPersonCount(database: database)
            return FaceClusteringResult(personCount: personCount, faceCount: 0,
                                        unmatchedFaces: 0,
                                        durationSeconds: Date().timeIntervalSince(started))
        }

        guard !rows.isEmpty else {
            JSONLog.shared.info(ev: "face_cluster_empty")
            let personCount = await currentPersonCount(database: database)
            return FaceClusteringResult(personCount: personCount, faceCount: 0,
                                        unmatchedFaces: 0,
                                        durationSeconds: Date().timeIntervalSince(started))
        }
        // Hitting the cap means there are likely embedded faces past it that
        // this wipe-recluster run leaves unassigned. Surface it instead of the
        // old (false) "a re-run picks up overflow" promise. (audit F-C3-033)
        if rowsOverflowed {
            JSONLog.shared.warn(ev: "face_cluster_overflow",
                                error: "embedded faces reached the \(maxFacesPerRun) per-run cap; faces past it stay unassigned until a window-aware persist lands.")
        }

        // Per-face capture quality keyed by face id (parallel to the embeddings), so each
        // cluster's representative face is its highest-quality member — mirrors the Windows
        // anchor pick in face_clustering.rs (`max_by quality`) rather than just `.first`.
        let faceQualityByID: [Int64: Double] = Dictionary(
            rows.map { ($0.id, $0.quality) }, uniquingKeysWith: { first, _ in first })

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
            let personCount = await currentPersonCount(database: database)
            return FaceClusteringResult(personCount: personCount, faceCount: rows.count,
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
        for (i, face) in decoded.enumerated() {
            if i % 1_000 == 0, Self.clusterShouldCancel(
                baseline: cancelBaseline,
                current: ScanCoordinator.isCancelledSync(),
                shuttingDown: ScanCoordinator.isShuttingDownSync()
            ) {
                JSONLog.shared.info(ev: "face_cluster_cancelled",
                                    extra: ["faces": AnyCodable(decoded.count)])
                let personCount = await currentPersonCount(database: database)
                return FaceClusteringResult(personCount: personCount, faceCount: decoded.count,
                                            unmatchedFaces: unmatched,
                                            durationSeconds: Date().timeIntervalSince(started))
            }
            let hnswID = index.insert(face.vec)
            guard hnswID >= 0 else { unmatched += 1; continue }
            denseToFaceID.append(face.id)
            vecsByDense.append(face.vec)
        }
        let n = denseToFaceID.count
        guard n > 0 else {
            JSONLog.shared.warn(ev: "face_cluster_no_inserts",
                                error: "HNSW rejected every embedding")
            let personCount = await currentPersonCount(database: database)
            return FaceClusteringResult(personCount: personCount, faceCount: decoded.count,
                                        unmatchedFaces: rows.count,
                                        durationSeconds: Date().timeIntervalSince(started))
        }

        // pass1Cosine is the tightest "same person" gate; RAISE it via
        // FILEID_FACE_PASS1_COS (e.g. 0.70) to split different people apart more
        // aggressively when over-merging. Default preserved.
        let icParams = IdentityClustering.Hyperparameters(
            pass1Cosine: envCos("FILEID_FACE_PASS1_COS", 0.66),
            // Mutual-kNN Pass-1 gate — parity hook for the Rust FILEID_FACE_MUTUAL_KNN.
            // DEFAULT OFF on macOS (Windows is default-ON): the Vision quality scale +
            // separate alignment path mean the Windows calibration doesn't transfer, so
            // enable only after on-Mac label validation. (see MACOS_LOCKSTEP_NOTES)
            mutualKNN: envBool("FILEID_FACE_MUTUAL_KNN", false) // UNVERIFIED-UNTIL-MAC (2026-07-05 parity mirror)
        )
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
            // Poll the engine's sync cancel mirror (set via ScanCoordinator
            // .requestCancel on .cancelScan/.shutdown) so a mid-flight pass aborts
            // at a safe boundary instead of being killed by _exit(0) behind its
            // persist transaction. `clusterShouldCancel` ignores a mirror that was
            // already true at entry (a sticky prior scan-cancel) so this job runs;
            // it fires only on a transition during the run. Cancelled = nothing
            // persisted. (F-C3-042, R-07)
            shouldCancel: {
                Self.clusterShouldCancel(baseline: cancelBaseline,
                                         current: ScanCoordinator.isCancelledSync(),
                                         shuttingDown: ScanCoordinator.isShuttingDownSync())
            }
        )

        // A cancellation mid-pass discards the (partial) clustering result: we
        // persist nothing so the next run re-clusters from a clean slate. (F-C3-042)
        if icResult.cancelled {
            JSONLog.shared.info(ev: "face_cluster_cancelled",
                                extra: ["faces": AnyCodable(decoded.count)])
            let personCount = await currentPersonCount(database: database)
            return FaceClusteringResult(personCount: personCount, faceCount: decoded.count,
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

        let rawClusters = byCluster

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
        struct PersistStats: Sendable {
            let inherited: Int
            let lostNames: Int
            let priors: Int
            let personCount: Int
            let suppressedClusters: Int
            let suppressedFaces: Int
            let truncatedFaces: Int
        }
        let stats: PersistStats
        do {
            // Capture the fully-built Phase-0 pool arrays immutably: the
            // pool.write closure is @Sendable, and Swift 6 rejects referencing
            // the outer `var` bindings from concurrently-executing code. Both
            // are read-only here and arrays are copy-on-write, so this is a
            // reference bump, not a deep copy.
            stats = try await database.pool.write { [denseToFaceID, vecsByDense] db -> PersistStats in
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
                let differentPairs = try Self.differentFacePairs(from: db)
                var ownerByFaceID: [Int64: Int64] = [:]
                for prior in freshPriors {
                    for faceID in prior.faceIDs { ownerByFaceID[faceID] = prior.id }
                }
                let verdictOwnerIDs = Set(differentPairs.flatMap { pair in
                    [ownerByFaceID[pair.first], ownerByFaceID[pair.second]].compactMap { $0 }
                })
                let namedOwnerIDs = Set(
                    freshPriors.filter { $0.hasName && !$0.isUnknown }.map { $0.id }
                )
                let protectedOwnerIDs = namedOwnerIDs.union(verdictOwnerIDs)
                let poolFaceIDs = Set(denseToFaceID)
                let protectedOutsidePool = protectedOutsidePoolOwnerIDs(
                    priors: freshPriors,
                    protectedOwnerIDs: protectedOwnerIDs,
                    poolFaceIDs: poolFaceIDs
                )
                let preserveIDs = Set(freshPriors.filter { $0.isUnknown }.map { $0.id })
                    .union(poolExcludedPersonIDs)
                    .union(protectedOutsidePool)
                let preservedPersonCount = freshPriors.filter {
                    preserveIDs.contains($0.id)
                }.count
                let excludedFaceIDs = Set(
                    freshPriors
                        .filter { preserveIDs.contains($0.id) }
                        .flatMap { $0.faceIDs }
                )
                let activeProtectedOwnerIDs = protectedOwnerIDs.subtracting(preserveIDs)
                let bucketOwnerByFaceID = ownerByFaceID.filter {
                    activeProtectedOwnerIDs.contains($0.value) && poolFaceIDs.contains($0.key)
                }
                let identityOwnerByFaceID = ownerByFaceID.filter {
                    namedOwnerIDs.contains($0.value) && !preserveIDs.contains($0.value)
                        && poolFaceIDs.contains($0.key)
                }
                let partition = partitionProtectedClusters(
                    rawClusters,
                    denseToFaceID: denseToFaceID,
                    bucketOwnerByFaceID: bucketOwnerByFaceID,
                    differentPairs: differentPairs,
                    excludedFaceIDs: excludedFaceIDs
                )
                let protectedClusterIDs = Set(partition.clusters.compactMap { (clusterID, members) in
                    members.contains { partition.protectedFaceIDs.contains(denseToFaceID[$0]) }
                        ? clusterID : nil
                })
                let suppression = suppressLowQualityClusters(
                    partition.clusters,
                    denseToFaceID: denseToFaceID,
                    faceQualityByID: faceQualityByID,
                    minSize: minClusterSizeToKeep,
                    qualityFloor: soloQualityFloor,
                    alwaysKeep: protectedClusterIDs
                )
                let capped = try capClusters(
                    suppression.kept,
                    protectedClusterIDs: protectedClusterIDs,
                    preservedPersonCount: preservedPersonCount,
                    maxPersons: maxPersons
                )
                let finalClusters = capped.kept
                let truncatedFaces = capped.truncatedFaces
                try validateProtectedClusters(
                    finalClusters,
                    denseToFaceID: denseToFaceID,
                    identityOwnerByFaceID: identityOwnerByFaceID,
                    differentPairs: differentPairs
                )

                let nextClusters: [(centroid: [Float], radius: Float, faceIDs: [Int64], repFaceID: Int64)] =
                    finalClusters.sorted { $0.key < $1.key }.map { (_, denseIdxs) in
                        let centroid = computeNormalizedCentroid(
                            denseIdxs: denseIdxs, vecsByDense: vecsByDense, dim: firstDim
                        )
                        let radius = computeAnchorRadius(
                            denseIdxs: denseIdxs, vecsByDense: vecsByDense, centroid: centroid
                        )
                        let faceIDs = denseIdxs.map { denseToFaceID[$0] }
                        return (centroid, radius, faceIDs,
                                representativeFaceID(faceIDs, quality: faceQualityByID))
                    }
                let inheritanceCandidates = freshPriors.filter { !preserveIDs.contains($0.id) }
                let matches = matchClustersToPriorAnchors(
                    newClusters: nextClusters.map { ($0.centroid, $0.faceIDs) },
                    priorAnchors: inheritanceCandidates
                )
                let priorsWithNames = inheritanceCandidates.filter { $0.hasName }.count
                let claimedPriorIDs = Set(matches.compactMap { $0?.priorPersonID })
                let lostAnchorCount = max(0, priorsWithNames - claimedPriorIDs.count)
                let preserveList = Array(preserveIDs)

                let personsList: [ClusterPersist] = nextClusters.enumerated().map { idx, cluster in
                    ClusterPersist(
                        repFaceID: cluster.repFaceID,
                        faceIDs: cluster.faceIDs,
                        count: cluster.faceIDs.count,
                        centroid: cluster.centroid,
                        anchorRadius: cluster.radius,
                        inherited: matches[idx]
                    )
                }
                try validatePersistPlan(
                    from: db,
                    clusterFaceIDs: personsList.map(\.faceIDs),
                    representativeFaceIDs: personsList.map(\.repFaceID)
                )

                // Preserve pool-excluded persons in place (fresh unknowns + anyone
                // whose faces never entered this run's pool). Their face_ids stay
                // bound to their existing row; only persons whose faces WERE
                // clustered this run get wiped + recreated. (R3-02)
                if preserveList.isEmpty {
                    try db.execute(sql: "UPDATE face_prints SET person_id = NULL")
                    try db.execute(sql: "DELETE FROM persons")
                } else {
                    let placeholders = preserveList.map { _ in "?" }.joined(separator: ",")
                    let preserveArgs = StatementArguments(preserveList.map { Int($0) })
                    try db.execute(
                        sql: """
                            UPDATE face_prints SET person_id = NULL
                            WHERE person_id IS NULL OR person_id NOT IN (\(placeholders))
                            """,
                        arguments: preserveArgs
                    )
                    try db.execute(
                        sql: "DELETE FROM persons WHERE id NOT IN (\(placeholders))",
                        arguments: preserveArgs
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

                return PersistStats(
                    inherited: matches.compactMap { $0 }.count,
                    lostNames: lostAnchorCount,
                    priors: freshPriors.count,
                    personCount: preservedPersonCount + finalClusters.count,
                    suppressedClusters: suppression.suppressedClusters,
                    suppressedFaces: suppression.suppressedFaces,
                    truncatedFaces: truncatedFaces
                )
            }
        } catch {
            JSONLog.shared.error(ev: "face_cluster_persist_failed", error: "\(error)")
            await sink.emit(.error(EngineError(
                kind: "face_cluster_persist_failed",
                message: "Could not write clusters: \(error)"
            )))
            let personCount = await currentPersonCount(database: database)
            return FaceClusteringResult(personCount: personCount, faceCount: decoded.count,
                                        unmatchedFaces: unmatched,
                                        durationSeconds: Date().timeIntervalSince(started))
        }
        unmatched += stats.suppressedFaces + stats.truncatedFaces
        if stats.suppressedClusters > 0 {
            JSONLog.shared.info(ev: "face_cluster_suppressed_lowq",
                                extra: ["suppressedClusters": AnyCodable(stats.suppressedClusters),
                                        "suppressedFaces": AnyCodable(stats.suppressedFaces),
                                        "minClusterSize": AnyCodable(minClusterSizeToKeep),
                                        "soloQualityFloor": AnyCodable(soloQualityFloor),
                                        "remainingClusters": AnyCodable(stats.personCount)])
        }
        if stats.truncatedFaces > 0 {
            JSONLog.shared.warn(ev: "face_cluster_truncated",
                                error: "IdentityClustering exceeded maxPersons (\(maxPersons)); \(stats.truncatedFaces) unprotected faces stayed unclustered.")
        }
        JSONLog.shared.info(ev: "face_cluster_built",
                            extra: ["faces": AnyCodable(decoded.count),
                                    "clusters": AnyCodable(stats.personCount),
                                    "cores": AnyCodable(icResult.coreCount),
                                    "outliersAssigned": AnyCodable(icResult.outliersAssigned),
                                    "outliersAsSingletons": AnyCodable(icResult.outliersAsSingletons),
                                    "splitsApplied": AnyCodable(icResult.splitsApplied),
                                    "unmatched": AnyCodable(unmatched),
                                    "buildSeconds": AnyCodable(icResult.durationSeconds)])
        let prePolishPersonCount = stats.personCount
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

    /// Eligible-person ceiling before centroid search. The exact metric index
    /// has independent distance-evaluation and qualifying-edge budgets, so a
    /// high-dimensional or dense corpus fails closed without a partial merge.
    private static let autoMergePersonCap = 20_000
    private static let autoMergeEmbeddingRowCap = 250_000
    private static let autoMergeEmbeddingByteCap: Int64 = 768 * 1024 * 1024
    private static let autoMergeSingleEmbeddingByteCap = 16 * 1024

    static func autoMergeInputWithinLimits(
        personCount: Int, embeddingCount: Int, embeddingBytes: Int64,
        maxEmbeddingBytes: Int
    ) -> Bool {
        personCount >= 0 && embeddingCount >= 0 && embeddingBytes >= 0 &&
            maxEmbeddingBytes >= 0 && personCount <= autoMergePersonCap &&
            embeddingCount <= autoMergeEmbeddingRowCap &&
            embeddingBytes <= autoMergeEmbeddingByteCap &&
            maxEmbeddingBytes <= autoMergeSingleEmbeddingByteCap
    }

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
    static func tightPairAutoMerge(
        database: Database,
        searchLimits: ExactCosineJoin.Limits = .autoMerge
    ) async -> Int {
        struct CentroidRow: Sendable {
            let personID: Int64
            let sum: [Float]
            let fileCount: Int
            let named: Bool
        }
        struct ReadData: Sendable {
            let rows: [CentroidRow]
            // "Different people" verdicts projected onto the persons that own
            // the anchor faces RIGHT NOW (after the phase-4 persist).
            let verdictPersonPairs: [(Int64, Int64)]
            let eligiblePersonCount: Int
            let embeddingCount: Int
            let embeddingBytes: Int64
            let maxEmbeddingBytes: Int
        }
        let data: ReadData
        do {
            data = try await database.pool.read { db -> ReadData in
                let input = try GRDB.Row.fetchOne(db, sql: """
                    SELECT COUNT(DISTINCT fp.person_id) AS persons,
                           COUNT(*) AS embeddings,
                           COALESCE(SUM(LENGTH(fp.arcface_embedding)), 0) AS embedding_bytes,
                           COALESCE(MAX(LENGTH(fp.arcface_embedding)), 0) AS max_embedding_bytes
                    FROM face_prints fp
                    INNER JOIN persons p ON p.id = fp.person_id
                    WHERE fp.person_id IS NOT NULL
                      AND LENGTH(fp.arcface_embedding) > 0
                      AND COALESCE(p.is_unknown, 0) = 0
                    """)
                let eligiblePersonCount: Int = input?["persons"] ?? 0
                let embeddingCount: Int = input?["embeddings"] ?? 0
                let embeddingBytes: Int64 = input?["embedding_bytes"] ?? 0
                let maxEmbeddingBytes: Int = input?["max_embedding_bytes"] ?? 0
                guard autoMergeInputWithinLimits(
                    personCount: eligiblePersonCount,
                    embeddingCount: embeddingCount,
                    embeddingBytes: embeddingBytes,
                    maxEmbeddingBytes: maxEmbeddingBytes) else {
                    return ReadData(rows: [], verdictPersonPairs: [],
                                    eligiblePersonCount: eligiblePersonCount,
                                    embeddingCount: embeddingCount,
                                    embeddingBytes: embeddingBytes,
                                    maxEmbeddingBytes: maxEmbeddingBytes)
                }
                // `named` = the user gave this cluster an identity (structured or
                // legacy name). is_unknown persons are excluded by the WHERE so
                // their "don't identify" verdict can never be merged away.
                let cursor = try GRDB.Row.fetchCursor(db, sql: """
                    SELECT fp.person_id AS pid, fp.arcface_embedding AS blob,
                           p.file_count AS fc,
                           (COALESCE(p.title,'') != ''
                             OR COALESCE(p.first_name,'') != ''
                             OR COALESCE(p.middle_name,'') != ''
                             OR COALESCE(p.last_name,'') != ''
                             OR COALESCE(p.suffix,'') != ''
                             OR COALESCE(p.name,'') != '') AS named
                    FROM face_prints fp
                    INNER JOIN persons p ON p.id = fp.person_id
                    WHERE fp.person_id IS NOT NULL
                      AND LENGTH(fp.arcface_embedding) > 0
                      AND COALESCE(p.is_unknown, 0) = 0
                    """)
                var sums: [Int64: (sum: [Float], fileCount: Int, named: Bool)] = [:]
                var dimension = 0
                while let row = try cursor.next() {
                    let personID: Int64 = row["pid"] ?? 0
                    let blob: Data = row["blob"] ?? Data()
                    guard blob.count <= autoMergeSingleEmbeddingByteCap,
                          blob.count.isMultiple(of: MemoryLayout<Float>.stride) else { continue }
                    let vector = ArcFaceService.blobToEmbedding(blob)
                    guard personID != 0, !vector.isEmpty,
                          vector.allSatisfy(\.isFinite) else { continue }
                    if dimension == 0 { dimension = vector.count }
                    guard vector.count == dimension else { continue }
                    var payload = sums[personID] ?? (
                        sum: [Float](repeating: 0, count: dimension),
                        fileCount: row["fc"] ?? 0,
                        named: (row["named"] ?? 0) != 0)
                    for index in 0..<dimension { payload.sum[index] += vector[index] }
                    sums[personID] = payload
                }
                let rows = sums.map { personID, payload in
                    CentroidRow(personID: personID, sum: payload.sum,
                                fileCount: payload.fileCount, named: payload.named)
                }

                let rawPairs = try Self.differentFacePairs(from: db)
                var verdictFaces = Set<Int64>()
                for pair in rawPairs {
                    verdictFaces.insert(pair.first)
                    verdictFaces.insert(pair.second)
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
                for pair in rawPairs {
                    if let personA = facePerson[pair.first],
                       let personB = facePerson[pair.second], personA != personB {
                        pairs.append((personA, personB))
                    }
                }
                return ReadData(rows: rows, verdictPersonPairs: pairs,
                                eligiblePersonCount: eligiblePersonCount,
                                embeddingCount: embeddingCount,
                                embeddingBytes: embeddingBytes,
                                maxEmbeddingBytes: maxEmbeddingBytes)
            }
        } catch {
            JSONLog.shared.warn(ev: "face_auto_merge_query_failed", error: "\(error)")
            return 0
        }
        if !autoMergeInputWithinLimits(
            personCount: data.eligiblePersonCount,
            embeddingCount: data.embeddingCount,
            embeddingBytes: data.embeddingBytes,
            maxEmbeddingBytes: data.maxEmbeddingBytes) {
            JSONLog.shared.info(ev: "face_auto_merge_skipped",
                                extra: ["persons": AnyCodable(data.eligiblePersonCount),
                                        "embeddings": AnyCodable(data.embeddingCount),
                                        "embeddingBytes": AnyCodable(data.embeddingBytes),
                                        "maxEmbeddingBytes": AnyCodable(data.maxEmbeddingBytes),
                                        "reason": AnyCodable("input_cap")])
            return 0
        }
        let rows = data.rows
        guard !rows.isEmpty else { return 0 }

        // L2-normalize the streamed per-person sums into centroids.
        struct Cluster { let id: Int64; let centroid: [Float]; let fileCount: Int; let named: Bool }
        guard let firstDim = rows.first?.sum.count, firstDim > 0, rows.count >= 2 else { return 0 }
        if rows.count > autoMergePersonCap {
            JSONLog.shared.info(ev: "face_auto_merge_skipped",
                                extra: ["persons": AnyCodable(rows.count),
                                        "cap": AnyCodable(autoMergePersonCap)])
            return 0
        }

        // Deterministic cluster order (sorted by person id) so the edge sweep,
        // union targets, and persist are stable across runs. (audit F-C3-007)
        var clusters: [Cluster] = []
        clusters.reserveCapacity(rows.count)
        for row in rows.sorted(by: { $0.personID < $1.personID }) {
            var sum = row.sum
            guard sum.count == firstDim else { continue }
            // L2-normalize so cosine = dot product downstream.
            var norm: Float = 0
            for x in sum { norm += x * x }
            guard norm.isFinite else { continue }
            let invN = Float(1) / max(.leastNonzeroMagnitude, norm.squareRoot())
            for i in 0..<firstDim { sum[i] *= invN }
            clusters.append(Cluster(id: row.personID, centroid: sum,
                                     fileCount: row.fileCount,
                                     named: row.named))
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
        // Explicit "different people" verdicts as sparse component adjacency.
        var forbidden = Array(repeating: Set<Int>(), count: clusters.count)
        for (personA, personB) in data.verdictPersonPairs {
            guard let indexA = idxOf[personA], let indexB = idxOf[personB] else { continue }
            forbidden[indexA].insert(indexB)
            forbidden[indexB].insert(indexA)
        }
        var componentSize = Array(repeating: 1, count: clusters.count)

        // Exact metric threshold join. For normalized centroids, a cosine
        // cutoff is an Euclidean radius; the VP tree prunes metric regions,
        // then every candidate is rechecked with the original scalar cosine
        // predicate. Small inputs retain the direct sweep. No union or DB write
        // occurs unless the complete qualifying edge set fits both budgets.
        let started = Date()
        let search = ExactCosineJoin.edges(
            vectors: clusters.map(\.centroid),
            small: clusters.map { $0.fileCount <= 1 },
            tightThreshold: tightAutoMergeCos,
            smallThreshold: smallClusterAutoMergeCos,
            limits: searchLimits)
        let edges: [ExactCosineEdge]
        let distanceEvaluations: Int
        switch search {
        case let .success(found, evaluations):
            edges = found
            distanceEvaluations = evaluations
        case let .limitExceeded(reason, evaluations):
            JSONLog.shared.info(ev: "face_auto_merge_skipped",
                                extra: ["persons": AnyCodable(clusters.count),
                                        "reason": AnyCodable(reason),
                                        "distanceEvaluations": AnyCodable(evaluations)])
            return 0
        }
        let pairCount = edges.count

        for edge in edges {
            let i = Int(edge.first), j = Int(edge.second)
            let ri = find(i), rj = find(j)
            if ri == rj { continue }
            if hasNamed[ri] && hasNamed[rj] { continue }
            if forbidden[ri].contains(rj) || forbidden[rj].contains(ri) { continue }
            let keep: Int
            let drop: Int
            if forbidden[ri].count > forbidden[rj].count
                || (forbidden[ri].count == forbidden[rj].count
                    && componentSize[ri] >= componentSize[rj]) {
                keep = ri; drop = rj
            } else {
                keep = rj; drop = ri
            }
            parent[drop] = keep
            componentSize[keep] += componentSize[drop]
            hasNamed[keep] = hasNamed[keep] || hasNamed[drop]
            let moved = forbidden[drop]
            forbidden[drop].removeAll(keepingCapacity: false)
            for neighbor in moved {
                let neighborRoot = find(neighbor)
                if neighborRoot == keep { continue }
                forbidden[neighborRoot].remove(drop)
                forbidden[neighborRoot].insert(keep)
                forbidden[keep].insert(neighborRoot)
            }
            forbidden[keep].remove(drop)
            forbidden[keep].remove(keep)
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
                                        "distanceEvaluations": AnyCodable(distanceEvaluations),
                                        "merged": AnyCodable(0),
                                        "seconds": AnyCodable(Date().timeIntervalSince(started))])
            return 0
        }

        // Snapshot to immutable lets so the Sendable write closure captures
        // by value (no concurrent-mutation warnings).
        let byTargetSnapshot: [(target: Int64, sources: [Int64])] =
            byTarget.map { (target: $0.key, sources: $0.value) }
        let targetIDs: [Int64] = byTargetSnapshot.map(\.target)
        let merged: Int
        do {
            merged = try await database.pool.write { db -> Int in
                // R3-11: re-read identity UNDER the writer lock before deleting. A
                // rename / mark-unknown the user committed during the lock-free
                // compute window (on macOS the app writes the persons table on its
                // own connection) must not be clobbered by the now-stale plan.
                // Mirrors the phase-4 persist's F-C3-002 under-lock re-read,
                // extended to the auto-merge polish path.
                let freshByID = Dictionary(uniqueKeysWithValues:
                    try Self.priorAnchors(from: db).map { ($0.id, $0) })
                var absorbed = 0
                for entry in byTargetSnapshot {
                    // Skip the group if the survivor vanished or became Unknown mid-window.
                    guard let freshTarget = freshByID[entry.target], !freshTarget.isUnknown else {
                        continue
                    }
                    let target = entry.target
                    // Drop any source that became named / unknown / deleted since
                    // the read — it is no longer an eligible merge source.
                    let sources = entry.sources.filter { sid in
                        guard let a = freshByID[sid] else { return false }
                        return !a.hasName
                    }
                    guard !sources.isEmpty else { continue }
                    absorbed += sources.count
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
                return absorbed
            }
        } catch {
            JSONLog.shared.error(ev: "face_auto_merge_persist_failed", error: "\(error)")
            return 0
        }

        JSONLog.shared.info(ev: "face_auto_merge_done",
                            extra: ["persons": AnyCodable(clusters.count),
                                    "pairsFound": AnyCodable(pairCount),
                                    "distanceEvaluations": AnyCodable(distanceEvaluations),
                                    "merged": AnyCodable(merged),
                                    "seconds": AnyCodable(Date().timeIntervalSince(started))])
        return merged
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

    static func protectedOutsidePoolOwnerIDs(
        priors: [PriorAnchor],
        protectedOwnerIDs: Set<Int64>,
        poolFaceIDs: Set<Int64>
    ) -> Set<Int64> {
        Set(priors.compactMap { prior in
            guard protectedOwnerIDs.contains(prior.id) else { return nil }
            return (prior.faceIDs.isEmpty || !prior.faceIDs.isSubset(of: poolFaceIDs))
                ? prior.id : nil
        })
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

    /// The cluster's representative face = its highest-quality member, so the People tab
    /// anchors on the sharpest / most-frontal crop. Mirrors the Windows anchor pick
    /// (face_clustering.rs `max_by quality`). Falls back to the first face id when no member
    /// carries a measured quality; strict `>` keeps the earliest of any ties so the rep is
    /// stable across runs. (audit parity)
    fileprivate static func representativeFaceID(
        _ faceIDs: [Int64], quality: [Int64: Double]
    ) -> Int64 {
        guard let first = faceIDs.first else { return 0 }
        var best = first
        var bestQ = quality[first] ?? -1
        for fid in faceIDs.dropFirst() {
            let q = quality[fid] ?? -1
            if q > bestQ { best = fid; bestQ = q }
        }
        return bestQ < 0 ? first : best
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
            // Require ≥ 50% of the prior's faces in this cluster (ceiling division).
            let threshold = max(1, (prior.faceIDs.count + 1) / 2)
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
                    // FaceAlign (opt-in): detect 5-point landmarks ONCE per image so
                    // each face can be similarity-ALIGNED to the SFace template
                    // (matching the Windows YuNet+align pipeline the thresholds assume)
                    // instead of a raw bbox crop. Default off → falls back to the bbox
                    // crop, behavior identical to before. (macOS lockstep)
                    let detected = FaceAlign.enabled ? detectFaceLandmarks(in: cg) : []
                    var aligned = 0
                    var out: [PendingExtract] = []
                    out.reserveCapacity(rows.count)
                    for row in rows {
                        var crop: CGImage?
                        if FaceAlign.enabled,
                           let pts = matchLandmarks(forBBox: row.bbox,
                                                    imageWidth: cg.width, imageHeight: cg.height,
                                                    in: detected),
                           let acrop = FaceAlign.align112(source: cg, landmarks: pts) {
                            crop = acrop
                            aligned += 1
                        } else {
                            crop = cropFaceCGImage(cgImage: cg, bboxString: row.bbox)
                        }
                        guard let crop else { continue }
                        saveFaceCrop(faceID: row.id, croppedCGImage: crop)
                        guard let vec = ArcFaceService.shared.embed(crop) else { continue }
                        out.append(PendingExtract(id: row.id,
                                                  arcFace: ArcFaceService.embeddingToBlob(vec)))
                    }
                    if FaceAlign.enabled {
                        JSONLog.shared.info(ev: "face_align_applied",
                                            extra: ["path": AnyCodable(redactPathForLog(path)),
                                                    "faces": AnyCodable(rows.count),
                                                    "detected": AnyCodable(detected.count),
                                                    "aligned": AnyCodable(aligned),
                                                    "bbox_fallback": AnyCodable(rows.count - aligned)])
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
    /// Detect 5-point face landmarks (FaceAlign opt-in). One
    /// VNDetectFaceLandmarksRequest on the full image → per detected face, its
    /// normalized (bottom-left) bbox center + the 5 landmarks in SOURCE-PIXEL
    /// top-left coords, FileID template order [hi-x eye, lo-x eye, nose, hi-x
    /// mouth corner, lo-x mouth corner]. Eye/mouth points are assigned to template
    /// slots by IMAGE-X (not Vision's subject/viewer naming) so they line up with
    /// the template's x-layout regardless of naming convention.
    private static func detectFaceLandmarks(
        in cg: CGImage
    ) -> [(center: CGPoint, points: [(Float, Float)])] {
        let req = VNDetectFaceLandmarksRequest()
        let handler = VNImageRequestHandler(cgImage: cg, options: [:])
        do {
            try handler.perform([req])
        } catch {
            JSONLog.shared.warn(ev: "face_align_detect_failed", error: "\(error)")
            return []
        }
        let size = CGSize(width: cg.width, height: cg.height)
        let imgH = Float(cg.height)
        func centroid(_ pts: [(Float, Float)]) -> (Float, Float)? {
            guard !pts.isEmpty else { return nil }
            var sx: Float = 0, sy: Float = 0
            for p in pts { sx += p.0; sy += p.1 }
            return (sx / Float(pts.count), sy / Float(pts.count))
        }
        var result: [(CGPoint, [(Float, Float)])] = []
        for obs in (req.results ?? []) {
            guard let lm = obs.landmarks,
                  let le = lm.leftEye, let re = lm.rightEye,
                  let nose = lm.nose, let lips = lm.outerLips else { continue }
            // pointsInImage → pixel coords, Vision bottom-left origin → flip Y to
            // top-left (consistent with cropFaceCGImage's `1 - y - h` flip).
            func tl(_ region: VNFaceLandmarkRegion2D) -> [(Float, Float)] {
                region.pointsInImage(imageSize: size).map { (Float($0.x), imgH - Float($0.y)) }
            }
            let lipsP = tl(lips)
            guard let eyeA = centroid(tl(le)),
                  let eyeB = centroid(tl(re)),
                  let noseC = centroid(tl(nose)),
                  let mouthHi = lipsP.max(by: { $0.0 < $1.0 }),
                  let mouthLo = lipsP.min(by: { $0.0 < $1.0 }) else { continue }
            let hiEye = eyeA.0 >= eyeB.0 ? eyeA : eyeB
            let loEye = eyeA.0 >= eyeB.0 ? eyeB : eyeA
            let five: [(Float, Float)] = [hiEye, loEye, noseC, mouthHi, mouthLo]
            result.append((CGPoint(x: obs.boundingBox.midX, y: obs.boundingBox.midY), five))
        }
        return result
    }

    /// Match a stored normalized (bottom-left) "x,y,w,h" bbox to the nearest
    /// detected face by center; returns its 5 landmarks only on a confident match
    /// (else nil → caller falls back to the bbox crop, never mis-aligns).
    private static func matchLandmarks(
        forBBox bboxString: String,
        imageWidth: Int, imageHeight: Int,
        in detected: [(center: CGPoint, points: [(Float, Float)])]
    ) -> [(Float, Float)]? {
        guard !detected.isEmpty,
              let b = FaceBBox.parseNormalized(bboxString, imageWidth: imageWidth, imageHeight: imageHeight)
        else { return nil }
        let cx = CGFloat(b.x + b.w / 2)
        let cy = CGFloat(b.y + b.h / 2)
        var best: (dist: CGFloat, pts: [(Float, Float)])?
        for d in detected {
            let dist = hypot(d.center.x - cx, d.center.y - cy)
            if best == nil || dist < best!.dist { best = (dist, d.points) }
        }
        // Centers within ~8% of the frame — a looser match risks aligning to the
        // wrong face in a group photo.
        if let b = best, b.dist < 0.08 { return b.pts }
        return nil
    }

    static func cropFaceCGImage(cgImage: CGImage, bboxString: String) -> CGImage? {
        guard let roi = parseBBox(bboxString, imageWidth: cgImage.width, imageHeight: cgImage.height) else { return nil }
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

    /// Parse a face bbox (macOS CSV normalized OR Windows JSON pixels — see
    /// FaceBBox) → normalized bottom-left CGRect with 15% padding (matches the
    /// historical v1 padding). Image dims convert the Windows pixel/top-left form;
    /// the CSV branch is unchanged, so a macOS-native library is byte-identical.
    private static func parseBBox(_ s: String, imageWidth: Int, imageHeight: Int) -> CGRect? {
        guard let b = FaceBBox.parseNormalized(s, imageWidth: imageWidth, imageHeight: imageHeight) else { return nil }
        let pad: CGFloat = 0.15
        let bx = CGFloat(b.x); let by = CGFloat(b.y)
        let bw = CGFloat(b.w); let bh = CGFloat(b.h)
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
