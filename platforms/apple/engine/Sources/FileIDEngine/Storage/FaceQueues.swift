import Foundation
import GRDB
import FileIDShared

/// Durable merge and split suggestion queues for People. Mirrored by the Rust
/// engine's commands/face_queues.rs; keep thresholds, ordering, messages and
/// journal shapes in step (cross-engine conformance reads both journals).
enum FaceQueues {
    struct Settings: Sendable, Equatable {
        var mergeMinimum: Double = 0.55
        var splitMaximum: Double = 0.45
        var splitMinimumFaces = 6
        var splitMinimumCohesion: Double = 0.5

        static func fromEnvironment() -> Settings {
            let environment = ProcessInfo.processInfo.environment
            func read(_ key: String, _ fallback: Double) -> Double {
                guard let value = environment[key].flatMap({ Double($0.trimmingCharacters(in: .whitespaces)) }),
                      value.isFinite else { return fallback }
                return min(max(value, -1), 1)
            }
            let defaults = Settings()
            let faces = environment["FILEID_FACE_SPLIT_MIN_FACES"].flatMap { Int($0.trimmingCharacters(in: .whitespaces)) }
            return Settings(mergeMinimum: read("FILEID_FACE_MERGE_SUGGEST_COS", defaults.mergeMinimum),
                            splitMaximum: read("FILEID_FACE_SPLIT_MAX_COS", defaults.splitMaximum),
                            splitMinimumFaces: faces.map { min(max($0, 4), 1000) } ?? defaults.splitMinimumFaces,
                            splitMinimumCohesion: read("FILEID_FACE_SPLIT_MIN_COHESION", defaults.splitMinimumCohesion))
        }
    }

    static let sharedPhotoMessage = "These two people share a photo, so they can't be one person."
    static let markedDifferentMessage = "These two people were marked as different."
    static let otherRequiredMessage = "Choose the other person in this merge suggestion."
    static let renamedMessage = "The new person was named since, so nothing changed."
    static let maxMerges = 200
    static let maxSplits = 100
    static let maxSplitFaces = 2000
    static let preview = 8

    struct Member {
        let face: Int64
        let quality: Double
        let vector: [Float]
    }

    struct Person {
        let id: Int64
        let named: Bool
        let faces: Int
        let representative: Int64?
        var members: [Member]
    }

    struct Pair: Hashable {
        let a: Int64
        let b: Int64
        init(_ x: Int64, _ y: Int64) { a = min(x, y); b = max(x, y) }
    }

    static func dot(_ a: [Float], _ b: [Float]) -> Double { Double(FaceReview.dot(a, b)) }

    static func centroid(_ vectors: [[Float]]) -> [Float]? {
        guard var sum = vectors.first else { return nil }
        for vector in vectors.dropFirst() where vector.count == sum.count {
            for index in sum.indices { sum[index] += vector[index] }
        }
        return FaceReview.normalized(sum)
    }

    static func people(_ db: GRDB.Database, model: String, processing: String) throws -> [Person] {
        let rows = try Row.fetchAll(db, sql: """
            SELECT p.id AS pid, \(FaceReview.display("p")) IS NOT NULL AS named,
                   (SELECT COUNT(*) FROM face_prints a WHERE a.person_id=p.id) AS faces, p.representative_face_id AS rep,
                   fp.id AS fid, COALESCE(fp.face_quality,0) AS quality, fp.arcface_embedding AS embedding
            FROM face_prints fp JOIN persons p ON p.id=fp.person_id
            WHERE COALESCE(p.is_unknown,0)=0 AND COALESCE(fp.excluded,0)=0 AND length(fp.arcface_embedding)=512
              AND fp.embedding_model=? AND fp.processing_version=? ORDER BY p.id, fp.id
            """, arguments: [model, processing])
        var out: [Person] = []
        for row in rows {
            guard let vector = FaceReview.normalized(FloatBlob.decode(row["embedding"])) else { continue }
            let id: Int64 = row["pid"]
            let member = Member(face: row["fid"], quality: row["quality"], vector: vector)
            if out.last?.id == id {
                out[out.count - 1].members.append(member)
            } else {
                out.append(Person(id: id, named: (row["named"] as Int64) != 0, faces: row["faces"], representative: row["rep"],
                                  members: [member]))
            }
        }
        return out
    }

    static func owner(_ db: GRDB.Database, legacy: Int64?, fileID: Int64?, bbox: String?) throws -> Int64? {
        if let fileID, let bbox,
           let found = try Row.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE file_id=? AND bbox=? ORDER BY id LIMIT 1",
                                        arguments: [fileID, bbox]) {
            return found["person_id"]
        }
        guard let legacy else { return nil }
        return try Int64.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id=?", arguments: [legacy])
    }

    static func blockedPairs(_ db: GRDB.Database) throws -> Set<Pair> {
        var blocked = Set<Pair>()
        for row in try Row.fetchAll(db, sql: "SELECT face_a, file_a, bbox_a, face_b, file_b, bbox_b FROM face_verifications WHERE same_person=0") {
            if let x = try owner(db, legacy: row["face_a"], fileID: row["file_a"], bbox: row["bbox_a"]),
               let y = try owner(db, legacy: row["face_b"], fileID: row["file_b"], bbox: row["bbox_b"]), x != y {
                blocked.insert(Pair(x, y))
            }
        }
        for row in try Row.fetchAll(db, sql: """
            SELECT file_id, bbox, anchor_file_id AS other_file, anchor_bbox AS other_bbox FROM face_identity_constraints
            UNION ALL SELECT file_id, bbox, other_file_id, other_bbox FROM face_queue_dismissals WHERE kind='merge'
            """) {
            if let x = try owner(db, legacy: nil, fileID: row["file_id"], bbox: row["bbox"]),
               let y = try owner(db, legacy: nil, fileID: row["other_file"], bbox: row["other_bbox"]), x != y {
                blocked.insert(Pair(x, y))
            }
        }
        return blocked
    }

    static func filesByPerson(_ db: GRDB.Database) throws -> [Int64: Set<Int64>] {
        var out: [Int64: Set<Int64>] = [:]
        for row in try Row.fetchAll(db, sql: "SELECT DISTINCT person_id, file_id FROM face_prints WHERE person_id IS NOT NULL") {
            out[row["person_id"], default: []].insert(row["file_id"])
        }
        return out
    }

    static func keepsFirst(_ a: Person, _ b: Person) -> Bool {
        if a.named != b.named { return a.named }
        if a.faces != b.faces { return a.faces > b.faces }
        return a.id <= b.id
    }

    struct MergeRow: Equatable {
        let keep: Int64
        let other: Int64
        let score: Double
    }

    static func merges(_ people: [Person], blocked: Set<Pair>, files: [Int64: Set<Int64>], settings: Settings) -> [MergeRow] {
        let centroids = people.map { centroid($0.members.map(\.vector)) }
        var out: [MergeRow] = []
        for i in people.indices {
            guard let a = centroids[i] else { continue }
            for j in people.indices where j > i {
                guard let b = centroids[j], a.count == b.count else { continue }
                let score = dot(a, b)
                guard score >= settings.mergeMinimum, !blocked.contains(Pair(people[i].id, people[j].id)),
                      (files[people[i].id] ?? []).isDisjoint(with: files[people[j].id] ?? []) else { continue }
                let (keep, other) = keepsFirst(people[i], people[j]) ? (people[i], people[j]) : (people[j], people[i])
                out.append(MergeRow(keep: keep.id, other: other.id, score: FaceReview.round4(score)))
            }
        }
        out.sort { $0.score != $1.score ? $0.score > $1.score : ($0.keep != $1.keep ? $0.keep < $1.keep : $0.other < $1.other) }
        return Array(out.prefix(maxMerges))
    }

    struct SplitRow: Equatable {
        let person: Int64
        let score: Double
        let keep: [Int64]
        let split: [Int64]
    }

    /// Deterministic two-means over a person's unit vectors. Seeds are the face
    /// farthest from the centroid and the face farthest from that one.
    static func twoGroups(_ vectors: [[Float]]) -> (groups: [Int], first: [Float], second: [Float])? {
        guard let whole = centroid(vectors) else { return nil }
        func farthest(_ from: [Float]) -> Int? {
            var best: (index: Int, score: Double)?
            for (index, vector) in vectors.enumerated() {
                let score = dot(vector, from)
                if best == nil || score < best!.score { best = (index, score) }
            }
            return best?.index
        }
        guard let a = farthest(whole), let b = farthest(vectors[a]), a != b else { return nil }
        var centers = (vectors[a], vectors[b])
        var groups = [Int](repeating: -1, count: vectors.count)
        for _ in 0..<20 {
            let next = vectors.map { dot($0, centers.0) < dot($0, centers.1) ? 1 : 0 }
            if next == groups { break }
            groups = next
            guard let first = centroid(zip(vectors, groups).filter { $0.1 == 0 }.map(\.0)),
                  let second = centroid(zip(vectors, groups).filter { $0.1 == 1 }.map(\.0)) else { return nil }
            centers = (first, second)
        }
        return (groups, centers.0, centers.1)
    }

    static func split(of person: Person, settings: Settings) -> SplitRow? {
        let members = Array(person.members.prefix(maxSplitFaces))
        let n = members.count
        guard n >= settings.splitMinimumFaces else { return nil }
        let vectors = members.map(\.vector)
        guard let (groups, first, second) = twoGroups(vectors) else { return nil }
        let minimumGroup = max(3, (n + 9) / 10)
        let sizes = (groups.filter { $0 == 0 }.count, groups.filter { $0 == 1 }.count)
        guard sizes.0 >= minimumGroup, sizes.1 >= minimumGroup else { return nil }
        let between = dot(first, second)
        guard between <= settings.splitMaximum else { return nil }
        func cohesion(_ group: Int, _ center: [Float]) -> Double {
            let scores = zip(vectors, groups).filter { $0.1 == group }.map { dot($0.0, center) }
            return scores.reduce(0, +) / Double(scores.count)
        }
        guard min(cohesion(0, first), cohesion(1, second)) >= settings.splitMinimumCohesion else { return nil }
        var parts: [[Member]] = [[], []]
        for (member, group) in zip(members, groups) { parts[group].append(member) }
        for index in parts.indices {
            parts[index].sort { $0.quality != $1.quality ? $0.quality > $1.quality : $0.face < $1.face }
        }
        func holds(_ part: [Member]) -> Bool { person.representative.map { rep in part.contains { $0.face == rep } } ?? false }
        let keepIndex: Int
        if holds(parts[0]) { keepIndex = 0 }
        else if holds(parts[1]) { keepIndex = 1 }
        else if parts[0].count != parts[1].count { keepIndex = parts[1].count > parts[0].count ? 1 : 0 }
        else { keepIndex = (parts[1].map(\.face).min() ?? .max) < (parts[0].map(\.face).min() ?? .max) ? 1 : 0 }
        return SplitRow(person: person.id, score: FaceReview.round4(between), keep: parts[keepIndex].map(\.face),
                        split: parts[1 - keepIndex].map(\.face))
    }

    static func resolveFace(_ db: GRDB.Database, legacy: Int64?, fileID: Int64?, bbox: String?) throws -> Int64? {
        if let fileID, let bbox,
           let id = try Int64.fetchOne(db, sql: "SELECT id FROM face_prints WHERE file_id=? AND bbox=? ORDER BY id LIMIT 1",
                                       arguments: [fileID, bbox]) {
            return id
        }
        guard let legacy else { return nil }
        return try Int64.fetchOne(db, sql: "SELECT id FROM face_prints WHERE id=?", arguments: [legacy])
    }

    static func splitBlocks(_ db: GRDB.Database) throws -> (linked: [(Int64, Int64)], dismissed: Set<Int64>) {
        var linked: [(Int64, Int64)] = []
        for row in try Row.fetchAll(db, sql: "SELECT face_a, file_a, bbox_a, face_b, file_b, bbox_b FROM face_verifications WHERE same_person=1") {
            if let x = try resolveFace(db, legacy: row["face_a"], fileID: row["file_a"], bbox: row["bbox_a"]),
               let y = try resolveFace(db, legacy: row["face_b"], fileID: row["file_b"], bbox: row["bbox_b"]) {
                linked.append((x, y))
            }
        }
        var dismissed = Set<Int64>()
        for row in try Row.fetchAll(db, sql: "SELECT file_id, bbox FROM face_queue_dismissals WHERE kind='split'") {
            if let owner = try owner(db, legacy: nil, fileID: row["file_id"], bbox: row["bbox"]) { dismissed.insert(owner) }
        }
        return (linked, dismissed)
    }

    /// Rebuilds both queues for one face space; runs after the review assessment.
    static func refresh(_ db: GRDB.Database, model: String, processing: String, settings: Settings, now: Double) throws -> (merges: Int, splits: Int) {
        let namespace = FaceReview.namespace(model: model, processing: processing)
        var created: [Pair: (Int64, Int64, Double)] = [:]
        for row in try Row.fetchAll(db, sql: "SELECT person_id, other_person_id, created_at FROM face_merge_suggestions") {
            let keep: Int64 = row["person_id"], other: Int64 = row["other_person_id"]
            created[Pair(keep, other)] = (keep, other, row["created_at"])
        }
        var splitCreated: [Int64: Double] = [:]
        for row in try Row.fetchAll(db, sql: "SELECT person_id, created_at FROM face_split_suggestions") {
            splitCreated[row["person_id"]] = row["created_at"]
        }
        try db.execute(sql: "DELETE FROM face_merge_suggestions")
        try db.execute(sql: "DELETE FROM face_split_suggestions")
        let everyone = try people(db, model: model, processing: processing)
        let rows = merges(everyone, blocked: try blockedPairs(db), files: try filesByPerson(db), settings: settings)
        for row in rows {
            let previous = created[Pair(row.keep, row.other)].flatMap { $0.0 == row.keep && $0.1 == row.other ? $0.2 : nil }
            try db.execute(sql: "INSERT INTO face_merge_suggestions(person_id,other_person_id,score,namespace,created_at) VALUES(?,?,?,?,?)",
                           arguments: [row.keep, row.other, row.score, namespace, previous ?? now])
        }
        let (linked, dismissed) = try splitBlocks(db)
        var splits = everyone.filter { !dismissed.contains($0.id) }.compactMap { split(of: $0, settings: settings) }.filter { row in
            let keep = Set(row.keep), moved = Set(row.split)
            return !linked.contains { (keep.contains($0.0) && moved.contains($0.1)) || (keep.contains($0.1) && moved.contains($0.0)) }
        }
        splits.sort { $0.score != $1.score ? $0.score < $1.score : $0.person < $1.person }
        splits = Array(splits.prefix(maxSplits))
        for row in splits {
            try db.execute(sql: "INSERT INTO face_split_suggestions(person_id,score,keep_json,split_json,namespace,created_at) VALUES(?,?,?,?,?,?)",
                           arguments: [row.person, row.score, json(row.keep), json(row.split), namespace, splitCreated[row.person] ?? now])
        }
        return (rows.count, splits.count)
    }

    static func json(_ ids: [Int64]) -> String { "[" + ids.map(String.init).joined(separator: ",") + "]" }

    static func ids(_ text: String?) -> [Int64] {
        guard let text, let data = text.data(using: .utf8) else { return [] }
        return (try? JSONDecoder().decode([Int64].self, from: data)) ?? []
    }

    // MARK: - Lists

    static let mergeFrom = """
        FROM face_merge_suggestions s JOIN persons p ON p.id=s.person_id JOIN persons o ON o.id=s.other_person_id
        WHERE COALESCE(p.is_unknown,0)=0 AND COALESCE(o.is_unknown,0)=0 AND (?1 IS NULL OR s.person_id=?1 OR s.other_person_id=?1)
        """
    static let splitFrom = """
        FROM face_split_suggestions s JOIN persons p ON p.id=s.person_id WHERE COALESCE(p.is_unknown,0)=0 AND (?1 IS NULL OR s.person_id=?1)
        """

    static func mergeCount(_ db: GRDB.Database) throws -> Int {
        try Int.fetchOne(db, sql: "SELECT COUNT(*) " + mergeFrom, arguments: [nil as Int64?]) ?? 0
    }

    static func splitCount(_ db: GRDB.Database) throws -> Int {
        try Int.fetchOne(db, sql: "SELECT COUNT(*) " + splitFrom, arguments: [nil as Int64?]) ?? 0
    }

    static func page(_ requestID: String, _ action: String, _ status: String, queue: String, message: String? = nil,
                     operationID: String? = nil, personID: Int64? = nil, otherPersonID: Int64? = nil, total: Int = 0,
                     offset: Int = 0, merges: [FaceMergeSuggestion]? = nil, splits: [FaceSplitSuggestion]? = nil) -> FaceReviewPage {
        FaceReviewPage(requestID: requestID, action: action, status: status, message: message, operationID: operationID,
                       personID: personID, total: total, offset: offset, queue: queue, merges: merges, splits: splits,
                       otherPersonID: otherPersonID)
    }

    static func refused(_ db: GRDB.Database, _ requestID: String, _ action: String, _ queue: String, _ message: String) throws -> FaceReviewPage {
        page(requestID, action, "refused", queue: queue, message: message,
             total: queue == "merges" ? try mergeCount(db) : try splitCount(db))
    }

    static func listMerges(_ db: GRDB.Database, requestID: String, personID: Int64?, offset: Int?, limit: Int?) throws -> FaceReviewPage {
        let (offset, limit, valid) = FaceReview.pageBounds(offset: offset, limit: limit)
        guard valid else { return page(requestID, "list", "refused", queue: "merges", message: FaceReview.pageRefusal, offset: offset) }
        let total = try Int.fetchOne(db, sql: "SELECT COUNT(*) " + mergeFrom, arguments: [personID]) ?? 0
        let rows = try Row.fetchAll(db, sql: """
            SELECT s.person_id, \(FaceReview.display("p")) AS person_name, p.representative_face_id AS rep,
                   (SELECT COUNT(*) FROM face_prints a WHERE a.person_id=s.person_id) AS faces,
                   s.other_person_id, \(FaceReview.display("o")) AS other_name, o.representative_face_id AS other_rep,
                   (SELECT COUNT(*) FROM face_prints a WHERE a.person_id=s.other_person_id) AS other_faces, s.score, s.created_at
            """ + " " + mergeFrom + " ORDER BY s.score DESC, s.person_id, s.other_person_id LIMIT ?2 OFFSET ?3",
            arguments: [personID, limit, offset])
        let merges = rows.map { row in
            FaceMergeSuggestion(personID: row["person_id"], personName: row["person_name"], representativeFaceID: row["rep"],
                                faceCount: row["faces"], otherPersonID: row["other_person_id"], otherName: row["other_name"],
                                otherRepresentativeFaceID: row["other_rep"], otherFaceCount: row["other_faces"],
                                score: row["score"], createdAt: row["created_at"])
        }
        return page(requestID, "list", "ok", queue: "merges", total: total, offset: offset, merges: merges)
    }

    static func listSplits(_ db: GRDB.Database, requestID: String, personID: Int64?, offset: Int?, limit: Int?) throws -> FaceReviewPage {
        let (offset, limit, valid) = FaceReview.pageBounds(offset: offset, limit: limit)
        guard valid else { return page(requestID, "list", "refused", queue: "splits", message: FaceReview.pageRefusal, offset: offset) }
        let total = try Int.fetchOne(db, sql: "SELECT COUNT(*) " + splitFrom, arguments: [personID]) ?? 0
        let rows = try Row.fetchAll(db, sql: """
            SELECT s.person_id, \(FaceReview.display("p")) AS person_name, p.representative_face_id AS rep, s.score,
                   s.keep_json, s.split_json, s.created_at
            """ + " " + splitFrom + " ORDER BY s.score ASC, s.person_id LIMIT ?2 OFFSET ?3",
            arguments: [personID, limit, offset])
        let splits = rows.map { row in
            let keep = ids(row["keep_json"]), moved = ids(row["split_json"])
            return FaceSplitSuggestion(personID: row["person_id"], personName: row["person_name"], representativeFaceID: row["rep"],
                                       score: row["score"], keepCount: keep.count, splitCount: moved.count,
                                       keepFaceIDs: Array(keep.prefix(preview)), splitFaceIDs: Array(moved.prefix(preview)),
                                       createdAt: row["created_at"])
        }
        return page(requestID, "list", "ok", queue: "splits", total: total, offset: offset, splits: splits)
    }

    // MARK: - Resolutions

    struct Anchor {
        let face: Int64
        let fileID: Int64
        let bbox: String
    }

    static func anchor(_ db: GRDB.Database, personID: Int64) throws -> Anchor? {
        try Row.fetchOne(db, sql: """
            SELECT fp.id, fp.file_id, fp.bbox FROM face_prints fp WHERE fp.person_id=?1
            ORDER BY (fp.id=(SELECT representative_face_id FROM persons WHERE id=?1)) DESC, (fp.arcface_embedding IS NOT NULL) DESC,
                     COALESCE(fp.face_quality,0) DESC, fp.id ASC LIMIT 1
            """, arguments: [personID]).map { Anchor(face: $0["id"], fileID: $0["file_id"], bbox: $0["bbox"]) }
    }

    static func faceAnchor(_ db: GRDB.Database, faceID: Int64) throws -> Anchor? {
        try Row.fetchOne(db, sql: "SELECT id, file_id, bbox FROM face_prints WHERE id=?", arguments: [faceID])
            .map { Anchor(face: $0["id"], fileID: $0["file_id"], bbox: $0["bbox"]) }
    }

    static func verification(_ db: GRDB.Database, _ pair: Pair) throws -> Any {
        guard let row = try Row.fetchOne(db, sql: """
            SELECT same_person, confidence, vlm_model, verified_at, face_a, face_b, file_a, bbox_a, file_b, bbox_b
            FROM face_verifications WHERE person_a=? AND person_b=?
            """, arguments: [pair.a, pair.b]) else { return NSNull() }
        func value<T: DatabaseValueConvertible>(_ column: String, _ type: T.Type) -> Any { (row[column] as T?) as Any? ?? NSNull() }
        return [
            "samePerson": row["same_person"] as Int64, "confidence": row["confidence"] as Double,
            "model": row["vlm_model"] as String, "verifiedAt": row["verified_at"] as Double,
            "faceA": value("face_a", Int64.self), "faceB": value("face_b", Int64.self),
            "fileA": value("file_a", Int64.self), "bboxA": value("bbox_a", String.self),
            "fileB": value("file_b", Int64.self), "bboxB": value("bbox_b", String.self),
        ] as [String: Any]
    }

    static func restoreVerification(_ db: GRDB.Database, _ pair: Pair, same: Int64, model: String, previous: Any?) throws {
        try db.execute(sql: "DELETE FROM face_verifications WHERE person_a=? AND person_b=? AND same_person=? AND vlm_model=?",
                       arguments: [pair.a, pair.b, same, model])
        guard let previous = previous as? [String: Any] else { return }
        try db.execute(sql: """
            INSERT OR REPLACE INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,face_a,face_b,file_a,bbox_a,file_b,bbox_b)
            VALUES(?,?,?,?,?,?,?,?,?,?,?,?)
            """, arguments: [pair.a, pair.b, FaceReview.int(previous["samePerson"]) ?? 0, FaceReview.double(previous["confidence"]) ?? 1,
                             previous["model"] as? String ?? "", FaceReview.double(previous["verifiedAt"]) ?? 0,
                             FaceReview.int(previous["faceA"]), FaceReview.int(previous["faceB"]), FaceReview.int(previous["fileA"]),
                             previous["bboxA"] as? String, FaceReview.int(previous["fileB"]), previous["bboxB"] as? String])
    }

    /// Writes a verdict the way markPersonsDifferent and mergeClusters store theirs.
    static func writeVerification(_ db: GRDB.Database, people: (Int64, Int64), anchors: (Anchor, Anchor), same: Int64,
                                  model: String, now: Double) throws {
        let pair = Pair(people.0, people.1)
        let ordered: (Anchor, Anchor)
        if same == 1 { ordered = people.0 < people.1 ? anchors : (anchors.1, anchors.0) }
        else { ordered = anchors.0.face <= anchors.1.face ? anchors : (anchors.1, anchors.0) }
        try db.execute(sql: """
            INSERT OR REPLACE INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,face_a,face_b,file_a,bbox_a,file_b,bbox_b)
            VALUES(?,?,?,1.0,?,?,?,?,?,?,?,?)
            """, arguments: [pair.a, pair.b, same, model, now, ordered.0.face, ordered.1.face, ordered.0.fileID, ordered.0.bbox,
                             ordered.1.fileID, ordered.1.bbox])
    }

    static func journal(_ db: GRDB.Database, kind: String, fileID: Int64?, before: [String: Any], after: [String: Any],
                        now: Double) throws -> String {
        let id = UUID().uuidString.lowercased()
        let beforeJSON = try FaceReview.jsonText(before)
        let plan = try FaceReview.jsonText(["type": kind, "resolution": after["resolution"] ?? NSNull(), "personID": after["personID"] ?? NSNull()])
        try db.execute(sql: "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?,?,?,?,?,?)",
                       arguments: [id, fileID, kind, beforeJSON, try FaceReview.jsonText(after), now])
        try db.execute(sql: "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?,?,?,'completed',?)",
                       arguments: [id, plan, beforeJSON, now])
        return id
    }

    static let nameFields = ["name", "title", "first_name", "middle_name", "last_name", "suffix"]

    static func personJSON(_ db: GRDB.Database, _ personID: Int64) throws -> [String: Any]? {
        guard let row = try Row.fetchOne(db, sql: """
            SELECT name,title,first_name,middle_name,last_name,suffix,COALESCE(is_unknown,0) AS unknown,created_at,
                   representative_face_id,anchor_radius,last_clustered_at FROM persons WHERE id=?
            """, arguments: [personID]) else { return nil }
        var value: [String: Any] = [:]
        for field in nameFields { value[field] = (row[field] as String?) as Any? ?? NSNull() }
        value["is_unknown"] = row["unknown"] as Int64
        value["created_at"] = row["created_at"] as Double
        value["representative_face_id"] = (row["representative_face_id"] as Int64?) as Any? ?? NSNull()
        value["anchor_radius"] = (row["anchor_radius"] as Double?) as Any? ?? NSNull()
        value["last_clustered_at"] = (row["last_clustered_at"] as Double?) as Any? ?? NSNull()
        return value
    }

    static func names(_ value: [String: Any]) -> [String?] { nameFields.map { value[$0] as? String } }

    static func blank(_ fields: [String?]) -> Bool {
        fields.allSatisfy { ($0 ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    }

    static func sharesPhoto(_ db: GRDB.Database, _ a: Int64, _ b: Int64) throws -> Bool {
        try Bool.fetchOne(db, sql: """
            SELECT EXISTS(SELECT 1 FROM face_prints x JOIN face_prints y ON y.file_id=x.file_id WHERE x.person_id=? AND y.person_id=?)
            """, arguments: [a, b]) ?? false
    }

    static func exists(_ db: GRDB.Database, _ personID: Int64) throws -> Bool {
        try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM persons WHERE id=?)", arguments: [personID]) ?? false
    }

    static func resolve(_ db: GRDB.Database, requestID: String, personID: Int64, otherPersonID: Int64?, resolution: String,
                        now: Double) throws -> FaceReviewPage {
        switch (resolution, otherPersonID) {
        case ("split", _): return try split(db, requestID: requestID, personID: personID, now: now)
        case ("dismiss", nil): return try dismissSplit(db, requestID: requestID, personID: personID, now: now)
        case (_, let other?): return try merge(db, requestID: requestID, resolution: resolution, keep: personID, other: other, now: now)
        default: return try refused(db, requestID, "resolve", "merges", otherRequiredMessage)
        }
    }

    static func merge(_ db: GRDB.Database, requestID: String, resolution: String, keep: Int64, other: Int64,
                      now: Double) throws -> FaceReviewPage {
        func refuse(_ message: String) throws -> FaceReviewPage { try refused(db, requestID, "resolve", "merges", message) }
        guard let suggestion = try Row.fetchOne(db, sql: "SELECT s.score, s.namespace, s.created_at " + mergeFrom
                + " AND s.person_id=?2 AND s.other_person_id=?3", arguments: [nil as Int64?, keep, other]) else {
            return try refuse(FaceReview.staleMessage)
        }
        guard let keepAnchor = try anchor(db, personID: keep), let otherAnchor = try anchor(db, personID: other) else {
            return try refuse(FaceReview.noAnchorMessage)
        }
        let pair = Pair(keep, other)
        let stored: [String: Any] = ["score": suggestion["score"] as Double, "namespace": suggestion["namespace"] as String,
                                     "createdAt": suggestion["created_at"] as Double]
        var before: [String: Any] = ["resolution": resolution, "personID": keep, "otherPersonID": other, "suggestion": stored]
        var after: [String: Any] = ["resolution": resolution, "personID": keep, "otherPersonID": other]
        switch resolution {
        case "merge":
            if try sharesPhoto(db, keep, other) { return try refuse(sharedPhotoMessage) }
            if try blockedPairs(db).contains(pair) { return try refuse(markedDifferentMessage) }
            guard let otherRow = try personJSON(db, other), let keepRow = try personJSON(db, keep) else {
                return try refuse(FaceReview.staleMessage)
            }
            let faces = try Int64.fetchAll(db, sql: "SELECT id FROM face_prints WHERE person_id=? ORDER BY id", arguments: [other])
            before["verification"] = try verification(db, pair)
            before["other"] = otherRow
            before["keepNames"] = names(keepRow).map { $0 as Any? ?? NSNull() }
            try writeVerification(db, people: (keep, other), anchors: (keepAnchor, otherAnchor), same: 1, model: "user-merged", now: now)
            try db.execute(sql: "UPDATE face_prints SET person_id=? WHERE person_id=?", arguments: [keep, other])
            let blankName = "length(trim(COALESCE(name,'') || COALESCE(title,'') || COALESCE(first_name,'') || COALESCE(middle_name,'') || COALESCE(last_name,'') || COALESCE(suffix,'')))"
            try db.execute(sql: """
                UPDATE persons SET name=(SELECT name FROM persons WHERE id=?2), title=(SELECT title FROM persons WHERE id=?2),
                    first_name=(SELECT first_name FROM persons WHERE id=?2), middle_name=(SELECT middle_name FROM persons WHERE id=?2),
                    last_name=(SELECT last_name FROM persons WHERE id=?2), suffix=(SELECT suffix FROM persons WHERE id=?2), is_unknown=0
                WHERE id=?1 AND \(blankName)=0 AND EXISTS(SELECT 1 FROM persons WHERE id=?2 AND \(blankName)>0)
                """, arguments: [keep, other])
            try db.execute(sql: "DELETE FROM persons WHERE id=?", arguments: [other])
            try db.execute(sql: """
                UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?1), representative_face_id=COALESCE(
                    (SELECT fp.id FROM face_prints fp WHERE fp.person_id=?1 AND fp.arcface_embedding IS NOT NULL ORDER BY COALESCE(fp.face_quality,0) DESC, fp.id LIMIT 1),
                    (SELECT fp.id FROM face_prints fp WHERE fp.person_id=?1 ORDER BY COALESCE(fp.face_quality,0) DESC, fp.id LIMIT 1), representative_face_id)
                WHERE id=?1
                """, arguments: [keep])
            after["faceIDs"] = faces
        case "notSamePerson":
            before["verification"] = try verification(db, pair)
            try writeVerification(db, people: (keep, other), anchors: (keepAnchor, otherAnchor), same: 0, model: "user-verified", now: now)
        default:
            try db.execute(sql: """
                INSERT OR IGNORE INTO face_queue_dismissals(kind,file_id,bbox,other_file_id,other_bbox,created_at) VALUES('merge',?,?,?,?,?)
                """, arguments: [keepAnchor.fileID, keepAnchor.bbox, otherAnchor.fileID, otherAnchor.bbox, now])
            let created = db.changesCount > 0
            after["dismissalID"] = try Int64.fetchOne(db, sql: """
                SELECT id FROM face_queue_dismissals WHERE kind='merge' AND file_id=? AND bbox=? AND other_file_id=? AND other_bbox=?
                """, arguments: [keepAnchor.fileID, keepAnchor.bbox, otherAnchor.fileID, otherAnchor.bbox]) ?? NSNull()
            after["dismissalCreated"] = created
        }
        try db.execute(sql: "DELETE FROM face_merge_suggestions WHERE person_id=? AND other_person_id=?", arguments: [keep, other])
        let operationID = try journal(db, kind: "face_merge_review", fileID: keepAnchor.fileID, before: before, after: after, now: now)
        return page(requestID, "resolve", "ok", queue: "merges", operationID: operationID, personID: keep, otherPersonID: other,
                    total: try mergeCount(db))
    }

    static func splitRow(_ db: GRDB.Database, _ personID: Int64) throws -> (score: Double, keep: [Int64], split: [Int64], namespace: String, createdAt: Double)? {
        try Row.fetchOne(db, sql: "SELECT s.score, s.keep_json, s.split_json, s.namespace, s.created_at " + splitFrom,
                         arguments: [personID]).map { ($0["score"], ids($0["keep_json"]), ids($0["split_json"]), $0["namespace"], $0["created_at"]) }
    }

    static func owned(_ db: GRDB.Database, _ faces: [Int64], by personID: Int64) throws -> Bool {
        let count = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM face_prints WHERE person_id=? AND id IN (SELECT value FROM json_each(?))",
                                     arguments: [personID, json(faces)]) ?? 0
        return count == faces.count
    }

    static func split(_ db: GRDB.Database, requestID: String, personID: Int64, now: Double) throws -> FaceReviewPage {
        func refuse(_ message: String) throws -> FaceReviewPage { try refused(db, requestID, "resolve", "splits", message) }
        guard let row = try splitRow(db, personID), let firstMoved = row.split.first, let firstKept = row.keep.first,
              try owned(db, row.keep, by: personID), try owned(db, row.split, by: personID) else {
            return try refuse(FaceReview.staleMessage)
        }
        let representative = try Int64.fetchOne(db, sql: "SELECT representative_face_id FROM persons WHERE id=?", arguments: [personID])
        let keepFace = representative.flatMap { row.keep.contains($0) ? $0 : nil } ?? firstKept
        guard let keepAnchor = try faceAnchor(db, faceID: keepFace), let splitAnchor = try faceAnchor(db, faceID: firstMoved) else {
            return try refuse(FaceReview.staleMessage)
        }
        try db.execute(sql: "INSERT INTO persons(name,is_unknown,created_at,representative_face_id,file_count) VALUES(NULL,0,?,?,0)",
                       arguments: [now, firstMoved])
        let created = db.lastInsertedRowID
        let pair = Pair(personID, created)
        let previous = try verification(db, pair)
        try db.execute(sql: "UPDATE face_prints SET person_id=? WHERE person_id=? AND id IN (SELECT value FROM json_each(?))",
                       arguments: [created, personID, json(row.split)])
        try FaceReview.recount(db, personID: personID)
        try FaceReview.recount(db, personID: created)
        try writeVerification(db, people: (personID, created), anchors: (keepAnchor, splitAnchor), same: 0, model: "user-verified", now: now)
        try db.execute(sql: "DELETE FROM face_split_suggestions WHERE person_id=?", arguments: [personID])
        let before: [String: Any] = [
            "resolution": "split", "personID": personID, "verification": previous,
            "suggestion": ["score": row.score, "keep": row.keep, "split": row.split, "namespace": row.namespace, "createdAt": row.createdAt] as [String: Any],
        ]
        let after: [String: Any] = ["resolution": "split", "personID": personID, "newPersonID": created, "faceIDs": row.split]
        let operationID = try journal(db, kind: "face_split_review", fileID: keepAnchor.fileID, before: before, after: after, now: now)
        return page(requestID, "resolve", "ok", queue: "splits", operationID: operationID, personID: personID, otherPersonID: created,
                    total: try splitCount(db))
    }

    static func dismissSplit(_ db: GRDB.Database, requestID: String, personID: Int64, now: Double) throws -> FaceReviewPage {
        func refuse(_ message: String) throws -> FaceReviewPage { try refused(db, requestID, "resolve", "splits", message) }
        guard let row = try splitRow(db, personID) else { return try refuse(FaceReview.staleMessage) }
        guard let anchor = try anchor(db, personID: personID) else { return try refuse(FaceReview.noAnchorMessage) }
        try db.execute(sql: """
            INSERT OR IGNORE INTO face_queue_dismissals(kind,file_id,bbox,other_file_id,other_bbox,created_at) VALUES('split',?,?,NULL,NULL,?)
            """, arguments: [anchor.fileID, anchor.bbox, now])
        let inserted = db.changesCount > 0
        let dismissal = try Int64.fetchOne(db, sql: """
            SELECT id FROM face_queue_dismissals WHERE kind='split' AND file_id=? AND bbox=? AND other_file_id IS NULL
            """, arguments: [anchor.fileID, anchor.bbox])
        try db.execute(sql: "DELETE FROM face_split_suggestions WHERE person_id=?", arguments: [personID])
        let before: [String: Any] = [
            "resolution": "dismiss", "personID": personID,
            "suggestion": ["score": row.score, "keep": row.keep, "split": row.split, "namespace": row.namespace, "createdAt": row.createdAt] as [String: Any],
        ]
        let after: [String: Any] = ["resolution": "dismiss", "personID": personID, "dismissalID": dismissal as Any? ?? NSNull(),
                                    "dismissalCreated": inserted]
        let operationID = try journal(db, kind: "face_split_review", fileID: anchor.fileID, before: before, after: after, now: now)
        return page(requestID, "resolve", "ok", queue: "splits", operationID: operationID, personID: personID, total: try splitCount(db))
    }

    // MARK: - Undo

    static func restoreMergeRow(_ db: GRDB.Database, keep: Int64, other: Int64, suggestion: [String: Any]?, now: Double) throws {
        guard try exists(db, keep), try exists(db, other) else { return }
        try db.execute(sql: "INSERT OR REPLACE INTO face_merge_suggestions(person_id,other_person_id,score,namespace,created_at) VALUES(?,?,?,?,?)",
                       arguments: [keep, other, FaceReview.double(suggestion?["score"]) ?? 0, suggestion?["namespace"] as? String ?? "",
                                   FaceReview.double(suggestion?["createdAt"]) ?? now])
    }

    static func restoreSplitRow(_ db: GRDB.Database, personID: Int64, suggestion: [String: Any]?, now: Double) throws {
        guard try exists(db, personID) else { return }
        let keep = (suggestion?["keep"] as? [Any] ?? []).compactMap { FaceReview.int($0) }
        let moved = (suggestion?["split"] as? [Any] ?? []).compactMap { FaceReview.int($0) }
        try db.execute(sql: "INSERT OR REPLACE INTO face_split_suggestions(person_id,score,keep_json,split_json,namespace,created_at) VALUES(?,?,?,?,?,?)",
                       arguments: [personID, FaceReview.double(suggestion?["score"]) ?? 0, json(keep), json(moved),
                                   suggestion?["namespace"] as? String ?? "", FaceReview.double(suggestion?["createdAt"]) ?? now])
    }

    static func idList(_ value: Any?) -> [Int64] { (value as? [Any] ?? []).compactMap { FaceReview.int($0) } }

    static func undo(_ db: GRDB.Database, requestID: String, before: [String: Any], after: [String: Any], now: Double) throws -> FaceReviewPage {
        guard let personID = FaceReview.int(after["personID"]) else {
            return try refused(db, requestID, "undo", "merges", FaceReview.nothingToUndoMessage)
        }
        let resolution = after["resolution"] as? String ?? ""
        if let other = FaceReview.int(after["otherPersonID"]) {
            let pair = Pair(personID, other)
            switch resolution {
            case "merge":
                let faces = idList(after["faceIDs"])
                guard try !exists(db, other), try owned(db, faces, by: personID) else {
                    return try refused(db, requestID, "undo", "merges", FaceReview.movedManyMessage)
                }
                let row = before["other"] as? [String: Any] ?? [:]
                try db.execute(sql: """
                    INSERT INTO persons(id,name,title,first_name,middle_name,last_name,suffix,is_unknown,created_at,representative_face_id,file_count,anchor_radius,last_clustered_at)
                    VALUES(?,?,?,?,?,?,?,?,?,?,0,?,?)
                    """, arguments: [other, row["name"] as? String, row["title"] as? String, row["first_name"] as? String,
                                     row["middle_name"] as? String, row["last_name"] as? String, row["suffix"] as? String,
                                     FaceReview.int(row["is_unknown"]) ?? 0, FaceReview.double(row["created_at"]) ?? now,
                                     FaceReview.int(row["representative_face_id"]), FaceReview.double(row["anchor_radius"]),
                                     FaceReview.double(row["last_clustered_at"])])
                try db.execute(sql: "UPDATE face_prints SET person_id=? WHERE person_id=? AND id IN (SELECT value FROM json_each(?))",
                               arguments: [other, personID, json(faces)])
                let kept = (before["keepNames"] as? [Any] ?? []).map { $0 as? String }
                if blank(kept), let current = try personJSON(db, personID), names(current) == names(row) {
                    try db.execute(sql: "UPDATE persons SET name=NULL,title=NULL,first_name=NULL,middle_name=NULL,last_name=NULL,suffix=NULL WHERE id=?",
                                   arguments: [personID])
                }
                try restoreVerification(db, pair, same: 1, model: "user-merged", previous: before["verification"])
                try FaceReview.recount(db, personID: personID)
                try FaceReview.recount(db, personID: other)
            case "notSamePerson":
                try restoreVerification(db, pair, same: 0, model: "user-verified", previous: before["verification"])
            case "dismiss":
                if FaceReview.bool(after["dismissalCreated"]), let id = FaceReview.int(after["dismissalID"]) {
                    try db.execute(sql: "DELETE FROM face_queue_dismissals WHERE id=?", arguments: [id])
                }
            default:
                return try refused(db, requestID, "undo", "merges", FaceReview.nothingToUndoMessage)
            }
            try restoreMergeRow(db, keep: personID, other: other, suggestion: before["suggestion"] as? [String: Any], now: now)
            return page(requestID, "undo", "ok", queue: "merges", personID: personID, otherPersonID: other, total: try mergeCount(db))
        }
        switch resolution {
        case "split":
            guard let created = FaceReview.int(after["newPersonID"]) else {
                return try refused(db, requestID, "undo", "splits", FaceReview.nothingToUndoMessage)
            }
            let faces = idList(after["faceIDs"])
            guard try exists(db, personID), try exists(db, created), try owned(db, faces, by: created) else {
                return try refused(db, requestID, "undo", "splits", FaceReview.movedManyMessage)
            }
            if let row = try personJSON(db, created), !blank(names(row)) {
                return try refused(db, requestID, "undo", "splits", renamedMessage)
            }
            try db.execute(sql: "UPDATE face_prints SET person_id=? WHERE person_id=? AND id IN (SELECT value FROM json_each(?))",
                           arguments: [personID, created, json(faces)])
            try restoreVerification(db, Pair(personID, created), same: 0, model: "user-verified", previous: before["verification"])
            try db.execute(sql: "DELETE FROM persons WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM face_prints WHERE person_id=?1)", arguments: [created])
            try FaceReview.recount(db, personID: personID)
        case "dismiss":
            if FaceReview.bool(after["dismissalCreated"]), let id = FaceReview.int(after["dismissalID"]) {
                try db.execute(sql: "DELETE FROM face_queue_dismissals WHERE id=?", arguments: [id])
            }
        default:
            return try refused(db, requestID, "undo", "splits", FaceReview.nothingToUndoMessage)
        }
        try restoreSplitRow(db, personID: personID, suggestion: before["suggestion"] as? [String: Any], now: now)
        return page(requestID, "undo", "ok", queue: "splits", personID: personID, total: try splitCount(db))
    }
}
