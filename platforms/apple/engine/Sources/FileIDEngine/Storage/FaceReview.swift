import Foundation
import GRDB
import FileIDShared

enum FaceReview {
    struct Settings: Sendable, Equatable {
        var suggestCosine: Float = 0.55
        var minimumCosine: Float = 0.40
        var margin: Float = 0.05
        var autoAssign = false

        static func fromEnvironment() -> Settings {
            Settings(
                suggestCosine: FaceClustering.envCos("FILEID_FACE_REVIEW_SUGGEST_COS", 0.55),
                minimumCosine: FaceClustering.envCos("FILEID_FACE_REVIEW_MIN_COS", 0.40),
                margin: FaceClustering.envCos("FILEID_FACE_REVIEW_MARGIN", 0.05),
                autoAssign: FaceClustering.envBool("FILEID_FACE_REVIEW_AUTO_ASSIGN", false))
        }
    }

    static let pendingOutcomes = ["suggested", "uncertain", "ambiguous", "rejected"]
    static let reviewReasons: Set<String> = ["suggested", "uncertain", "ambiguous"]
    private static let pendingSQL = "('suggested','uncertain','ambiguous','rejected')"
    static let unavailableMessage = "The library database is unavailable."

    static func namespace(model: String, processing: String) -> String { model + "|" + processing }

    static func round4(_ value: Double) -> Double { (value * 10_000).rounded() / 10_000 }

    // MARK: - Holdout

    static func holdout(_ db: GRDB.Database, model: String, processing: String, minQuality: Double) throws -> Set<Int64> {
        let incremental = try Bool.fetchOne(db, sql: """
            SELECT EXISTS(SELECT 1 FROM face_prints WHERE person_id IS NOT NULL AND embedding_model=? AND processing_version=?)
            """, arguments: [model, processing]) ?? false
        let rows = try Row.fetchAll(db, sql: """
            SELECT fp.id, a.outcome FROM face_prints fp
            LEFT JOIN face_assignment_assessed a ON a.face_id=fp.id
            JOIN catalog_revisions r ON r.file_id=fp.file_id
            WHERE fp.person_id IS NULL AND length(fp.arcface_embedding)=512
              AND fp.embedding_model=? AND fp.processing_version=? AND fp.source_revision=r.revision
              AND (a.face_id IS NULL OR a.outcome IN \(pendingSQL))
              AND (? <= 0 OR COALESCE(fp.face_quality,-1) >= ?)
            """, arguments: [model, processing, minQuality, minQuality])
        var ids = Set<Int64>()
        for row in rows {
            let outcome: String? = row["outcome"]
            if incremental || outcome != nil { ids.insert(row["id"]) }
        }
        return ids
    }

    // MARK: - After a clustering pass

    static func finish(database: Database, pool: [Int64], holdout: Set<Int64>, model: String, processing: String,
                       settings: Settings = .fromEnvironment(), queues: FaceQueues.Settings = .fromEnvironment()) async -> Int? {
        do {
            return try await database.pool.write { db in
                let now = Date().timeIntervalSince1970
                try markPool(db, faceIDs: pool, now: now)
                try assess(db, faceIDs: holdout, model: model, processing: processing, settings: settings, now: now)
                let stored = try FaceQueues.refresh(db, model: model, processing: processing, settings: queues, now: now)
                JSONLog.shared.info(ev: "face_queues_stored", extra: ["merges": AnyCodable(stored.merges), "splits": AnyCodable(stored.splits)])
                return try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM face_review_candidates") ?? 0
            }
        } catch {
            JSONLog.shared.error(ev: "face_review_assess_failed", error: "\(error)")
            return nil
        }
    }

    static func markPool(_ db: GRDB.Database, faceIDs: [Int64], now: Double) throws {
        guard !faceIDs.isEmpty else { return }
        try db.execute(sql: """
            INSERT OR IGNORE INTO face_assignment_assessed(face_id,namespace,outcome,assessed_at)
            SELECT id, COALESCE(embedding_model,'') || '|' || COALESCE(processing_version,''),
                   CASE WHEN person_id IS NULL THEN 'unmatched' ELSE 'clustered' END, ?
            FROM face_prints WHERE id IN (SELECT value FROM json_each(?))
            """, arguments: [now, json(faceIDs)])
    }

    // MARK: - Assessment

    struct Receiver: Sendable {
        let id: Int64
        let unknown: Bool
        let centroid: [Float]
    }

    struct Face: Sendable {
        let id: Int64
        let fileID: Int64
        let bbox: String
        let vector: [Float]
        var held = false
    }

    struct Scored: Sendable {
        let person: Int64
        let score: Float
        let unknown: Bool
    }

    struct Decision: Equatable, Sendable {
        var outcome: String
        var person: Int64?
        var score: Float?
        var runnerUp: Int64?
        var runnerUpScore: Float?
    }

    static func normalized(_ values: [Float]) -> [Float]? {
        var sum: Float = 0
        for value in values {
            guard value.isFinite else { return nil }
            sum += value * value
        }
        let norm = sum.squareRoot()
        guard norm.isFinite, norm > Float.leastNormalMagnitude else { return nil }
        return values.map { $0 / norm }
    }

    static func dot(_ a: [Float], _ b: [Float]) -> Float {
        var sum: Float = 0
        for index in 0..<min(a.count, b.count) { sum += a[index] * b[index] }
        return sum
    }

    static func loadReceivers(_ db: GRDB.Database, model: String, processing: String) throws -> [Receiver] {
        let rows = try Row.fetchAll(db, sql: """
            SELECT fp.person_id, fp.arcface_embedding, COALESCE(p.is_unknown,0) AS unknown
            FROM face_prints fp JOIN persons p ON p.id=fp.person_id
            WHERE fp.person_id IS NOT NULL AND COALESCE(fp.excluded,0)=0 AND length(fp.arcface_embedding)=512
              AND fp.embedding_model=? AND fp.processing_version=?
            ORDER BY fp.id
            """, arguments: [model, processing])
        var sums: [Int64: [Float]] = [:]
        var unknown: [Int64: Bool] = [:]
        for row in rows {
            guard let vector = normalized(FloatBlob.decode(row["arcface_embedding"])) else { continue }
            let person: Int64 = row["person_id"]
            unknown[person] = (row["unknown"] as Int64) != 0
            if var sum = sums[person], sum.count == vector.count {
                for index in sum.indices { sum[index] += vector[index] }
                sums[person] = sum
            } else if sums[person] == nil {
                sums[person] = vector
            }
        }
        return sums.keys.sorted().compactMap { person in
            normalized(sums[person]!).map { Receiver(id: person, unknown: unknown[person] ?? false, centroid: $0) }
        }
    }

    static func loadFaces(_ db: GRDB.Database, ids: Set<Int64>, model: String, processing: String) throws -> [Face] {
        guard !ids.isEmpty else { return [] }
        return try Row.fetchAll(db, sql: """
            SELECT id, file_id, bbox, arcface_embedding, COALESCE(excluded,0) AS held FROM face_prints
            WHERE id IN (SELECT value FROM json_each(?)) AND person_id IS NULL
              AND length(arcface_embedding)=512 AND embedding_model=? AND processing_version=?
            ORDER BY id
            """, arguments: [json(Array(ids)), model, processing]).compactMap { row in
            normalized(FloatBlob.decode(row["arcface_embedding"])).map {
                Face(id: row["id"], fileID: row["file_id"], bbox: row["bbox"], vector: $0, held: (row["held"] as Int64) != 0)
            }
        }
    }

    static func personsAt(_ db: GRDB.Database, fileID: Int64, bbox: String) throws -> [Int64] {
        try Int64.fetchAll(db, sql: "SELECT person_id FROM face_prints WHERE file_id=? AND bbox=? AND person_id IS NOT NULL",
                           arguments: [fileID, bbox])
    }

    static func staticBars(_ db: GRDB.Database, face: Face) throws -> Set<Int64> {
        var barred = Set(try Int64.fetchAll(db, sql: """
            SELECT person_id FROM face_prints WHERE file_id=? AND id<>? AND person_id IS NOT NULL
            """, arguments: [face.fileID, face.id]))
        for row in try Row.fetchAll(db, sql: """
            SELECT anchor_file_id, anchor_bbox FROM face_identity_constraints WHERE file_id=? AND bbox=?
            """, arguments: [face.fileID, face.bbox]) {
            barred.formUnion(try personsAt(db, fileID: row["anchor_file_id"], bbox: row["anchor_bbox"]))
        }
        for row in try Row.fetchAll(db, sql: """
            SELECT face_a, face_b, file_a, bbox_a, file_b, bbox_b FROM face_verifications WHERE same_person=0
              AND (face_a=?1 OR face_b=?1 OR (file_a=?2 AND bbox_a=?3) OR (file_b=?2 AND bbox_b=?3))
            """, arguments: [face.id, face.fileID, face.bbox]) {
            let isA = (row["face_a"] as Int64?) == face.id
                || ((row["file_a"] as Int64?) == face.fileID && (row["bbox_a"] as String?) == face.bbox)
            let other = isA ? "b" : "a"
            barred.formUnion(try verdictOwners(db, legacy: row["face_\(other)"], fileID: row["file_\(other)"], bbox: row["bbox_\(other)"]))
        }
        return barred
    }

    static func verdictOwners(_ db: GRDB.Database, legacy: Int64?, fileID: Int64?, bbox: String?) throws -> [Int64] {
        if let fileID, let bbox,
           try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM face_prints WHERE file_id=? AND bbox=?)", arguments: [fileID, bbox]) == true {
            return try personsAt(db, fileID: fileID, bbox: bbox)
        }
        guard let legacy else { return [] }
        return try Int64.fetchAll(db, sql: "SELECT person_id FROM face_prints WHERE id=? AND person_id IS NOT NULL", arguments: [legacy])
    }

    static func scores(_ face: Face, receivers: [Receiver]) -> [Scored] {
        receivers.map { Scored(person: $0.id, score: dot(face.vector, $0.centroid), unknown: $0.unknown) }
            .sorted { $0.score != $1.score ? $0.score > $1.score : $0.person < $1.person }
    }

    static func bestEligible(_ scored: [Scored], barred: Set<Int64>) -> Float? {
        scored.first { !$0.unknown && !barred.contains($0.person) }?.score
    }

    static func decide(_ scored: [Scored], barred: Set<Int64>, settings: Settings) -> Decision {
        let unmatched = Decision(outcome: "unmatched")
        guard let top = scored.first, !top.unknown,
              let chosen = scored.firstIndex(where: { !$0.unknown && !barred.contains($0.person) }) else { return unmatched }
        let best = scored[chosen]
        let runner = scored.indices.first { $0 != chosen }.map { scored[$0] }
        let margin = runner.map { best.score - $0.score } ?? .infinity
        let outcome: String
        if best.score < settings.minimumCosine { return unmatched }
        if margin < settings.margin { outcome = "ambiguous" }
        else if best.score >= settings.suggestCosine { outcome = settings.autoAssign ? "assigned" : "suggested" }
        else { outcome = "uncertain" }
        return Decision(outcome: outcome, person: best.person, score: best.score,
                        runnerUp: runner?.person, runnerUpScore: runner?.score)
    }

    /// A face that failed the per-face quality gate is only ever an uncertain
    /// suggestion: never suggested, never auto-assigned.
    static func capped(_ decision: Decision, held: Bool) -> Decision {
        guard held, decision.outcome == "suggested" || decision.outcome == "assigned" else { return decision }
        var capped = decision
        capped.outcome = "uncertain"
        return capped
    }

    static func assess(_ db: GRDB.Database, faceIDs: Set<Int64>, model: String, processing: String,
                       settings: Settings, now: Double) throws {
        let faces = try loadFaces(db, ids: faceIDs, model: model, processing: processing)
        guard !faces.isEmpty else { return }
        let receivers = try loadReceivers(db, model: model, processing: processing)
        let space = namespace(model: model, processing: processing)
        var prepared: [(face: Face, scored: [Scored], barred: Set<Int64>, best: Float?)] = []
        for face in faces {
            let scored = scores(face, receivers: receivers)
            let barred = try staticBars(db, face: face)
            prepared.append((face, scored, barred, bestEligible(scored, barred: barred)))
        }
        prepared.sort { lhs, rhs in
            switch (lhs.best, rhs.best) {
            case let (l?, r?): return l != r ? l > r : lhs.face.id < rhs.face.id
            case (.some, nil): return true
            case (nil, .some): return false
            case (nil, nil): return lhs.face.id < rhs.face.id
            }
        }
        var claimed: [Int64: Set<Int64>] = [:]
        var received = Set<Int64>()
        for item in prepared {
            let decision = capped(decide(item.scored, barred: item.barred.union(claimed[item.face.fileID] ?? []), settings: settings),
                                  held: item.face.held)
            if let person = decision.person, decision.outcome == "suggested" || decision.outcome == "assigned" {
                claimed[item.face.fileID, default: []].insert(person)
            }
            try write(db, face: item.face, decision: decision, namespace: space, now: now)
            if decision.outcome == "assigned", let person = decision.person { received.insert(person) }
        }
        for person in received.sorted() {
            try db.execute(sql: """
                UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?) WHERE id=?
                """, arguments: [person, person])
        }
    }

    private static func write(_ db: GRDB.Database, face: Face, decision: Decision, namespace: String, now: Double) throws {
        let previous = try Row.fetchOne(db, sql: "SELECT person_id, created_at FROM face_review_candidates WHERE face_id=?", arguments: [face.id])
        let previousOutcome = try String.fetchOne(db, sql: "SELECT outcome FROM face_assignment_assessed WHERE face_id=?", arguments: [face.id])
        try db.execute(sql: "DELETE FROM face_review_candidates WHERE face_id=?", arguments: [face.id])
        var outcome = decision.outcome
        if reviewReasons.contains(outcome), let person = decision.person, let score = decision.score {
            let created: Double = (previous?["person_id"] as Int64?) == person ? (previous?["created_at"] ?? now) : now
            try db.execute(sql: """
                INSERT INTO face_review_candidates(face_id,person_id,score,reason,runner_up_person_id,runner_up_score,namespace,created_at)
                VALUES(?,?,?,?,?,?,?,?)
                """, arguments: [face.id, person, round4(Double(score)), outcome, decision.runnerUp,
                                 decision.runnerUpScore.map { round4(Double($0)) }, namespace, created])
        } else if outcome == "assigned", let person = decision.person {
            try db.execute(sql: "UPDATE face_prints SET person_id=? WHERE id=?", arguments: [person, face.id])
            try db.execute(sql: "UPDATE persons SET representative_face_id=COALESCE(representative_face_id, ?) WHERE id=?",
                           arguments: [face.id, person])
        } else if outcome == "unmatched", previousOutcome == "rejected" {
            outcome = "rejected"
        }
        try upsertOutcome(db, faceID: face.id, namespace: namespace, outcome: outcome, now: now)
    }

    static func upsertOutcome(_ db: GRDB.Database, faceID: Int64, namespace: String, outcome: String, now: Double) throws {
        try db.execute(sql: """
            INSERT INTO face_assignment_assessed(face_id,namespace,outcome,assessed_at) VALUES(?,?,?,?)
            ON CONFLICT(face_id) DO UPDATE SET namespace=excluded.namespace,outcome=excluded.outcome,assessed_at=excluded.assessed_at
            """, arguments: [faceID, namespace, outcome, now])
    }

    // MARK: - List

    static func display(_ alias: String) -> String {
        "COALESCE(NULLIF(TRIM(\(alias).name),''), NULLIF(TRIM(COALESCE(\(alias).first_name,'') || ' ' || COALESCE(\(alias).last_name,'')),''))"
    }

    private static let pendingFrom = """
        FROM face_review_candidates c JOIN face_prints fp ON fp.id=c.face_id JOIN files f ON f.id=fp.file_id
        JOIN persons p ON p.id=c.person_id LEFT JOIN persons rp ON rp.id=c.runner_up_person_id
        WHERE fp.person_id IS NULL AND c.namespace = fp.embedding_model || '|' || fp.processing_version
          AND (?1 IS NULL OR c.person_id=?1)
        """

    static func pendingTotal(_ db: GRDB.Database, personID: Int64? = nil) throws -> Int {
        try Int.fetchOne(db, sql: "SELECT COUNT(*) " + pendingFrom, arguments: [personID]) ?? 0
    }

    static let pageRefusal = "Ask for 1 to 200 suggestions from offset 0 or later."
    static let queueRefusal = "Choose the faces, merges, splits or refresh queue."

    static func pageBounds(offset: Int?, limit: Int?) -> (offset: Int, limit: Int, valid: Bool) {
        let offset = offset ?? 0
        let limit = limit ?? 50
        return (offset, limit, offset >= 0 && (1...200).contains(limit))
    }

    static func list(_ db: GRDB.Database, requestID: String, personID: Int64?, offset: Int?, limit: Int?,
                     queue: String? = nil) throws -> FaceReviewPage {
        switch queue {
        case nil, "faces": return try listFaces(db, requestID: requestID, personID: personID, offset: offset, limit: limit)
        case "merges": return try FaceQueues.listMerges(db, requestID: requestID, personID: personID, offset: offset, limit: limit)
        case "splits": return try FaceQueues.listSplits(db, requestID: requestID, personID: personID, offset: offset, limit: limit)
        case "refresh": return try FaceRefreshJob.statusPage(db, requestID: requestID)
        default:
            return FaceReviewPage(requestID: requestID, action: "list", status: "refused", message: queueRefusal, total: 0, offset: 0)
        }
    }

    static func listFaces(_ db: GRDB.Database, requestID: String, personID: Int64?, offset: Int?, limit: Int?) throws -> FaceReviewPage {
        let (offset, limit, valid) = pageBounds(offset: offset, limit: limit)
        guard valid else {
            return FaceReviewPage(requestID: requestID, action: "list", status: "refused", message: pageRefusal,
                                  total: 0, offset: offset)
        }
        let rows = try Row.fetchAll(db, sql: """
            SELECT c.face_id, fp.file_id, f.path_text, fp.bbox, c.person_id, \(display("p")) AS person_name,
                   p.representative_face_id, c.score, c.reason, c.runner_up_person_id, \(display("rp")) AS runner_name,
                   c.runner_up_score, c.created_at, COALESCE(fp.excluded,0) AS held
            """ + " " + pendingFrom + " ORDER BY c.score DESC, c.face_id ASC LIMIT ?2 OFFSET ?3",
            arguments: [personID, limit, offset])
        let items = rows.map { row in
            FaceReviewItem(faceID: row["face_id"], fileID: row["file_id"], path: row["path_text"], bbox: row["bbox"],
                           personID: row["person_id"], personName: row["person_name"],
                           representativeFaceID: row["representative_face_id"], score: row["score"], reason: row["reason"],
                           runnerUpPersonID: row["runner_up_person_id"], runnerUpName: row["runner_name"],
                           runnerUpScore: row["runner_up_score"], createdAt: row["created_at"],
                           lowQuality: (row["held"] as Int64) != 0 ? true : nil)
        }
        return FaceReviewPage(requestID: requestID, action: "list", status: "ok",
                              total: try pendingTotal(db, personID: personID), offset: offset, items: items)
    }

    // MARK: - Resolve

    static let staleMessage = "This suggestion changed. Refresh the review list."
    static let resolutionMessage = "Choose accept, reject, ignore, acceptAll, merge, notSamePerson, dismiss or split."
    static let faceRequiredMessage = "Choose a face to resolve."
    static let nothingToAcceptMessage = "Nothing to accept for this person."
    static let allInPhotoMessage = "Every suggested face is in a photo that already has this person, so nothing changed."
    static let movedManyMessage = "Some of these faces were moved since, so nothing changed."
    static let samePhotoMessage = "That person is already in this photo."
    static let noAnchorMessage = "This person has no face to compare with."
    static let nothingToUndoMessage = "There is nothing to undo for this suggestion."
    static let missingFaceMessage = "This face no longer exists."
    static let movedMessage = "This face was moved since, so nothing changed."

    private static func refused(_ requestID: String, _ action: String, _ message: String, db: GRDB.Database) throws -> FaceReviewPage {
        FaceReviewPage(requestID: requestID, action: action, status: "refused", message: message,
                       total: try pendingTotal(db), offset: 0)
    }

    static func resolve(_ db: GRDB.Database, requestID: String, faceID: Int64?, personID: Int64, resolution: String,
                        otherPersonID: Int64? = nil, now: Double = Date().timeIntervalSince1970) throws -> FaceReviewPage {
        switch resolution {
        case "accept", "reject", "ignore":
            guard let faceID else { return try refused(requestID, "resolve", faceRequiredMessage, db: db) }
            return try resolveFace(db, requestID: requestID, faceID: faceID, personID: personID, resolution: resolution, now: now)
        case "acceptAll":
            return try acceptAll(db, requestID: requestID, personID: personID, now: now)
        case "merge", "notSamePerson", "dismiss", "split":
            return try FaceQueues.resolve(db, requestID: requestID, personID: personID, otherPersonID: otherPersonID,
                                          resolution: resolution, now: now)
        default:
            return try refused(requestID, "resolve", resolutionMessage, db: db)
        }
    }

    static func resolveFace(_ db: GRDB.Database, requestID: String, faceID: Int64, personID: Int64, resolution: String,
                            now: Double) throws -> FaceReviewPage {
        guard let candidate = try Row.fetchOne(db, sql: """
                SELECT c.person_id, c.score, c.reason, c.runner_up_person_id, c.runner_up_score, c.namespace, c.created_at,
                       fp.file_id, fp.bbox
                FROM face_review_candidates c JOIN face_prints fp ON fp.id=c.face_id
                WHERE c.face_id=? AND c.person_id=? AND fp.person_id IS NULL
                """, arguments: [faceID, personID]) else {
            return try refused(requestID, "resolve", staleMessage, db: db)
        }
        let fileID: Int64 = candidate["file_id"]
        let bbox: String = candidate["bbox"]
        var newPerson: Int64?
        var constraintID: Int64?
        var constraintCreated = false
        let outcome: String
        switch resolution {
        case "accept":
            let present = try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM face_prints WHERE file_id=? AND id<>? AND person_id=?)",
                                            arguments: [fileID, faceID, personID]) ?? false
            if present { return try refused(requestID, "resolve", samePhotoMessage, db: db) }
            try db.execute(sql: "UPDATE face_prints SET person_id=? WHERE id=?", arguments: [personID, faceID])
            try db.execute(sql: """
                UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?1),
                    representative_face_id=COALESCE(representative_face_id, ?2)
                WHERE id=?1
                """, arguments: [personID, faceID])
            newPerson = personID
            outcome = "accepted"
        case "reject":
            guard let anchor = try anchorFace(db, personID: personID) else {
                return try refused(requestID, "resolve", noAnchorMessage, db: db)
            }
            try db.execute(sql: """
                INSERT OR IGNORE INTO face_identity_constraints(file_id,bbox,anchor_file_id,anchor_bbox,source,created_at)
                VALUES(?,?,?,?,'review',?)
                """, arguments: [fileID, bbox, anchor.fileID, anchor.bbox, now])
            constraintCreated = db.changesCount > 0
            constraintID = try Int64.fetchOne(db, sql: """
                SELECT id FROM face_identity_constraints WHERE file_id=? AND bbox=? AND anchor_file_id=? AND anchor_bbox=?
                """, arguments: [fileID, bbox, anchor.fileID, anchor.bbox])
            outcome = "rejected"
        default:
            try db.execute(sql: """
                INSERT INTO persons(name,is_unknown,created_at,representative_face_id,file_count) VALUES(NULL,1,?,?,1)
                """, arguments: [now, faceID])
            let created = db.lastInsertedRowID
            try db.execute(sql: "UPDATE face_prints SET person_id=? WHERE id=?", arguments: [created, faceID])
            newPerson = created
            outcome = "ignored"
        }
        let previousOutcome = try String.fetchOne(db, sql: "SELECT outcome FROM face_assignment_assessed WHERE face_id=?", arguments: [faceID])
        let space: String = candidate["namespace"]
        try db.execute(sql: "DELETE FROM face_review_candidates WHERE face_id=?", arguments: [faceID])
        try upsertOutcome(db, faceID: faceID, namespace: space, outcome: outcome, now: now)
        let before: [String: Any] = [
            "faceID": faceID,
            "outcome": previousOutcome as Any? ?? NSNull(),
            "candidate": [
                "personID": personID,
                "score": candidate["score"] as Double,
                "reason": candidate["reason"] as String,
                "runnerUpPersonID": (candidate["runner_up_person_id"] as Int64?) as Any? ?? NSNull(),
                "runnerUpScore": (candidate["runner_up_score"] as Double?) as Any? ?? NSNull(),
                "namespace": space,
                "createdAt": candidate["created_at"] as Double,
            ] as [String: Any],
        ]
        let after: [String: Any] = [
            "faceID": faceID,
            "resolution": resolution,
            "personID": newPerson as Any? ?? NSNull(),
            "constraintID": constraintID as Any? ?? NSNull(),
            "constraintCreated": constraintCreated,
        ]
        let operationID = UUID().uuidString.lowercased()
        let beforeJSON = try jsonText(before)
        try db.execute(sql: "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?,?,'face_review',?,?,?)",
                       arguments: [operationID, fileID, beforeJSON, try jsonText(after), now])
        try db.execute(sql: "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?,?,?,'completed',?)",
                       arguments: [operationID, try jsonText(["type": "faceReview", "faceID": faceID, "resolution": resolution]),
                                   beforeJSON, now])
        return FaceReviewPage(requestID: requestID, action: "resolve", status: "ok", operationID: operationID,
                              faceID: faceID, personID: newPerson, total: try pendingTotal(db), offset: 0)
    }

    static func anchorFace(_ db: GRDB.Database, personID: Int64) throws -> (fileID: Int64, bbox: String)? {
        let row = try Row.fetchOne(db, sql: """
            SELECT fp.file_id, fp.bbox FROM persons p JOIN face_prints fp ON fp.id=p.representative_face_id WHERE p.id=?
            """, arguments: [personID]) ?? Row.fetchOne(db, sql: """
            SELECT file_id, bbox FROM face_prints WHERE person_id=? ORDER BY COALESCE(face_quality,0) DESC, id ASC LIMIT 1
            """, arguments: [personID])
        return row.map { ($0["file_id"], $0["bbox"]) }
    }

    // MARK: - Undo

    static func undo(_ db: GRDB.Database, requestID: String, operationID: String,
                     now: Double = Date().timeIntervalSince1970) throws -> FaceReviewPage {
        guard let row = try Row.fetchOne(db, sql: """
                SELECT c.kind, c.before_json, c.after_json FROM catalog_operations o JOIN catalog_corrections c ON c.id=o.id
                WHERE o.id=? AND o.state='completed'
                  AND c.kind IN ('face_review','face_review_bulk','face_merge_review','face_split_review')
                """, arguments: [operationID]),
              let before = parse(row["before_json"]), let after = parse(row["after_json"]) else {
            return try refused(requestID, "undo", nothingToUndoMessage, db: db)
        }
        let kind: String = row["kind"]
        let reply: FaceReviewPage
        switch kind {
        case "face_review_bulk": reply = try undoAcceptAll(db, requestID: requestID, before: before, after: after, now: now)
        case "face_merge_review", "face_split_review":
            reply = try FaceQueues.undo(db, requestID: requestID, before: before, after: after, now: now)
        default: reply = try undoFace(db, requestID: requestID, before: before, after: after, now: now)
        }
        guard reply.status == "ok" else { return reply }
        try db.execute(sql: "UPDATE catalog_operations SET state='undone' WHERE id=?", arguments: [operationID])
        return reply.withOperation(operationID)
    }

    private static func undoFace(_ db: GRDB.Database, requestID: String, before: [String: Any], after: [String: Any],
                                 now: Double) throws -> FaceReviewPage {
        guard let faceID = int(before["faceID"]), let candidate = before["candidate"] as? [String: Any],
              let resolution = after["resolution"] as? String else {
            return try refused(requestID, "undo", nothingToUndoMessage, db: db)
        }
        guard let face = try Row.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id=?", arguments: [faceID]) else {
            return try refused(requestID, "undo", missingFaceMessage, db: db)
        }
        let owner: Int64? = face["person_id"]
        let assigned = int(after["personID"])
        switch resolution {
        case "accept", "ignore":
            guard let assigned, owner == assigned else { return try refused(requestID, "undo", movedMessage, db: db) }
            try db.execute(sql: "UPDATE face_prints SET person_id=NULL WHERE id=?", arguments: [faceID])
            if resolution == "accept" {
                try recomputeAfterRemoval(db, personID: assigned, faceID: faceID)
            } else {
                try db.execute(sql: """
                    DELETE FROM persons WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM face_prints WHERE person_id=?1)
                      AND COALESCE(TRIM(name),'')='' AND COALESCE(TRIM(first_name),'')='' AND COALESCE(TRIM(last_name),'')=''
                    """, arguments: [assigned])
            }
        default:
            guard owner == nil else { return try refused(requestID, "undo", movedMessage, db: db) }
            if bool(after["constraintCreated"]), let constraint = int(after["constraintID"]) {
                try db.execute(sql: "DELETE FROM face_identity_constraints WHERE id=?", arguments: [constraint])
            }
        }
        try restore(db, faceID: faceID, candidate: candidate, now: now)
        return FaceReviewPage(requestID: requestID, action: "undo", status: "ok", faceID: faceID, total: try pendingTotal(db), offset: 0)
    }

    // MARK: - Accept all

    static func candidateEntry(_ db: GRDB.Database, faceID: Int64) throws -> (fileID: Int64, entry: [String: Any])? {
        guard let row = try Row.fetchOne(db, sql: """
            SELECT fp.file_id, c.person_id, c.score, c.reason, c.runner_up_person_id, c.runner_up_score, c.namespace, c.created_at, a.outcome
            FROM face_review_candidates c JOIN face_prints fp ON fp.id=c.face_id
            LEFT JOIN face_assignment_assessed a ON a.face_id=c.face_id WHERE c.face_id=?
            """, arguments: [faceID]) else { return nil }
        let candidate: [String: Any] = [
            "personID": row["person_id"] as Int64,
            "score": row["score"] as Double,
            "reason": row["reason"] as String,
            "runnerUpPersonID": (row["runner_up_person_id"] as Int64?) as Any? ?? NSNull(),
            "runnerUpScore": (row["runner_up_score"] as Double?) as Any? ?? NSNull(),
            "namespace": row["namespace"] as String,
            "createdAt": row["created_at"] as Double,
        ]
        return (row["file_id"], ["faceID": faceID, "outcome": (row["outcome"] as String?) as Any? ?? NSNull(), "candidate": candidate])
    }

    static func acceptAll(_ db: GRDB.Database, requestID: String, personID: Int64, now: Double) throws -> FaceReviewPage {
        let rows = try Row.fetchAll(db, sql: "SELECT c.face_id, fp.file_id " + pendingFrom
            + " AND c.reason='suggested' ORDER BY c.score DESC, c.face_id ASC", arguments: [personID])
        guard !rows.isEmpty else {
            return try refused(requestID, "resolve", nothingToAcceptMessage, db: db).withPerson(personID)
        }
        var accepted: [Int64] = [], skipped: [Int64] = [], entries: [[String: Any]] = []
        var files = Set<Int64>()
        var firstFile: Int64?
        for row in rows {
            let faceID: Int64 = row["face_id"]
            let fileID: Int64 = row["file_id"]
            let present = try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM face_prints WHERE file_id=? AND person_id=? AND id<>?)",
                                            arguments: [fileID, personID, faceID]) ?? false
            if present || files.contains(fileID) { skipped.append(faceID); continue }
            guard let (_, entry) = try candidateEntry(db, faceID: faceID) else { continue }
            let namespace = (entry["candidate"] as? [String: Any])?["namespace"] as? String ?? ""
            try db.execute(sql: "UPDATE face_prints SET person_id=? WHERE id=? AND person_id IS NULL", arguments: [personID, faceID])
            try db.execute(sql: "DELETE FROM face_review_candidates WHERE face_id=?", arguments: [faceID])
            try upsertOutcome(db, faceID: faceID, namespace: namespace, outcome: "accepted", now: now)
            files.insert(fileID)
            if firstFile == nil { firstFile = fileID }
            accepted.append(faceID)
            entries.append(entry)
        }
        guard let first = accepted.first else {
            return FaceReviewPage(requestID: requestID, action: "resolve", status: "refused", message: allInPhotoMessage,
                                  personID: personID, total: try pendingTotal(db), offset: 0,
                                  acceptedFaceIDs: [], skippedFaceIDs: skipped)
        }
        try db.execute(sql: """
            UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?1),
                representative_face_id=COALESCE(representative_face_id, ?2) WHERE id=?1
            """, arguments: [personID, first])
        let operationID = UUID().uuidString.lowercased()
        let beforeJSON = try jsonText(["personID": personID, "faces": entries])
        let after: [String: Any] = ["personID": personID, "resolution": "acceptAll", "acceptedFaceIDs": accepted, "skippedFaceIDs": skipped]
        try db.execute(sql: "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?,?,'face_review_bulk',?,?,?)",
                       arguments: [operationID, firstFile, beforeJSON, try jsonText(after), now])
        try db.execute(sql: "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?,?,?,'completed',?)",
                       arguments: [operationID, try jsonText(["type": "faceReviewBulk", "personID": personID, "resolution": "acceptAll",
                                                              "count": accepted.count]), beforeJSON, now])
        return FaceReviewPage(requestID: requestID, action: "resolve", status: "ok", operationID: operationID, personID: personID,
                              total: try pendingTotal(db), offset: 0, acceptedFaceIDs: accepted, skippedFaceIDs: skipped)
    }

    private static func undoAcceptAll(_ db: GRDB.Database, requestID: String, before: [String: Any], after: [String: Any],
                                      now: Double) throws -> FaceReviewPage {
        guard let personID = int(after["personID"]), let list = after["acceptedFaceIDs"] as? [Any] else {
            return try refused(requestID, "undo", nothingToUndoMessage, db: db)
        }
        let faces = list.compactMap { int($0) }
        for face in faces where try Int64.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id=?", arguments: [face]) != personID {
            return try refused(requestID, "undo", movedManyMessage, db: db)
        }
        for face in faces {
            try db.execute(sql: "UPDATE face_prints SET person_id=NULL WHERE id=?", arguments: [face])
        }
        try recount(db, personID: personID)
        for entry in before["faces"] as? [[String: Any]] ?? [] {
            if let face = int(entry["faceID"]), let candidate = entry["candidate"] as? [String: Any] {
                try restore(db, faceID: face, candidate: candidate, now: now)
            }
        }
        return FaceReviewPage(requestID: requestID, action: "undo", status: "ok", personID: personID,
                              total: try pendingTotal(db), offset: 0, acceptedFaceIDs: faces)
    }

    /// Recomputes a person's file count, keeping its representative face when it
    /// still belongs to the person (else the best embedded face).
    static func recount(_ db: GRDB.Database, personID: Int64) throws {
        try db.execute(sql: """
            UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?1),
                representative_face_id=CASE WHEN representative_face_id IS NOT NULL
                    AND EXISTS(SELECT 1 FROM face_prints WHERE id=persons.representative_face_id AND person_id=?1)
                  THEN representative_face_id ELSE COALESCE(
                    (SELECT fp.id FROM face_prints fp WHERE fp.person_id=?1 AND fp.arcface_embedding IS NOT NULL
                     ORDER BY COALESCE(fp.face_quality,0) DESC LIMIT 1),
                    (SELECT fp.id FROM face_prints fp WHERE fp.person_id=?1 ORDER BY COALESCE(fp.face_quality,0) DESC LIMIT 1)) END
            WHERE id=?1
            """, arguments: [personID])
    }

    private static func recomputeAfterRemoval(_ db: GRDB.Database, personID: Int64, faceID: Int64) throws {
        try db.execute(sql: """
            UPDATE persons
            SET file_count = (SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id = ?1),
                representative_face_id = CASE WHEN representative_face_id = ?2 THEN COALESCE(
                    (SELECT fp.id FROM face_prints fp WHERE fp.person_id = ?1 AND fp.arcface_embedding IS NOT NULL
                     ORDER BY COALESCE(fp.face_quality, 0) DESC LIMIT 1),
                    (SELECT fp.id FROM face_prints fp WHERE fp.person_id = ?1
                     ORDER BY COALESCE(fp.face_quality, 0) DESC LIMIT 1)) ELSE representative_face_id END
            WHERE id = ?1
            """, arguments: [personID, faceID])
    }

    static func restore(_ db: GRDB.Database, faceID: Int64, candidate: [String: Any], now: Double) throws {
        guard let reason = candidate["reason"] as? String, reviewReasons.contains(reason) else { return }
        let space = candidate["namespace"] as? String ?? ""
        if let person = int(candidate["personID"]),
           try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM persons WHERE id=?)", arguments: [person]) == true {
            var runner = int(candidate["runnerUpPersonID"])
            if let id = runner, try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM persons WHERE id=?)", arguments: [id]) != true {
                runner = nil
            }
            try db.execute(sql: """
                INSERT OR REPLACE INTO face_review_candidates(face_id,person_id,score,reason,runner_up_person_id,runner_up_score,namespace,created_at)
                VALUES(?,?,?,?,?,?,?,?)
                """, arguments: [faceID, person, double(candidate["score"]) ?? 0, reason, runner,
                                 double(candidate["runnerUpScore"]), space, double(candidate["createdAt"]) ?? now])
        }
        try upsertOutcome(db, faceID: faceID, namespace: space, outcome: reason, now: now)
    }

    // MARK: - Manual reassignment

    static func recordReassignment(_ db: GRDB.Database, faceID: Int64, previous: Int64?, destination: Int64?,
                                   now: Double = Date().timeIntervalSince1970) throws {
        guard previous != destination,
              let face = try Row.fetchOne(db, sql: """
                SELECT file_id, bbox, COALESCE(embedding_model,'') || '|' || COALESCE(processing_version,'') AS ns
                FROM face_prints WHERE id=?
                """, arguments: [faceID]) else { return }
        let fileID: Int64 = face["file_id"]
        let bbox: String = face["bbox"]
        if let previous, let anchor = try Row.fetchOne(db, sql: """
                SELECT fp.id, fp.file_id, fp.bbox FROM persons p JOIN face_prints fp ON fp.id=p.representative_face_id WHERE p.id=?
                """, arguments: [previous]), (anchor["id"] as Int64) != faceID {
            try db.execute(sql: """
                INSERT OR IGNORE INTO face_identity_constraints(file_id,bbox,anchor_file_id,anchor_bbox,source,created_at)
                VALUES(?,?,?,?,'reassign',?)
                """, arguments: [fileID, bbox, anchor["file_id"] as Int64, anchor["bbox"] as String, now])
        }
        try db.execute(sql: "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?,?,'face_reassign',?,?,?)",
                       arguments: [UUID().uuidString.lowercased(), fileID,
                                   try jsonText(["faceID": faceID, "personID": previous as Any? ?? NSNull()]),
                                   try jsonText(["faceID": faceID, "personID": destination as Any? ?? NSNull()]), now])
        try db.execute(sql: "DELETE FROM face_review_candidates WHERE face_id=?", arguments: [faceID])
        try upsertOutcome(db, faceID: faceID, namespace: face["ns"], outcome: "manual", now: now)
    }

    // MARK: - IPC

    enum Command: Sendable {
        case list(requestID: String, personID: Int64?, offset: Int?, limit: Int?, queue: String? = nil)
        case resolve(requestID: String, faceID: Int64?, personID: Int64, resolution: String, otherPersonID: Int64? = nil)
        case undo(requestID: String, operationID: String)

        var requestID: String {
            switch self {
            case .list(let id, _, _, _, _), .resolve(let id, _, _, _, _), .undo(let id, _): return id
            }
        }

        var action: String {
            switch self {
            case .list: return "list"
            case .resolve: return "resolve"
            case .undo: return "undo"
            }
        }
    }

    static func reply(_ command: Command, database: Database?) async -> FaceReviewPage {
        let requestID = command.requestID
        guard let database else {
            return FaceReviewPage(requestID: requestID, action: command.action, status: "failed",
                                  message: unavailableMessage, total: 0, offset: 0)
        }
        do {
            switch command {
            case .list(_, let personID, let offset, let limit, let queue):
                return try await database.pool.read {
                    try list($0, requestID: requestID, personID: personID, offset: offset, limit: limit, queue: queue)
                }
            case .resolve(_, let faceID, let personID, let resolution, let otherPersonID):
                return try await database.pool.write {
                    try resolve($0, requestID: requestID, faceID: faceID, personID: personID, resolution: resolution,
                                otherPersonID: otherPersonID)
                }
            case .undo(_, let operationID):
                return try await database.pool.write { try undo($0, requestID: requestID, operationID: operationID) }
            }
        } catch {
            JSONLog.shared.error(ev: "face_review_failed", error: "\(error)")
            let message = command.action == "list" ? "The review list could not be read." : "The review could not be saved."
            return FaceReviewPage(requestID: requestID, action: command.action, status: "failed", message: message,
                                  total: 0, offset: 0)
        }
    }

    // MARK: - JSON helpers

    private static func json(_ ids: [Int64]) -> String {
        "[" + ids.sorted().map(String.init).joined(separator: ",") + "]"
    }

    static func jsonText(_ object: [String: Any]) throws -> String {
        String(decoding: try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), as: UTF8.self)
    }

    static func parse(_ text: String?) -> [String: Any]? {
        guard let text else { return nil }
        return (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any]
    }

    static func int(_ value: Any?) -> Int64? {
        guard let number = value as? NSNumber, !(value is NSNull) else { return nil }
        return number.int64Value
    }

    static func double(_ value: Any?) -> Double? {
        (value as? NSNumber)?.doubleValue
    }

    static func bool(_ value: Any?) -> Bool {
        (value as? NSNumber)?.boolValue ?? false
    }
}

extension FaceReviewPage {
    func withOperation(_ operationID: String) -> FaceReviewPage {
        FaceReviewPage(requestID: requestID, action: action, status: status, message: message, operationID: operationID,
                       faceID: faceID, personID: personID, total: total, offset: offset, items: items, queue: queue,
                       merges: merges, splits: splits, refresh: refresh, otherPersonID: otherPersonID,
                       acceptedFaceIDs: acceptedFaceIDs, skippedFaceIDs: skippedFaceIDs)
    }

    func withPerson(_ personID: Int64) -> FaceReviewPage {
        FaceReviewPage(requestID: requestID, action: action, status: status, message: message, operationID: operationID,
                       faceID: faceID, personID: personID, total: total, offset: offset, items: items, queue: queue,
                       merges: merges, splits: splits, refresh: refresh, otherPersonID: otherPersonID,
                       acceptedFaceIDs: acceptedFaceIDs, skippedFaceIDs: skippedFaceIDs)
    }
}
