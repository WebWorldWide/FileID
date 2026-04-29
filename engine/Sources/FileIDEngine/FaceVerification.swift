// FaceVerification — VLM-driven merging on the borderline L2 band.
//
// For each cluster pair whose face-print centroids land in the band,
// the local VLM compares the two representative face crops and emits
// a SAME/DIFFERENT verdict + confidence. High-confidence SAME pairs
// auto-merge; results are persisted in `face_verifications`.
import Foundation
import GRDB
import Vision
import FileIDShared

public enum FaceVerification {

    /// Centroid L2 band for candidate pairs. Below the floor is already
    /// merged by the bootstrap pass; above the ceiling is reliably
    /// different-person.
    private static let borderlineMin: Float = 0.30
    private static let borderlineMax: Float = 0.65

    /// VLM confidence floor for auto-merging. Conservative to avoid
    /// false-merging similar-looking people (e.g. siblings).
    private static let autoMergeConfidence: Float = 0.75

    /// Wall-time safety net: cap actual VLM calls per run. Each call
    /// is ~150-300 ms.
    private static let maxVLMCallsPerPass: Int = 2000

    /// Cap on bounded candidate pairs (sorted closest-L2 first).
    private static let maxPairsPerPass: Int = 50000

    /// Single pass. Subsequent passes pick up transitive merges but
    /// at high marginal cost; users re-run if they want more.
    private static let maxPasses: Int = 1

    public static func run(
        database: Database, sink: IPCSink, modelKind: AIModelKind
    ) async -> VLMFaceVerificationResult {
        let start = Date()

        // Load the VLM up front. Failure here means the whole job
        // can't proceed; bail with a zero-result.
        do {
            try await DeepAnalyze.shared.ensureLoaded(kind: modelKind)
        } catch {
            await sink.emit(.error(EngineError(
                kind: "vlm_verify_load_failed",
                message: "Could not load \(modelKind.displayName): \(error.localizedDescription)"
            )))
            return VLMFaceVerificationResult(pairsExamined: 0,
                                              pairsConfirmedSame: 0,
                                              pairsMerged: 0,
                                              durationSeconds: 0)
        }

        var totalExamined = 0
        var totalConfirmed = 0
        var totalMerged = 0

        for pass in 1...maxPasses {
            let pairs = await borderlinePairs(database: database)
            JSONLog.shared.info(ev: "vlm_verify_pass_start", extra: [
                "pass": AnyCodable(pass),
                "pairs": AnyCodable(pairs.count)
            ])
            guard !pairs.isEmpty else { break }

            let bounded = Array(pairs.prefix(maxPairsPerPass))
            let repFaces = await representativeFaceIDs(
                database: database,
                personIDs: bounded.flatMap { [$0.a, $0.b] }
            )

            await sink.emit(.vlmFaceVerificationProgress(
                VLMFaceVerificationProgress(
                    pairsExamined: 0, pairsTotal: bounded.count,
                    mergedSoFar: totalMerged, etaSeconds: nil
                )
            ))

            var mergesThisPass = 0
            var mergedAway: Set<Int64> = []
            var examinedThisPass = 0
            var processedThisPass = 0
            var lastProgressEmit = Date()
            let passStart = Date()

            // Batched verification persistence — every flushBatchSize
            // calls we drain the buffer in one transaction.
            var verifyBuffer: [(a: Int64, b: Int64, same: Int, conf: Double)] = []
            let flushBatchSize = 50

            for pair in bounded {
                processedThisPass += 1
                if mergedAway.contains(pair.a) || mergedAway.contains(pair.b) { continue }
                guard let faceA = repFaces[pair.a], let faceB = repFaces[pair.b] else { continue }
                let cropA = FaceClustering.faceCropURL(faceID: faceA)
                let cropB = FaceClustering.faceCropURL(faceID: faceB)
                guard FileManager.default.fileExists(atPath: cropA.path),
                      FileManager.default.fileExists(atPath: cropB.path) else {
                    continue
                }

                if examinedThisPass >= maxVLMCallsPerPass { break }

                let result = await DeepAnalyze.shared.compareFaces(cropA: cropA, cropB: cropB)
                totalExamined += 1
                examinedThisPass += 1
                if result.sameClass { totalConfirmed += 1 }

                // ~1Hz progress throttle. ETA from rolling per-iteration
                // average — mixes cheap skips with VLM calls for an
                // honest estimate.
                if Date().timeIntervalSince(lastProgressEmit) >= 1.0 {
                    lastProgressEmit = Date()
                    let elapsed = Date().timeIntervalSince(passStart)
                    let eta: Double?
                    if processedThisPass >= 10 {
                        let avgPerIter = elapsed / Double(processedThisPass)
                        let remaining = max(0, bounded.count - processedThisPass)
                        eta = avgPerIter * Double(remaining)
                    } else {
                        eta = nil
                    }
                    await sink.emit(.vlmFaceVerificationProgress(
                        VLMFaceVerificationProgress(
                            pairsExamined: processedThisPass,
                            pairsTotal: bounded.count,
                            mergedSoFar: totalMerged + mergesThisPass,
                            etaSeconds: eta
                        )
                    ))
                }

                verifyBuffer.append((
                    a: min(pair.a, pair.b),
                    b: max(pair.a, pair.b),
                    same: result.sameClass ? 1 : 0,
                    conf: Double(result.confidence)
                ))
                if verifyBuffer.count >= flushBatchSize {
                    let snapshot = verifyBuffer
                    verifyBuffer.removeAll(keepingCapacity: true)
                    await Self.flushVerifyBuffer(database: database,
                                                  rows: snapshot,
                                                  modelKind: modelKind)
                }

                // Auto-merge: pick the more-photographed cluster as
                // target so it inherits the bigger sample.
                if result.sameClass && result.confidence >= autoMergeConfidence {
                    let target: Int64
                    let source: Int64
                    if pair.aFileCount >= pair.bFileCount {
                        target = pair.a; source = pair.b
                    } else {
                        target = pair.b; source = pair.a
                    }
                    do {
                        _ = try await database.mergePersons(target: target, sources: [source])
                        mergedAway.insert(source)
                        mergesThisPass += 1
                        totalMerged += 1
                    } catch {
                        JSONLog.shared.error(ev: "vlm_auto_merge_failed", error: "\(error)")
                    }
                }
            }

            if !verifyBuffer.isEmpty {
                let snapshot = verifyBuffer
                verifyBuffer.removeAll(keepingCapacity: true)
                await Self.flushVerifyBuffer(database: database,
                                              rows: snapshot,
                                              modelKind: modelKind)
            }

            JSONLog.shared.info(ev: "vlm_verify_pass_done", extra: [
                "pass": AnyCodable(pass),
                "merges": AnyCodable(mergesThisPass)
            ])
            if mergesThisPass == 0 { break }
        }

        let elapsed = Date().timeIntervalSince(start)
        JSONLog.shared.info(ev: "vlm_verify_done", extra: [
            "examined": AnyCodable(totalExamined),
            "confirmed_same": AnyCodable(totalConfirmed),
            "merged": AnyCodable(totalMerged),
            "seconds": AnyCodable(elapsed)
        ])
        return VLMFaceVerificationResult(pairsExamined: totalExamined,
                                          pairsConfirmedSame: totalConfirmed,
                                          pairsMerged: totalMerged,
                                          durationSeconds: elapsed)
    }

    /// Persist a batch of face_verifications rows in a single
    /// transaction. Called every flushBatchSize comparisons (and once
    /// more at the end of the pass to drain the tail).
    private static func flushVerifyBuffer(
        database: Database,
        rows: [(a: Int64, b: Int64, same: Int, conf: Double)],
        modelKind: AIModelKind
    ) async {
        guard !rows.isEmpty else { return }
        do {
            try await database.pool.write { db in
                let now = Date().timeIntervalSince1970
                for r in rows {
                    try db.execute(sql: """
                        INSERT OR REPLACE INTO face_verifications
                        (person_a, person_b, same_person, confidence, vlm_model, verified_at)
                        VALUES (?, ?, ?, ?, ?, ?)
                        """, arguments: [
                            r.a, r.b, r.same, r.conf,
                            modelKind.rawValue, now
                        ])
                }
            }
        } catch {
            JSONLog.shared.error(ev: "vlm_verify_persist_failed", error: "\(error)")
        }
    }

    public struct CandidatePair: Sendable {
        public let a: Int64
        public let b: Int64
        public let distance: Float
        public let aFileCount: Int
        public let bFileCount: Int
    }

    /// Pairs of clusters whose centroid L2 falls in the borderline band,
    /// sorted ascending by distance (closest matches first). Excludes
    /// clusters the user has flagged as `is_unknown`.
    private static func borderlinePairs(database: Database) async -> [CandidatePair] {
        struct PrintRow { let pid: Int64; let print: Data }
        let rows: [PrintRow] = (try? await database.pool.read { db in
            let raw = try GRDB.Row.fetchAll(db, sql: """
                SELECT face_prints.person_id AS pid, face_prints.print_data AS print
                FROM face_prints
                INNER JOIN persons ON persons.id = face_prints.person_id
                WHERE face_prints.person_id IS NOT NULL
                  AND LENGTH(face_prints.print_data) > 0
                  AND IFNULL(persons.is_unknown, 0) = 0
                """)
            return raw.map { PrintRow(pid: $0["pid"] ?? 0, print: $0["print"] ?? Data()) }
        }) ?? []
        guard !rows.isEmpty else { return [] }

        let fileCounts: [Int64: Int] = (try? await database.pool.read { db in
            let raw = try GRDB.Row.fetchAll(db, sql:
                "SELECT id, file_count FROM persons WHERE IFNULL(is_unknown, 0) = 0")
            var out: [Int64: Int] = [:]
            for r in raw {
                if let id: Int64 = r["id"], let fc: Int = r["file_count"] {
                    out[id] = fc
                }
            }
            return out
        }) ?? [:]

        var byPerson: [Int64: [[Float]]] = [:]
        var dim = 0
        for r in rows {
            guard let v = decode(r.print) else { continue }
            if dim == 0 { dim = v.count }
            if v.count == dim { byPerson[r.pid, default: []].append(v) }
        }
        guard byPerson.count >= 2 else { return [] }

        var centroids: [(Int64, [Float])] = []
        centroids.reserveCapacity(byPerson.count)
        for (pid, vecs) in byPerson {
            var sum = [Float](repeating: 0, count: dim)
            for v in vecs { for i in 0..<dim { sum[i] += v[i] } }
            let n = Float(vecs.count)
            centroids.append((pid, sum.map { $0 / n }))
        }

        var pairs: [CandidatePair] = []
        for i in 0..<centroids.count {
            for j in (i+1)..<centroids.count {
                let d = l2(centroids[i].1, centroids[j].1)
                if d >= borderlineMin && d <= borderlineMax {
                    let a = min(centroids[i].0, centroids[j].0)
                    let b = max(centroids[i].0, centroids[j].0)
                    pairs.append(CandidatePair(
                        a: a, b: b, distance: d,
                        aFileCount: fileCounts[a] ?? 0,
                        bFileCount: fileCounts[b] ?? 0
                    ))
                }
            }
        }
        pairs.sort { $0.distance < $1.distance }
        return pairs
    }

    private static func representativeFaceIDs(database: Database,
                                              personIDs: [Int64]) async -> [Int64: Int64] {
        let unique = Array(Set(personIDs))
        guard !unique.isEmpty else { return [:] }
        let placeholders = unique.map { _ in "?" }.joined(separator: ",")
        let rows: [GRDB.Row] = (try? await database.pool.read { db in
            try GRDB.Row.fetchAll(db, sql: """
                SELECT id, representative_face_id FROM persons
                WHERE id IN (\(placeholders)) AND representative_face_id IS NOT NULL
                """, arguments: StatementArguments(unique))
        }) ?? []
        var out: [Int64: Int64] = [:]
        for r in rows {
            if let pid: Int64 = r["id"], let face: Int64 = r["representative_face_id"] {
                out[pid] = face
            }
        }
        return out
    }

    private static func decode(_ data: Data) -> [Float]? {
        guard let obs = try? NSKeyedUnarchiver.unarchivedObject(
            ofClass: VNFeaturePrintObservation.self, from: data
        ) else { return nil }
        let count = obs.elementCount
        guard count >= 64, count <= 4096 else { return nil }
        var out = [Float](repeating: 0, count: count)
        obs.data.withUnsafeBytes { ptr in
            let fp = ptr.bindMemory(to: Float.self)
            let n = min(count, fp.count)
            for i in 0..<n { out[i] = fp[i] }
        }
        return out
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
}
