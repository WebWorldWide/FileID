import Foundation
import GRDB
import FileIDShared

/// Durable face refresh backlog: a `faceRefresh` catalog job that re-embeds
/// faces whose stored vector is missing or from another model, processing or
/// file version, one model/processing namespace at a time. Mirrored by the
/// Rust engine's commands/face_refresh.rs; keep states, SQL and messages in step.
actor FaceRefreshJob {
    static let shared = FaceRefreshJob()
    static let jobID = "face-refresh"
    static let kind = "faceRefresh"
    static let batch = 64
    static let noModelMessage = "Install the face recognition model in Settings, then update faces again."
    static let modelChangedMessage = "The face model changed. Start the face update again."
    static let interruptedMessage = "Interrupted. Resume to continue; faces already updated are kept."

    struct Interrupted: LocalizedError {
        var errorDescription: String? { "Face refresh stopped" }
    }

    struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    enum Source: Equatable { case available, missing, offline }

    enum Outcome: Equatable {
        case embedded(Data, jpeg: Data?)
        case missing
        case offline
        case failed(String)
    }

    /// Re-embeds the faces of one photo; a face absent from the result failed.
    protocol Backend: Sendable {
        var model: String { get }
        func embed(path: String, inputs: [FaceAnalysisCache.Input]) async -> [Int64: (embedding: Data, jpeg: Data?)]
    }

    static let outdatedSQL = """
        FROM face_prints fp JOIN files f ON f.id=fp.file_id LEFT JOIN catalog_revisions r ON r.file_id=fp.file_id
        WHERE f.failed=0 AND (length(COALESCE(fp.arcface_embedding,X''))<>512 OR fp.embedding_model IS NULL OR fp.embedding_model<>?1
          OR fp.processing_version IS NULL OR fp.processing_version<>?2 OR fp.source_revision IS NULL OR r.revision IS NULL
          OR fp.source_revision<>r.revision)
        """

    static var currentModel: String? { ArcFaceService.shared.modelVersion }
    static var processing: String { FaceAnalysisCache.processingVersion }

    static func outdated(_ db: GRDB.Database, model: String, processing: String) throws -> Int {
        try Int.fetchOne(db, sql: "SELECT COUNT(*) " + outdatedSQL, arguments: [model, processing]) ?? 0
    }

    static func recipeSpace(_ recipe: String?) -> (model: String, processing: String)? {
        guard let recipe, let object = FaceReview.parse(recipe),
              let model = object["model"] as? String, let processing = object["processing"] as? String else { return nil }
        return (model, processing)
    }

    static func status(_ db: GRDB.Database, model: String?, processing: String) throws -> FaceRefreshStatus {
        let job = try Row.fetchOne(db, sql: "SELECT state, progress, recipe_json FROM catalog_jobs WHERE id=? AND kind=?",
                                   arguments: [jobID, kind])
        var space = (model: model ?? "", processing: processing)
        if model == nil, let stored = recipeSpace(job?["recipe_json"]) { space = stored }
        var counts: [String: Int] = [:]
        for row in try Row.fetchAll(db, sql: """
            SELECT state, COUNT(*) AS n FROM face_refresh_backlog WHERE embedding_model=? AND processing_version=? GROUP BY state
            """, arguments: [space.model, space.processing]) {
            counts[row["state"]] = row["n"]
        }
        let progress: Double? = job.map { min(max($0["progress"] as Double? ?? 0, 0), 1) }
        return FaceRefreshStatus(model: space.model, processing: space.processing,
                                 outdated: space.model.isEmpty ? 0 : try outdated(db, model: space.model, processing: space.processing),
                                 total: counts.values.reduce(0, +), done: counts["done"] ?? 0, pending: counts["pending"] ?? 0,
                                 missing: counts["missing"] ?? 0, offline: counts["offline"] ?? 0, failed: counts["failed"] ?? 0,
                                 jobID: job == nil ? nil : jobID, state: job?["state"], progress: progress)
    }

    static func statusPage(_ db: GRDB.Database, requestID: String) throws -> FaceReviewPage {
        let refresh = try status(db, model: currentModel, processing: processing)
        return FaceReviewPage(requestID: requestID, action: "list", status: "ok", total: refresh.pending, offset: 0,
                              queue: "refresh", refresh: refresh)
    }

    static func recover(database: Database) async {
        try? await database.pool.write { db in try recover(db) }
    }

    static func recover(_ db: GRDB.Database) throws {
        try db.execute(sql: "UPDATE catalog_jobs SET state='paused',error=?,updated_at=? WHERE kind=? AND state IN ('running','queued')",
                       arguments: [interruptedMessage, Date().timeIntervalSince1970, kind])
    }

    static func jobs(_ db: GRDB.Database) throws -> [CatalogJob] {
        try CatalogStore.jobs(db).filter { $0.kind == kind }
    }

    static func seed(_ db: GRDB.Database, model: String, processing: String, now: Double) throws {
        try db.execute(sql: """
            INSERT OR IGNORE INTO face_refresh_backlog(face_id,embedding_model,processing_version,state,attempts,reason,updated_at)
            SELECT fp.id, ?1, ?2, 'pending', 0, NULL, ?3
            """ + " " + outdatedSQL, arguments: [model, processing, now])
        try db.execute(sql: """
            UPDATE face_refresh_backlog SET state='pending',updated_at=?3 WHERE embedding_model=?1 AND processing_version=?2
              AND state IN ('missing','offline','failed')
            """, arguments: [model, processing, now])
        try db.execute(sql: """
            UPDATE face_refresh_backlog SET state='pending',updated_at=?3 WHERE embedding_model=?1 AND processing_version=?2
              AND state='done' AND face_id IN (SELECT fp.id
            """ + " " + outdatedSQL + ")", arguments: [model, processing, now])
        let recipe = try FaceReview.jsonText(["model": model, "processing": processing])
        try db.execute(sql: "INSERT OR IGNORE INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,priority,created_at,updated_at) VALUES(?,?,'[]',?,'queued',0,?,?)",
                       arguments: [jobID, kind, recipe, now, now])
        try db.execute(sql: """
            UPDATE catalog_jobs SET state='queued',progress=0,error=NULL,checkpoint_json='{}',recipe_json=?,updated_at=?
            WHERE id=? AND kind=? AND state IN ('completed','cancelled','failed','paused')
            """, arguments: [recipe, now, jobID, kind])
        try db.execute(sql: "UPDATE catalog_jobs SET recipe_json=?,updated_at=? WHERE id=? AND kind=? AND state='queued'",
                       arguments: [recipe, now, jobID, kind])
    }

    static func enqueue(_ db: GRDB.Database, requestID: String, model: String?) throws -> CatalogResponse {
        guard let model else { throw Failure(message: noModelMessage) }
        try seed(db, model: model, processing: processing, now: Date().timeIntervalSince1970)
        return CatalogResponse(requestID: requestID, status: "ok",
                               message: "Updating faces for the current face model. People and names don't change.", jobs: try jobs(db))
    }

    /// Missing: the photo is gone but its library location is there. Offline: the
    /// location itself (or the volume it is on) is unreachable.
    static func classify(_ path: String, roots: [String]) -> Source {
        let manager = FileManager.default
        if manager.fileExists(atPath: path) { return .available }
        let root = roots.filter { path == $0 || path.hasPrefix($0.hasSuffix("/") ? $0 : $0 + "/") }.max { $0.count < $1.count }
        if let root { return manager.fileExists(atPath: root) ? .missing : .offline }
        let parts = path.split(separator: "/", omittingEmptySubsequences: true)
        if path.hasPrefix("/Volumes/"), parts.count >= 2, !manager.fileExists(atPath: "/Volumes/" + parts[1]) { return .offline }
        return .missing
    }

    static func roots(_ db: GRDB.Database) throws -> [String] {
        try String.fetchAll(db, sql: "SELECT path FROM library_roots")
    }

    static func nextBatch(_ db: GRDB.Database, model: String, processing: String, limit: Int) throws -> [FaceAnalysisCache.Input] {
        try Row.fetchAll(db, sql: """
            SELECT b.face_id, fp.file_id, fp.bbox, f.path_text, f.size_bytes, f.modified_at, fp.processing_version
            FROM face_refresh_backlog b JOIN face_prints fp ON fp.id=b.face_id JOIN files f ON f.id=fp.file_id
            WHERE b.embedding_model=? AND b.processing_version=? AND b.state='pending' ORDER BY b.face_id LIMIT ?
            """, arguments: [model, processing, limit]).map { row in
            FaceAnalysisCache.Input(id: row["face_id"], fileID: row["file_id"], bbox: row["bbox"], path: row["path_text"],
                                    size: row["size_bytes"], modified: row["modified_at"],
                                    previousProcessingVersion: row["processing_version"])
        }
    }

    static func embedBatch(_ batch: [FaceAnalysisCache.Input], roots: [String], backend: Backend) async -> [(Int64, Outcome)] {
        var byPath: [String: [FaceAnalysisCache.Input]] = [:]
        for input in batch { byPath[input.path, default: []].append(input) }
        var out: [(Int64, Outcome)] = []
        for path in byPath.keys.sorted() {
            let inputs = byPath[path] ?? []
            let shared: Outcome?
            switch classify(path, roots: roots) {
            case .missing: shared = .missing
            case .offline: shared = .offline
            case .available: shared = inputs.allSatisfy({ $0.currentIdentity() != nil }) ? nil : .failed("source_changed")
            }
            if let shared {
                out.append(contentsOf: inputs.map { ($0.id, shared) })
                continue
            }
            let results = await backend.embed(path: path, inputs: inputs)
            for input in inputs {
                if let result = results[input.id] { out.append((input.id, .embedded(result.embedding, jpeg: result.jpeg))) }
                else { out.append((input.id, .failed("no_embedding"))) }
            }
        }
        return out
    }

    /// Stores one batch's outcomes; returns the faces whose vectors were saved.
    @discardableResult
    static func persist(_ db: GRDB.Database, model: String, processing: String, batch: [FaceAnalysisCache.Input],
                        outcomes: [(Int64, Outcome)], now: Double) throws -> [Int64: Data] {
        var saved: [Int64: Data] = [:]
        for (face, outcome) in outcomes {
            guard let input = batch.first(where: { $0.id == face }) else { continue }
            let state: String
            var reason: String?
            switch outcome {
            case .embedded(let embedding, let jpeg):
                if processing == FaceAnalysisCache.processingVersion,
                   try FaceAnalysisCache.persist(db, input: input, embedding: embedding, modelVersion: model) {
                    state = "done"
                    if let jpeg { saved[face] = jpeg }
                } else {
                    state = "failed"; reason = "source_changed"
                }
            case .missing: state = "missing"
            case .offline: state = "offline"
            case .failed(let why): state = "failed"; reason = why
            }
            try db.execute(sql: """
                UPDATE face_refresh_backlog SET state=?1,reason=?2,attempts=attempts+CASE WHEN ?1='failed' THEN 1 ELSE 0 END,updated_at=?3
                WHERE face_id=?4 AND embedding_model=?5 AND processing_version=?6 AND state='pending'
                """, arguments: [state, reason, now, face, model, processing])
        }
        let row = try Row.fetchOne(db, sql: """
            SELECT COUNT(*) AS total, COALESCE(SUM(state='pending'),0) AS pending FROM face_refresh_backlog
            WHERE embedding_model=? AND processing_version=?
            """, arguments: [model, processing])
        let total: Int = row?["total"] ?? 0
        let pending: Int = row?["pending"] ?? 0
        let progress = total > 0 ? Double(total - pending) / Double(total) : 1
        try db.execute(sql: "UPDATE catalog_jobs SET progress=?,updated_at=? WHERE id=? AND state='running'",
                       arguments: [progress, now, jobID])
        return saved
    }

    static func requireRunning(_ db: GRDB.Database) throws -> (model: String, processing: String) {
        guard let row = try Row.fetchOne(db, sql: "SELECT state, recipe_json FROM catalog_jobs WHERE id=?", arguments: [jobID]),
              (row["state"] as String?) == "running" else { throw Interrupted() }
        guard let space = recipeSpace(row["recipe_json"]) else { throw Failure(message: "The face update has no model to refresh to.") }
        return space
    }

    /// Drains the backlog with `backend` until it is empty or the job stops running.
    static func drain(database: Database, backend: Backend, onBatch: () async -> Void = {}) async throws {
        while true {
            let (space, batch, roots) = try await database.pool.read { db in
                let space = try requireRunning(db)
                return (space, try nextBatch(db, model: space.model, processing: space.processing, limit: Self.batch), try Self.roots(db))
            }
            guard backend.model == space.model else { throw Failure(message: modelChangedMessage) }
            guard !batch.isEmpty else { return }
            let outcomes = await embedBatch(batch, roots: roots, backend: backend)
            let saved = try await database.pool.write { db in
                try persist(db, model: space.model, processing: space.processing, batch: batch, outcomes: outcomes,
                            now: Date().timeIntervalSince1970)
            }
            for (face, jpeg) in saved { FaceClustering.saveFaceCrop(faceID: face, jpeg: jpeg) }
            await onBatch()
        }
    }

    // MARK: - Driver

    private var active = false

    func enqueue(_ request: CatalogRequest, database: Database, sink: IPCSink) async -> CatalogResponse {
        do {
            let model = Self.currentModel
            let response = try await database.pool.write { db in try Self.enqueue(db, requestID: request.requestID, model: model) }
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

    static func run(database: Database, sink: IPCSink, backend: Backend? = nil) async {
        let started = (try? await database.pool.write { db -> Bool in
            try db.execute(sql: "UPDATE catalog_jobs SET state='running',error=NULL,updated_at=? WHERE id=? AND kind=? AND state='queued'",
                           arguments: [Date().timeIntervalSince1970, jobID, kind])
            return db.changesCount == 1
        }) ?? false
        guard started else { return }
        await publish(database: database, sink: sink)
        var failure: String?
        do {
            guard let backend = backend ?? NativeBackend.load() else { throw Failure(message: noModelMessage) }
            var published = Date()
            try await drain(database: database, backend: backend) {
                if Date().timeIntervalSince(published) >= 0.5 {
                    await publish(database: database, sink: sink)
                    published = Date()
                }
            }
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

    struct NativeBackend: Backend {
        let model: String

        static func load() -> NativeBackend? {
            if !ArcFaceService.shared.isReady {
                for kind in FaceEmbedderKind.installedKinds() {
                    _ = ArcFaceService.shared.load(kind)
                    break
                }
            }
            guard ArcFaceService.shared.isReady, let model = ArcFaceService.shared.modelVersion else { return nil }
            return NativeBackend(model: model)
        }

        func embed(path: String, inputs: [FaceAnalysisCache.Input]) async -> [Int64: (embedding: Data, jpeg: Data?)] {
            let extracted = await FaceClustering.extractOneFile(path: path, rows: inputs)
            var out: [Int64: (embedding: Data, jpeg: Data?)] = [:]
            for face in extracted { out[face.input.id] = (face.arcFace, face.jpeg) }
            return out
        }
    }
}
