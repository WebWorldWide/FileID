// Borderline person pairs worth a human "same person?" review, ranked the way
// the shared Rust engine ranks them (commands/bulk.rs
// handle_find_merge_suggestions): corroborated small-into-large fragments
// first, then other fragment pairs, then by SFace cosine. Only faces whose
// embedding provenance is current are compared.
import Foundation
import GRDB

extension MergeSuggestionPair: Identifiable {
    public var id: String { "\(sourcePersonID):\(destinationPersonID)" }
}

public enum PersonMergeSuggestions {
    public static let similarityFloor: Float = 0.55
    public static let similarityCeiling: Float = 0.97
    public static let resultLimit = 50

    static let fragmentMaxFaces = 12
    static let fragmentMemberCosine: Float = 0.75
    static let fragmentMinSourceVoters = 2
    static let fragmentMinTargetVoters = 3
    static let fragmentMinCohesion: Float = 0.70
    static let evidenceSampleLimit = 64
    static let evidenceCandidateLimit = 1_000

    public static let manifestFaceModel = "sface128-sha256:"
        + (ModelManifest.artifacts.first { $0.id == "sface_embedder" }?.sha256 ?? "")

    /// The installed SFace weights' identity, or the manifest's when none are
    /// installed (installs are verified against the manifest digest).
    public static func installedFaceModel() -> String {
        let url = FaceEmbedderKind.modelsDirectory.appendingPathComponent(FaceEmbedderKind.sface.modelFileName)
        guard let size = (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize, size > 0,
              let digest = ExactFileDigest.computeWithIdentity(url: url, expectedSize: Int64(size))
        else { return manifestFaceModel }
        return "sface128-sha256:" + digest.digest.map { String(format: "%02x", $0) }.joined()
    }

    public static func find(
        dbPath: String, selectedModel: String? = nil,
        processingVersion: String = FaceCacheProvenance.processingVersion
    ) throws -> [MergeSuggestionPair] {
        var configuration = Configuration()
        configuration.readonly = true
        let queue = try DatabaseQueue(path: dbPath, configuration: configuration)
        let model = selectedModel ?? installedFaceModel()
        return try queue.read { try find(in: $0, selectedModel: model, processingVersion: processingVersion) }
    }

    public static func find(
        in db: GRDB.Database, selectedModel: String,
        processingVersion: String = FaceCacheProvenance.processingVersion
    ) throws -> [MergeSuggestionPair] {
        let people = try loadPeople(db, model: selectedModel, processing: processingVersion)
        guard people.count >= 2 else { return [] }
        let blocked = try blockedPairs(db)
        var filesByPerson: [Int64: Set<Int64>] = [:]
        let memberships = try Row.fetchCursor(db, sql: """
            SELECT person_id, file_id FROM face_prints
            WHERE person_id IS NOT NULL AND COALESCE(excluded, 0) = 0
            """)
        while let row = try memberships.next() {
            filesByPerson[row[0] as Int64, default: []].insert(row[1] as Int64)
        }

        var fragments: [Candidate] = []
        var others: [Candidate] = []
        for i in 0..<people.count {
            for j in (i + 1)..<people.count {
                let similarity = dot(people[i].vector, people[j].vector)
                guard similarity >= similarityFloor, similarity < similarityCeiling,
                      !blocked.contains(PersonPair(people[i].id, people[j].id)),
                      let left = filesByPerson[people[i].id], let right = filesByPerson[people[j].id],
                      left.isDisjoint(with: right) else { continue }
                let candidate = Candidate(similarity: similarity, a: i, b: j)
                if isFragmentPair(candidate, people) {
                    fragments.append(candidate)
                    if fragments.count >= evidenceCandidateLimit * 4 { keepTop(&fragments, evidenceCandidateLimit) }
                } else {
                    others.append(candidate)
                    if others.count >= resultLimit * 8 { keepTop(&others, resultLimit) }
                }
            }
        }
        keepTop(&fragments, evidenceCandidateLimit)
        keepTop(&others, resultLimit)

        if !fragments.isEmpty {
            let ids = Array(Set(fragments.flatMap { [people[$0.a].id, people[$0.b].id] })).sorted()
            let faces = try evidenceFaces(db, personIDs: ids, model: selectedModel, processing: processingVersion)
            for index in fragments.indices {
                let a = people[fragments[index].a], b = people[fragments[index].b]
                let (source, target) = a.memberCount <= b.memberCount ? (a, b) : (b, a)
                if let sourceFaces = faces[source.id], let targetFaces = faces[target.id] {
                    fragments[index].corroborated = corroboratedFragmentMatch(source: sourceFaces, target: targetFaces)
                }
            }
        }

        let ranked = (fragments + others).sorted { lhs, rhs in
            if lhs.corroborated != rhs.corroborated { return lhs.corroborated }
            let left = isFragmentPair(lhs, people), right = isFragmentPair(rhs, people)
            if left != right { return left }
            return precedes(lhs, rhs)
        }
        return ranked.prefix(resultLimit).map { candidate in
            let a = people[candidate.a], b = people[candidate.b]
            let aIsSource = a.memberCount < b.memberCount || (a.memberCount == b.memberCount && a.id >= b.id)
            let (source, destination) = aIsSource ? (a, b) : (b, a)
            return MergeSuggestionPair(
                sourcePersonID: source.id, destinationPersonID: destination.id,
                similarity: Double(candidate.similarity),
                sourceAnchorFaceID: source.anchorFaceID, destinationAnchorFaceID: destination.anchorFaceID,
                sourceMemberCount: source.memberCount, destinationMemberCount: destination.memberCount)
        }
    }

    struct Person {
        let id: Int64
        let anchorFaceID: Int64
        let memberCount: Int
        let vector: [Float]
    }

    struct Face {
        let id: Int64
        let fileID: Int64
        let contentHash: Data?
        let vector: [Float]
        let quality: Float
    }

    struct Candidate {
        let similarity: Float
        let a: Int
        let b: Int
        var corroborated = false
    }

    private struct PersonPair: Hashable {
        let low: Int64
        let high: Int64
        init(_ lhs: Int64, _ rhs: Int64) { low = min(lhs, rhs); high = max(lhs, rhs) }
    }

    /// One vector per known person: the representative face when its
    /// embedding is current, else the person's first current face.
    private static func loadPeople(_ db: GRDB.Database, model: String, processing: String) throws -> [Person] {
        var counts: [Int64: Int] = [:]
        for row in try Row.fetchAll(db, sql: """
            SELECT person_id, COUNT(*) FROM face_prints
            WHERE person_id IS NOT NULL AND COALESCE(excluded, 0) = 0 GROUP BY person_id
            """) {
            counts[row[0] as Int64] = row[1] as Int
        }
        let cursor = try Row.fetchCursor(db, sql: """
            SELECT p.id AS person_id, fp.id AS face_id,
                   CASE WHEN LENGTH(fp.arcface_embedding) = 512 THEN fp.arcface_embedding END AS embedding,
                   fp.embedding_model, fp.processing_version, fp.source_revision,
                   r.revision AS catalog_revision, f.size_bytes, f.modified_at
            FROM persons p
            JOIN face_prints fp ON fp.person_id = p.id
            JOIN files f ON f.id = fp.file_id
            LEFT JOIN catalog_revisions r ON r.file_id = f.id
            WHERE COALESCE(p.is_unknown, 0) = 0 AND COALESCE(fp.excluded, 0) = 0 AND f.failed = 0
            ORDER BY p.id, (fp.id = p.representative_face_id) DESC, fp.id
            """)
        var people: [Person] = []
        var lastPerson: Int64?
        while let row = try cursor.next() {
            let personID: Int64 = row["person_id"]
            guard personID != lastPerson, personID > 0,
                  isCurrent(row, model: model, processing: processing),
                  let vector = unitVector(row["embedding"]) else { continue }
            let faceID: Int64 = row["face_id"]
            guard faceID > 0 else { continue }
            lastPerson = personID
            people.append(Person(id: personID, anchorFaceID: faceID, memberCount: counts[personID] ?? 1, vector: vector))
        }
        return people
    }

    private static func isCurrent(_ row: Row, model: String, processing: String) -> Bool {
        FaceCacheProvenance.isCurrent(
            embedding: row["embedding"], model: row["embedding_model"], processing: row["processing_version"],
            sourceRevision: row["source_revision"], catalogRevision: row["catalog_revision"],
            size: row["size_bytes"], modified: row["modified_at"],
            selectedModel: model, selectedProcessing: processing)
    }

    /// "Different people" verdicts: the stored person pair, plus the pair the
    /// two verified faces belong to now, so a verdict survives re-clustering.
    private static func blockedPairs(_ db: GRDB.Database) throws -> Set<PersonPair> {
        func resolveFace(_ legacy: Int64?, fileID: Int64?, bbox: String?) throws -> Int64? {
            if let fileID, let bbox, let current = try Int64.fetchOne(
                db, sql: "SELECT id FROM face_prints WHERE file_id = ? AND bbox = ? ORDER BY id LIMIT 1",
                arguments: [fileID, bbox]) {
                return current
            }
            guard let legacy else { return nil }
            return try Int64.fetchOne(db, sql: "SELECT id FROM face_prints WHERE id = ?", arguments: [legacy])
        }
        var blocked = Set<PersonPair>()
        for row in try Row.fetchAll(db, sql: """
            SELECT person_a, person_b, face_a, face_b, file_a, bbox_a, file_b, bbox_b
            FROM face_verifications WHERE same_person = 0
            """) {
            let personA: Int64 = row["person_a"] ?? 0
            let personB: Int64 = row["person_b"] ?? 0
            blocked.insert(PersonPair(personA, personB))
            if let faceA = try resolveFace(row["face_a"], fileID: row["file_a"], bbox: row["bbox_a"]),
               let faceB = try resolveFace(row["face_b"], fileID: row["file_b"], bbox: row["bbox_b"]),
               let currentA = try Int64.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id = ?", arguments: [faceA]),
               let currentB = try Int64.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id = ?", arguments: [faceB]),
               currentA != currentB {
                blocked.insert(PersonPair(currentA, currentB))
            }
        }
        return blocked
    }

    private static func evidenceFaces(
        _ db: GRDB.Database, personIDs: [Int64], model: String, processing: String
    ) throws -> [Int64: [Face]] {
        var faces: [Int64: [Face]] = [:]
        for start in stride(from: 0, to: personIDs.count, by: 900) {
            let chunk = Array(personIDs[start..<min(start + 900, personIDs.count)])
            let placeholders = Array(repeating: "?", count: chunk.count).joined(separator: ",")
            let cursor = try Row.fetchCursor(db, sql: """
                SELECT fp.person_id, fp.id AS face_id, fp.file_id, f.content_hash,
                       CASE WHEN LENGTH(fp.arcface_embedding) = 512 THEN fp.arcface_embedding END AS embedding,
                       COALESCE(fp.face_quality, 0.0) AS quality,
                       fp.embedding_model, fp.processing_version, fp.source_revision,
                       r.revision AS catalog_revision, f.size_bytes, f.modified_at
                FROM face_prints fp
                JOIN files f ON f.id = fp.file_id
                LEFT JOIN catalog_revisions r ON r.file_id = f.id
                WHERE fp.person_id IN (\(placeholders)) AND fp.arcface_embedding IS NOT NULL
                ORDER BY fp.person_id, fp.id
                """, arguments: StatementArguments(chunk))
            while let row = try cursor.next() {
                guard isCurrent(row, model: model, processing: processing),
                      let vector = unitVector(row["embedding"]) else { continue }
                let quality: Double = row["quality"]
                faces[row["person_id"] as Int64, default: []].append(Face(
                    id: row["face_id"], fileID: row["file_id"], contentHash: row["content_hash"],
                    vector: vector, quality: Float(quality)))
            }
        }
        return faces
    }

    static func corroboratedFragmentMatch(source: [Face], target: [Face]) -> Bool {
        guard source.count >= fragmentMinSourceVoters, source.count <= fragmentMaxFaces,
              target.count > fragmentMaxFaces else { return false }
        let captures = Set(source.map { face -> String in
            if let hash = face.contentHash, !hash.isEmpty { return "c" + hash.base64EncodedString() }
            return "f\(face.fileID)"
        })
        guard captures.count == source.count,
              Set(source.map(\.fileID)).isDisjoint(with: target.map(\.fileID)),
              cohesion(source) >= fragmentMinCohesion, cohesion(target) >= fragmentMinCohesion else { return false }
        let sourceMembers = distinctEvidence(source)
        let targetMembers = distinctEvidence(target)
        guard sourceMembers.count >= fragmentMinSourceVoters,
              targetMembers.count >= fragmentMinTargetVoters else { return false }
        return sourceMembers.allSatisfy { sourceFace in
            targetMembers.lazy
                .filter { dot(sourceFace.vector, $0.vector) >= fragmentMemberCosine }
                .prefix(fragmentMinTargetVoters)
                .count == fragmentMinTargetVoters
        }
    }

    private static func cohesion(_ faces: [Face]) -> Float {
        guard let dimension = faces.first?.vector.count, dimension > 0,
              faces.allSatisfy({ $0.vector.count == dimension }) else { return 0 }
        var sum = [Float](repeating: 0, count: dimension)
        for face in faces {
            for index in 0..<dimension { sum[index] += face.vector[index] }
        }
        return sum.reduce(Float(0)) { $0 + $1 * $1 }.squareRoot() / Float(faces.count)
    }

    /// Best-quality faces first, one per photo content and per exact embedding.
    private static func distinctEvidence(_ faces: [Face]) -> [Face] {
        let ordered = faces.sorted { $0.quality != $1.quality ? $0.quality > $1.quality : $0.id < $1.id }
        var seenContent = Set<Data>()
        var seenEmbeddings = Set<[UInt32]>()
        var distinct: [Face] = []
        for face in ordered {
            guard let hash = face.contentHash,
                  seenContent.insert(hash).inserted,
                  seenEmbeddings.insert(face.vector.map(\.bitPattern)).inserted else { continue }
            distinct.append(face)
            if distinct.count == evidenceSampleLimit { break }
        }
        return distinct
    }

    private static func isFragmentPair(_ candidate: Candidate, _ people: [Person]) -> Bool {
        let a = people[candidate.a].memberCount, b = people[candidate.b].memberCount
        return min(a, b) <= fragmentMaxFaces && max(a, b) > fragmentMaxFaces
    }

    private static func precedes(_ lhs: Candidate, _ rhs: Candidate) -> Bool {
        if lhs.similarity != rhs.similarity { return lhs.similarity > rhs.similarity }
        if lhs.a != rhs.a { return lhs.a < rhs.a }
        return lhs.b < rhs.b
    }

    private static func keepTop(_ candidates: inout [Candidate], _ limit: Int) {
        guard candidates.count > limit else { return }
        candidates.sort(by: precedes)
        candidates.removeLast(candidates.count - limit)
    }

    private static func unitVector(_ data: Data?) -> [Float]? {
        guard let data else { return nil }
        var vector = FloatBlob.decode(data)
        guard !vector.isEmpty, vector.allSatisfy(\.isFinite) else { return nil }
        let norm = vector.reduce(Float(0)) { $0 + $1 * $1 }.squareRoot()
        guard norm.isFinite, norm > .leastNormalMagnitude else { return nil }
        for index in vector.indices { vector[index] /= norm }
        return vector
    }

    private static func dot(_ a: [Float], _ b: [Float]) -> Float {
        guard a.count == b.count else { return -1 }
        var sum: Float = 0
        for index in a.indices { sum += a[index] * b[index] }
        return sum
    }
}
