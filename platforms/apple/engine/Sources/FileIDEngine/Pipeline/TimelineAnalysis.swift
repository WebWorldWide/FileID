import Foundation
import AVFoundation
import ImageIO
import GRDB
import FileIDShared

actor TimelineAnalysis {
    static let shared = TimelineAnalysis()
    private var scheduled: Set<String> = []
    private var runningID: String?
    private var samplerTask: Task<VideoFrameWorker.Sample, Error>?
    private struct Recipe: Codable, Sendable { let modelKind: String; let modelVersion: String; let intervalSeconds: Double }

    func enqueue(_ request: CatalogRequest, database: Database, sink: IPCSink) async -> CatalogResponse {
        do {
            guard !request.requestID.isEmpty, request.requestID.count <= 200, let ids = request.fileIDs, !ids.isEmpty, ids.count <= 1000 else { throw CatalogStore.InvalidRequest() }
            guard case .ready(let model) = await DeepAnalyze.shared.loadState else {
                return CatalogResponse(requestID: request.requestID, status: "error", message: "Load a visual model in Deep Analyze before starting timeline analysis. No model will be downloaded automatically.")
            }
            let unique = Array(Set(ids)).sorted()
            let id = UUID().uuidString
            let encodedIDs = String(decoding: try JSONEncoder().encode(unique), as: UTF8.self)
            let recipe = String(decoding: try JSONEncoder().encode(Recipe(modelKind: model.rawValue, modelVersion: "timeline-frame-v1/" + model.rawValue + "@" + (ModelManifest.vlmPin(forRepo: model.sourceRepo)?.revision ?? "unversioned"), intervalSeconds: 10)), as: UTF8.self)
            let now = Date().timeIntervalSince1970
            try await database.pool.write { db in
                for fileID in unique {
                    guard let kind = try String.fetchOne(db, sql: "SELECT kind FROM files WHERE id=?", arguments: [fileID]), kind == "video" else { throw CatalogStore.InvalidRequest() }
                }
                try db.execute(sql: "INSERT INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,created_at,updated_at) VALUES(?,'timelineSample',?,?,'queued',?,?)", arguments: [id,encodedIDs,recipe,now,now])
            }
            await schedule(id, database: database, sink: sink)
            return CatalogResponse(requestID: request.requestID, status: "ok", message: "Visual sampling checks one frame every ten seconds. It can miss fast events; coverage will remain incomplete.", jobs: try await CatalogStore.jobs(database))
        } catch {
            return CatalogResponse(requestID: request.requestID, status: "error", message: error.localizedDescription)
        }
    }

    func recover(database: Database) async {
        VideoFrameWorker.sweepAbandonedFrames()
        try? await database.pool.write { db in
            try db.execute(sql: "UPDATE catalog_jobs SET state='paused',error='Interrupted. Load the visual model and resume.',updated_at=? WHERE state IN ('running','queued') AND kind='timelineSample'", arguments: [Date().timeIntervalSince1970])
        }
    }

    func control(_ request: CatalogRequest, database: Database, sink: IPCSink) async -> CatalogResponse {
        if request.action == "resumeJob", case .ready = await DeepAnalyze.shared.loadState {} else if request.action == "resumeJob" {
            return CatalogResponse(requestID: request.requestID, status: "error", message: "Load the visual model before resuming.")
        }
        let result = await CatalogStore.handle(request, database: database)
        if result.status == "ok", let id = request.jobID {
            if request.action == "resumeJob" { await schedule(id, database: database, sink: sink) }
            else if id == runningID { samplerTask?.cancel(); await DeepAnalyze.shared.requestCancel() }
        }
        return result
    }

    private func schedule(_ id: String, database: Database, sink: IPCSink) async {
        guard scheduled.insert(id).inserted else { return }
        await JobQueue.shared.enqueue(.init(id: "timeline-" + id, category: .deepAnalyze, title: "Video timeline", etaSeconds: nil) {
            await TimelineAnalysis.shared.run(id, database: database, sink: sink)
        })
    }

    private func run(_ id: String, database: Database, sink: IPCSink) async {
        runningID = id

        do {
            let job: (ids: [Int64], model: String, version: String, index: Int) = try await database.pool.write { db in
                guard let row = try Row.fetchOne(db, sql: "SELECT * FROM catalog_jobs WHERE id=? AND state='queued' AND kind='timelineSample'", arguments: [id]) else { throw Interrupted() }
                let json: String = row["file_ids_json"]
                let checkpoint: String = row["checkpoint_json"]
                let index = Int(checkpoint) ?? 0
                try db.execute(sql: "UPDATE catalog_jobs SET state='running',error=NULL,updated_at=? WHERE id=?", arguments: [Date().timeIntervalSince1970,id])
                let recipeJSON: String = row["recipe_json"]
                let recipe = try JSONDecoder().decode(Recipe.self, from: Data(recipeJSON.utf8))
                return (try JSONDecoder().decode([Int64].self, from: Data(json.utf8)), recipe.modelKind, recipe.modelVersion, index)
            }
            await DeepAnalyze.shared.clearCancel()
            for (index, fileID) in job.ids.enumerated() where index >= job.index {
                try await requireRunning(id, database: database)
                guard case .ready(let model) = await DeepAnalyze.shared.loadState, model.rawValue == job.model else { throw ModelChanged() }
                let source: (String, String) = try await database.pool.read { db in
                    guard let path = try String.fetchOne(db, sql: "SELECT path_text FROM files WHERE id=?", arguments: [fileID]) else { throw CatalogStore.InvalidRequest() }
                    return (path, try CatalogStore.revision(db, fileID: fileID))
                }
                let url = URL(fileURLWithPath: source.0)
                let first = try await sample(source: url, seconds: 0)
                defer { try? FileManager.default.removeItem(at: first.url) }
                let duration = first.metadata.duration
                guard first.metadata.revision == source.1 else { throw SourceChanged() }
                try await database.pool.write { db in
                    try db.execute(sql: "INSERT OR REPLACE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?,?,0,?,'incomplete',?,?)", arguments: [id+"-"+String(fileID)+"-coverage",fileID,duration,source.1,job.version])
                }
                var seconds = 0.0
                while seconds < duration {
                    try await requireRunning(id, database: database)
                    guard case .ready(let activeModel) = await DeepAnalyze.shared.loadState, activeModel.rawValue == job.model else { throw ModelChanged() }
                    let evidenceID = "frame:\(fileID):\(source.1):\(job.version):\(Int(seconds))"
                    let exists = try await database.pool.read { db in try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM catalog_passages WHERE id=? AND source_revision=? AND stale=0)", arguments: [evidenceID,source.1]) ?? false }
                    if !exists {
                        let frame = seconds == 0 ? first : try await sample(source: url, seconds: seconds)
                        defer { if frame.url != first.url { try? FileManager.default.removeItem(at: frame.url) } }
                        guard frame.metadata.revision == source.1 else { throw SourceChanged() }
                        try await requireRunning(id, database: database)
                        let temporary = frame.url
                        let result = await DeepAnalyze.shared.runCancellableAnalysis {
                            await DeepAnalyze.shared.analyze(imageURL: temporary, mediaKind: .video)
                        }
                        try await requireRunning(id, database: database)
                        guard !DeepAnalyzeRunner.isAnalysisFailure(result) else { throw UnreadableVideo() }
                        let caption = result.description
                        let start = frame.metadata.seconds
                        try await database.pool.write { db in
                            guard try CatalogStore.revision(db, fileID: fileID) == source.1 else { throw SourceChanged() }
                            try db.execute(sql: "INSERT OR REPLACE INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES(?,?,?,?,?,?,?,0)", arguments: [evidenceID,fileID,start,start,caption,source.1,job.version])
                            try db.execute(sql: "INSERT OR REPLACE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?,?,?,?,'sampled',?,?)", arguments: [evidenceID,fileID,start,min(duration,start+0.001),source.1,job.version])
                        }
                    }
                    seconds += 10
                }
                try await requireRunning(id, database: database)
                try await persistChapterSuggestions(
                    fileID: fileID,
                    sourceRevision: source.1,
                    frameModelVersion: job.version,
                    duration: duration,
                    database: database
                )
                let progress = Double(index+1)/Double(job.ids.count)
                try await database.pool.write { db in
                    try db.execute(sql: "UPDATE catalog_jobs SET checkpoint_json=?,progress=?,updated_at=? WHERE id=? AND state='running'", arguments: [String(index+1),progress,Date().timeIntervalSince1970,id])
                }
                await publish(id, database: database, sink: sink)
            }
            try await database.pool.write { db in
                try db.execute(sql: "UPDATE catalog_jobs SET state='completed',progress=1,updated_at=? WHERE id=? AND state='running'", arguments: [Date().timeIntervalSince1970,id])
            }
        } catch is Interrupted {} catch {
            let message = error.localizedDescription
            try? await database.pool.write { db in
                try db.execute(sql: "UPDATE catalog_jobs SET state='failed',error=?,updated_at=? WHERE id=? AND state='running'", arguments: [message,Date().timeIntervalSince1970,id])
            }
        }
        await DeepAnalyze.shared.clearCancel()
        await publish(id, database: database, sink: sink)
        runningID = nil
        scheduled.remove(id)
        let queued = (try? await database.pool.read { db in try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [id]) }) == "queued"
        if queued { await schedule(id, database: database, sink: sink) }
    }

    private func requireRunning(_ id: String, database: Database) async throws {
        let state = try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [id]) }
        guard state == "running" else { throw Interrupted() }
    }

    func persistChapterSuggestions(
        fileID: Int64,
        sourceRevision: String,
        frameModelVersion: String,
        duration: Double,
        database: Database
    ) async throws {
        let captions = try await database.pool.read { db in
            try Row.fetchAll(db, sql: """
                SELECT start_seconds,text FROM catalog_passages
                WHERE file_id=? AND source_revision=? AND model_version=? AND stale=0
                  AND start_seconds IS NOT NULL AND confidence=0
                ORDER BY start_seconds,id
                """, arguments: [fileID, sourceRevision, frameModelVersion]).map { row in
                    TimelineChapterSuggestions.Caption(seconds: row["start_seconds"], text: row["text"])
                }
        }
        let suggestions = TimelineChapterSuggestions.propose(
            fileID: fileID,
            sourceRevision: sourceRevision,
            frameModelVersion: frameModelVersion,
            duration: duration,
            captions: captions
        )
        guard !suggestions.isEmpty else { return }

        try await database.pool.write { db in
            guard try CatalogStore.revision(db, fileID: fileID) == sourceRevision else { throw SourceChanged() }
            try db.execute(sql: "DELETE FROM catalog_chapters WHERE file_id=? AND user_edited=0 AND model_version LIKE 'timeline-chapter-suggestion-v1/%'", arguments: [fileID])
            for chapter in suggestions {
                let userEdited = try Bool.fetchOne(db, sql: "SELECT user_edited FROM catalog_chapters WHERE id=?", arguments: [chapter.id]) ?? false
                if !userEdited { try CatalogStore.upsertChapter(db, chapter: chapter) }
            }
        }
    }

    private func publish(_ id: String, database: Database, sink: IPCSink) async {
        if let jobs = try? await CatalogStore.jobs(database) {
            await sink.emit(.catalogResponse(CatalogResponse(requestID: id, status: "ok", jobs: jobs)))
        }
    }

    private func sample(source: URL, seconds: Double) async throws -> VideoFrameWorker.Sample {
        let task = Task { try await VideoFrameWorker.sample(source: source, seconds: seconds) }
        samplerTask = task
        defer { samplerTask = nil }
        return try await task.value
    }
    private struct Interrupted: Error {}
    private struct UnreadableVideo: LocalizedError { var errorDescription: String? { "A video frame could not be read or analyzed. Partial coverage remains explicitly incomplete." } }
    private struct SourceChanged: LocalizedError { var errorDescription: String? { "The source changed during analysis. Rescan it before continuing." } }
    private struct ModelChanged: LocalizedError { var errorDescription: String? { "The loaded model changed. Load the original model before continuing." } }
}
