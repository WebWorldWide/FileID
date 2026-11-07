import CryptoKit
import Foundation
import GRDB
import FileIDShared

/// Library-wide related-take proposals: a durable `takeGrouping` catalog job
/// that groups tries by capture session, CLIP similarity and user-named
/// people, and stores them as suggestions. Mirrored by the Rust engine's
/// commands/take_grouping.rs; keep thresholds, keys and evidence in step.
actor CatalogTakeGrouping {
    static let shared = CatalogTakeGrouping()
    static let jobID = "take-grouping"
    static let kind = "takeGrouping"
    static let version = "related-takes-v1"
    static var model: String { CLIPEmbeddingSpace.modelID }
    static let dimension = 512
    static let sessionGap = 1800.0
    static let maxSessionFiles = 2000
    static let maxActive = 32
    static let maxMembers = 100

    struct Limits: Sendable {
        let maxGap: Double
        let maxSpan: Double
        let similarity: Float
        let subjectSimilarity: Float
        let simultaneous: Double
    }

    static let photos = Limits(maxGap: 120, maxSpan: 1200, similarity: 0.90, subjectSimilarity: 0.86, simultaneous: 2)
    static let videos = Limits(maxGap: 1200, maxSpan: 7200, similarity: 0.88, subjectSimilarity: 0.84, simultaneous: 30)

    struct File: Sendable {
        let id: Int64
        let path: String
        let time: Double
        let camera: String?
        let fingerprint: String
        let subjects: Set<Int64>
    }

    struct Session: Sendable {
        let key: String
        let media: String
        let files: [File]
    }

    struct Plan: Sendable {
        let dirty: [Session]
        let keys: [String]
    }

    struct Proposal {
        let key: String
        let relation: String
        let fileIDs: [Int64]
        let evidence: [String]
        let similarity: Double
        let capturedAt: Double
    }

    struct Interrupted: LocalizedError {
        var errorDescription: String? { "Related-take grouping stopped" }
    }

    private var active = false

    func enqueue(_ request: CatalogRequest, database: Database, sink: IPCSink) async -> CatalogResponse {
        do {
            let response = try await database.pool.write { db in try Self.enqueue(db, requestID: request.requestID) }
            schedule(database: database, sink: sink)
            return response
        } catch {
            return CatalogResponse(requestID: request.requestID, status: "error", message: error.localizedDescription)
        }
    }

    func control(_ request: CatalogRequest, database: Database, sink: IPCSink) async -> CatalogResponse {
        let result = await CatalogStore.handle(request, database: database)
        if result.status == "ok", request.action == "resumeJob" { schedule(database: database, sink: sink) }
        return result
    }

    private func schedule(database: Database, sink: IPCSink) {
        guard !active else { return }
        active = true
        Task { await self.drive(database: database, sink: sink) }
    }

    private func drive(database: Database, sink: IPCSink) async {
        await Self.run(database: database, sink: sink)
        active = false
        let state = try? await database.pool.read { db in
            try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [Self.jobID])
        }
        if state == "queued" { schedule(database: database, sink: sink) }
    }

    static func recover(database: Database) async {
        try? await database.pool.write { db in try recover(db) }
    }

    static func recover(_ db: GRDB.Database) throws {
        try db.execute(sql: "UPDATE catalog_jobs SET state='paused',error='Interrupted. Resume to continue; finished capture sessions are kept.',updated_at=? WHERE kind=? AND state IN ('running','queued')",
                       arguments: [Date().timeIntervalSince1970, kind])
    }

    static func jobs(_ db: GRDB.Database) throws -> [CatalogJob] {
        try CatalogStore.jobs(db).filter { $0.kind == kind }
    }

    static func enqueue(_ db: GRDB.Database, requestID: String) throws -> CatalogResponse {
        let now = Date().timeIntervalSince1970
        let recipe = "{\"version\":\"\(version)\",\"model\":\"\(model)\"}"
        try db.execute(sql: "INSERT OR IGNORE INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,priority,created_at,updated_at) VALUES(?,?,'[]',?,'queued',0,?,?)",
                       arguments: [jobID, kind, recipe, now, now])
        try db.execute(sql: "UPDATE catalog_jobs SET state='queued',progress=0,error=NULL,checkpoint_json='{}',recipe_json=?,updated_at=? WHERE id=? AND kind=? AND state IN ('completed','cancelled','failed','paused')",
                       arguments: [recipe, now, jobID, kind])
        return CatalogResponse(requestID: requestID, status: "ok",
                               message: "Looking for related tries across the library. Suggestions never change, move or hide files.",
                               jobs: try jobs(db))
    }

    // MARK: - Reading proposals

    static func proposals(_ db: GRDB.Database, request: CatalogRequest) throws -> CatalogResponse {
        let limit = request.limit ?? 100
        guard (1...100).contains(limit) else { throw CatalogStore.InvalidRequest() }
        let grouped = Set(try Int64.fetchAll(db, sql: "SELECT file_id FROM catalog_event_files UNION SELECT derived_id FROM catalog_assets"))
        let rows = try Row.fetchAll(db, sql: "SELECT group_key,media,relation,file_ids_json,evidence_json,similarity,captured_at FROM take_proposals ORDER BY group_key")
        var groups: [CatalogTakeAutoGroup] = []
        for row in rows {
            if let group = try proposal(db, row: row, grouped: grouped) { groups.append(group) }
        }
        let visible = try CatalogTakeAuto.applyDecisions(db, groups)
        let newest = visible.map(\.capturedAt).max() ?? 0
        let ranked = Array(CatalogTakeAuto.rank(visible, newest: newest).prefix(limit))
        return CatalogResponse(requestID: request.requestID, status: "ok", jobs: try jobs(db), takeGroups: ranked)
    }

    private static func proposal(_ db: GRDB.Database, row: Row, grouped: Set<Int64>) throws -> CatalogTakeAutoGroup? {
        let membersJSON: String = row["file_ids_json"]
        let evidenceJSON: String = row["evidence_json"]
        guard let members = try? JSONDecoder().decode([Int64].self, from: Data(membersJSON.utf8)),
              let evidence = try? JSONDecoder().decode([String].self, from: Data(evidenceJSON.utf8)),
              !members.contains(where: grouped.contains) else { return nil }
        var items: [NearDuplicateGrouping.Item] = []
        for id in members {
            guard let file = try Row.fetchOne(db, sql: "SELECT path_text,created_at FROM files WHERE id=? AND failed=0", arguments: [id]) else { continue }
            items.append(NearDuplicateGrouping.Item(id: id, path: file["path_text"], time: file["created_at"], phash: nil, embedding: nil))
        }
        let shots = CatalogTakeAuto.shots(items)
        guard shots.count >= 2 else { return nil }
        let similarity: Double = row["similarity"]
        return CatalogTakeAutoGroup(groupKey: row["group_key"], kind: row["media"], status: "open", capturedAt: row["captured_at"],
                                    similarity: max(0, min(1, similarity)), shots: Array(shots.prefix(100)),
                                    relation: row["relation"], evidence: evidence)
    }

    // MARK: - The job

    static func run(database: Database, sink: IPCSink) async {
        let started = (try? await database.pool.write { db -> Bool in
            try db.execute(sql: "UPDATE catalog_jobs SET state='running',error=NULL,updated_at=? WHERE id=? AND kind=? AND state='queued'",
                           arguments: [Date().timeIntervalSince1970, jobID, kind])
            return db.changesCount == 1
        }) ?? false
        guard started else { return }
        await publish(database: database, sink: sink)
        var failure: String?
        do {
            let plan = try await database.pool.read { db -> Plan in
                try requireRunning(db)
                return try Self.plan(db)
            }
            var published = Date()
            for (index, session) in plan.dirty.enumerated() {
                try await database.pool.write { db in try process(db, session: session, processed: index + 1, total: plan.dirty.count) }
                if Date().timeIntervalSince(published) >= 0.5 {
                    await publish(database: database, sink: sink)
                    published = Date()
                }
            }
            try await database.pool.write { db in try finish(db, keys: plan.keys) }
        } catch is Interrupted {
            await publish(database: database, sink: sink)
            return
        } catch {
            failure = String(error.localizedDescription.prefix(2000))
        }
        let terminal = failure
        try? await database.pool.write { db in
            if let failure = terminal {
                try db.execute(sql: "UPDATE catalog_jobs SET state='failed',error=?,updated_at=? WHERE id=? AND state='running'",
                               arguments: [failure, Date().timeIntervalSince1970, jobID])
            } else {
                try db.execute(sql: "UPDATE catalog_jobs SET state='completed',progress=1,error=NULL,updated_at=? WHERE id=? AND state='running'",
                               arguments: [Date().timeIntervalSince1970, jobID])
            }
        }
        await publish(database: database, sink: sink)
    }

    private static func publish(database: Database, sink: IPCSink) async {
        if let jobs = try? await database.pool.read({ db in try jobs(db) }) {
            await sink.emit(.catalogResponse(CatalogResponse(requestID: jobID, status: "ok", jobs: jobs)))
        }
    }

    static func requireRunning(_ db: GRDB.Database) throws {
        let state = try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [jobID])
        guard state == "running" else { throw Interrupted() }
    }

    static func plan(_ db: GRDB.Database) throws -> Plan {
        let files = try eligible(db)
        var stored: [Int64: (String, String)] = [:]
        for row in try Row.fetchAll(db, sql: "SELECT file_id,session,fingerprint FROM take_proposal_files") {
            stored[row["file_id"]] = (row["session"], row["fingerprint"])
        }
        var dirty: [Session] = []
        var keys: [String] = []
        for session in sessions(files) {
            keys.append(session.key)
            let clean = session.files.allSatisfy { file in
                guard let entry = stored[file.id] else { return false }
                return entry.0 == session.key && entry.1 == file.fingerprint
            }
            if !clean { dirty.append(session) }
        }
        return Plan(dirty: dirty, keys: keys)
    }

    static func eligible(_ db: GRDB.Database) throws -> [(String, File)] {
        let subjects = try Self.subjects(db)
        let rows = try Row.fetchCursor(db, sql: """
            SELECT f.id,f.path_text,f.created_at,f.kind,f.size_bytes,f.modified_at,f.camera_model,f.content_hash
            FROM files f JOIN clip_embeddings e ON e.file_id=f.id
            WHERE f.kind IN ('image','video') AND f.failed=0 AND f.created_at IS NOT NULL
              AND e.model=? AND length(e.embedding)=?
              AND NOT EXISTS (SELECT 1 FROM catalog_assets a WHERE a.derived_id=f.id)
              AND NOT EXISTS (SELECT 1 FROM catalog_event_files ef WHERE ef.file_id=f.id)
            ORDER BY f.id
            """, arguments: [model, dimension * 4])
        var hashes = Set<Data>()
        var files: [(String, File)] = []
        while let row = try rows.next() {
            let time: Double = row["created_at"]
            guard time.isFinite else { continue }
            let hash: Data? = row["content_hash"]
            if let hash, !hashes.insert(hash).inserted { continue }
            files.append(file(row, time: time, subjects: subjects))
        }
        return files
    }

    private static func file(_ row: Row, time: Double, subjects: [Int64: [(Int64, String)]]) -> (String, File) {
        let id: Int64 = row["id"]
        let path: String = row["path_text"]
        let kind: String = row["kind"]
        let size: Int64 = row["size_bytes"]
        let modified: Double? = row["modified_at"]
        let rawCamera: String? = row["camera_model"]
        let camera = rawCamera.map { $0.trimmingCharacters(in: .whitespaces) }.flatMap { $0.isEmpty ? nil : $0 }
        let revision = Self.revision(size: size, modified: modified)
        let people = Set((subjects[id] ?? []).filter { $0.1 == revision }.map(\.0))
        let names = people.sorted().map(String.init).joined(separator: ",")
        let fingerprint = "\(revision)|\(time.bitPattern)|\(camera ?? "")|\(names)|\(path)"
        let media = kind == "image" ? "photos" : "videos"
        return (media, File(id: id, path: path, time: time, camera: camera, fingerprint: fingerprint, subjects: people))
    }

    static func revision(size: Int64, modified: Double?) -> String {
        "\(size):\(modified.map { String($0.bitPattern) } ?? "unknown")"
    }

    /// User-named people per file with the revision their evidence was taken
    /// from: compatible faces of a named person, or the person's user observations.
    static func subjects(_ db: GRDB.Database) throws -> [Int64: [(Int64, String)]] {
        let rows = try Row.fetchAll(db, sql: """
            SELECT fp.file_id,p.id AS person,p.title,p.first_name,p.name,fp.source_revision AS source
            FROM face_prints fp JOIN persons p ON p.id=fp.person_id JOIN face_prints rep ON rep.id=p.representative_face_id
            WHERE COALESCE(fp.excluded,0)=0 AND IFNULL(p.is_unknown,0)=0
              AND fp.embedding_model IS NOT NULL AND fp.embedding_model<>''
              AND rep.embedding_model=fp.embedding_model AND rep.processing_version IS fp.processing_version
            UNION ALL
            SELECT o.file_id,p.id,p.title,p.first_name,p.name,o.source_revision
            FROM catalog_observations o JOIN persons p ON p.id=o.person_id
            WHERE o.user_edited=1 AND o.stale=0 AND IFNULL(p.is_unknown,0)=0
            """)
        var found: [Int64: [(Int64, String)]] = [:]
        for row in rows {
            guard let source = row[5] as String? else { continue }
            let display = SmartNameStyle.personDisplay(title: row[2], first: row[3], legacy: row[4])
            guard SmartNameStyle.isUserNamed(display) else { continue }
            let fileID: Int64 = row[0]
            let person: Int64 = row[1]
            found[fileID, default: []].append((person, source))
        }
        return found
    }

    static func sessions(_ files: [(String, File)]) -> [Session] {
        var result: [Session] = []
        for media in ["photos", "videos"] {
            let ordered = files.filter { $0.0 == media }.map(\.1).sorted { ($0.time, $0.id) < ($1.time, $1.id) }
            var current: [File] = []
            for file in ordered {
                let gap = current.last.map { file.time - $0.time > sessionGap } ?? false
                if gap || current.count >= maxSessionFiles {
                    result.append(session(media, current))
                    current = []
                }
                current.append(file)
            }
            if !current.isEmpty { result.append(session(media, current)) }
        }
        return result
    }

    private static func session(_ media: String, _ files: [File]) -> Session {
        let ids = files.map(\.id).sorted().map(String.init).joined(separator: ",")
        let digest = SHA256.hash(data: Data("\(version)|\(model)|\(media)|\(ids)".utf8))
        let hex = digest.prefix(8).map { String(format: "%02x", $0) }.joined()
        return Session(key: "\(media)-\(hex)", media: media, files: files)
    }

    static func process(_ db: GRDB.Database, session: Session, processed: Int, total: Int) throws {
        try requireRunning(db)
        let proposals = try cluster(db, session: session)
        let members = String(decoding: try JSONEncoder().encode(session.files.map(\.id)), as: UTF8.self)
        try db.execute(sql: "DELETE FROM take_proposals WHERE session=? OR session IN (SELECT session FROM take_proposal_files WHERE file_id IN (SELECT value FROM json_each(?)))",
                       arguments: [session.key, members])
        let now = Date().timeIntervalSince1970
        for proposal in proposals {
            let ids = String(decoding: try JSONEncoder().encode(proposal.fileIDs), as: UTF8.self)
            let evidence = String(decoding: try JSONEncoder().encode(proposal.evidence), as: UTF8.self)
            try db.execute(sql: "INSERT OR REPLACE INTO take_proposals(group_key,session,media,relation,file_ids_json,evidence_json,similarity,captured_at,proposed_at) VALUES(?,?,?,?,?,?,?,?,?)",
                           arguments: [proposal.key, session.key, session.media, proposal.relation, ids, evidence,
                                       proposal.similarity, proposal.capturedAt, now])
        }
        for file in session.files {
            try db.execute(sql: "INSERT INTO take_proposal_files(file_id,session,fingerprint) VALUES(?,?,?) ON CONFLICT(file_id) DO UPDATE SET session=excluded.session,fingerprint=excluded.fingerprint",
                           arguments: [file.id, session.key, file.fingerprint])
        }
        let progress = min(0.99, Double(processed) / Double(max(1, total)))
        try db.execute(sql: "UPDATE catalog_jobs SET checkpoint_json=?,progress=?,updated_at=? WHERE id=? AND state='running'",
                       arguments: ["{\"processed\":\(processed),\"total\":\(total)}", progress, now, jobID])
    }

    static func finish(_ db: GRDB.Database, keys: [String]) throws {
        try requireRunning(db)
        let json = String(decoding: try JSONEncoder().encode(keys), as: UTF8.self)
        try db.execute(sql: "DELETE FROM take_proposals WHERE session NOT IN (SELECT value FROM json_each(?))", arguments: [json])
        try db.execute(sql: "DELETE FROM take_proposal_files WHERE session NOT IN (SELECT value FROM json_each(?))", arguments: [json])
    }

    // MARK: - Clustering

    private struct Group {
        var anchor: [Float]
        var anchorTime: Double
        var latestTime: Double
        var members: [Int]
        var common: Set<Int64>
        var known: Set<Int64>
        var lowest: Float
    }

    static func cluster(_ db: GRDB.Database, session: Session) throws -> [Proposal] {
        let limits = session.media == "photos" ? photos : videos
        var vectors: [[Float]?] = []
        for file in session.files {
            let blob = try Data.fetchOne(db, sql: "SELECT embedding FROM clip_embeddings WHERE file_id=? AND model=?", arguments: [file.id, model])
            vectors.append(blob.flatMap(unitVector))
        }
        var active: [Group] = []
        var finished: [Group] = []
        for (index, file) in session.files.enumerated() {
            guard let vector = vectors[index] else { continue }
            var still: [Group] = []
            for group in active {
                let expired = file.time - group.latestTime > limits.maxGap || file.time - group.anchorTime > limits.maxSpan
                if expired { finished.append(group) } else { still.append(group) }
            }
            active = still
            if let (position, similarity) = best(active, file: file, vector: vector, limits: limits) {
                active[position].members.append(index)
                active[position].latestTime = file.time
                active[position].lowest = min(active[position].lowest, similarity)
                active[position].common.formIntersection(file.subjects)
                active[position].known.formUnion(file.subjects)
            } else {
                if active.count >= maxActive { finished.append(active.removeFirst()) }
                active.append(Group(anchor: vector, anchorTime: file.time, latestTime: file.time, members: [index],
                                    common: file.subjects, known: file.subjects, lowest: 1))
            }
        }
        finished.append(contentsOf: active)
        return finished.filter { $0.members.count >= 2 }.compactMap { proposal($0, session: session, limits: limits) }
    }

    private static func best(_ active: [Group], file: File, vector: [Float], limits: Limits) -> (Int, Float)? {
        var best: (Int, Float)?
        for (position, group) in active.enumerated() where group.members.count < maxMembers {
            if !file.subjects.isEmpty, !group.known.isEmpty, file.subjects.isDisjoint(with: group.known) { continue }
            let similarity = dot(vector, group.anchor)
            let threshold = file.subjects.isDisjoint(with: group.common) ? limits.similarity : limits.subjectSimilarity
            if similarity >= threshold, best.map({ similarity > $0.1 }) ?? true { best = (position, similarity) }
        }
        return best
    }

    private static func proposal(_ group: Group, session: Session, limits: Limits) -> Proposal? {
        let files = group.members.map { session.files[$0] }
        let items = files.map { NearDuplicateGrouping.Item(id: $0.id, path: $0.path, time: $0.time, phash: nil, embedding: nil) }
        let shots = CatalogTakeAuto.shots(items)
        guard shots.count >= 2 else { return nil }
        let byID = Dictionary(uniqueKeysWithValues: files.map { ($0.id, $0) })
        let primaries = shots.compactMap { byID[$0.primaryFileID] }
        var evidence = ["captureSession", "visualSimilarity"]
        if !group.common.isEmpty { evidence.append("sharedSubject") }
        let (kind, camera) = relation(primaries, simultaneous: limits.simultaneous)
        evidence.append(camera)
        let ids = files.map(\.id).sorted()
        return Proposal(key: CatalogTakeAuto.key(ids), relation: kind, fileIDs: ids, evidence: evidence,
                        similarity: TakeScoring.round4(Double(max(0, min(1, group.lowest)))),
                        capturedAt: files.map(\.time).max() ?? 0)
    }

    static func relation(_ primaries: [File], simultaneous: Double) -> (String, String) {
        for (index, left) in primaries.enumerated() {
            for right in primaries[(index + 1)...] {
                if let a = left.camera, let b = right.camera, a != b, abs(left.time - right.time) <= simultaneous {
                    return ("alternateViews", "simultaneousCameras")
                }
            }
        }
        if primaries.contains(where: { $0.camera == nil }) { return ("uncertain", "cameraDetailsMissing") }
        if Set(primaries.compactMap(\.camera)).count == 1 { return ("repeatedAttempts", "sameCamera") }
        return ("uncertain", "mixedCameras")
    }

    static func unitVector(_ blob: Data) -> [Float]? {
        guard blob.count == dimension * 4 else { return nil }
        var vector = blob.withUnsafeBytes { raw in
            (0..<dimension).map { Float(bitPattern: UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: $0 * 4, as: UInt32.self))) }
        }
        guard vector.allSatisfy(\.isFinite) else { return nil }
        let norm = vector.reduce(0.0) { $0 + Double($1) * Double($1) }
        guard abs(norm - 1) <= 0.02 else { return nil }
        let scale = Float(norm.squareRoot())
        for index in vector.indices { vector[index] /= scale }
        return vector
    }

    static func dot(_ left: [Float], _ right: [Float]) -> Float {
        var sum: Float = 0
        for index in 0..<min(left.count, right.count) { sum += left[index] * right[index] }
        return sum
    }
}
