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
    private var sequenceTask: Task<[VideoFrameWorker.Sample], Error>?
    private var signalTask: Task<[TimelineSignalAnalysis.Signal], Error>?
    private var speechTask: Task<[TimelineSpeechTranscription.Passage]?, Never>?
    private struct Recipe: Codable, Sendable {
        let modelKind: String
        let modelVersion: String
        let intervalSeconds: Double
        let timelineMode: String?
    }

    func enqueue(_ request: CatalogRequest, database: Database, sink: IPCSink) async -> CatalogResponse {
        do {
            guard !request.requestID.isEmpty, request.requestID.count <= 200, let ids = request.fileIDs, !ids.isEmpty, ids.count <= 1000 else { throw CatalogStore.InvalidRequest() }
            if let mode = request.timelineMode, !["sampled", "moments"].contains(mode) { throw CatalogStore.InvalidRequest() }
            guard case .ready(let model) = await DeepAnalyze.shared.loadState else {
                return CatalogResponse(requestID: request.requestID, status: "error", message: "Load a visual model in Deep Analyze before starting timeline analysis. No model will be downloaded automatically.")
            }
            let unique = Array(Set(ids)).sorted()
            let id = UUID().uuidString
            let encodedIDs = String(decoding: try JSONEncoder().encode(unique), as: UTF8.self)
            let recipe = String(decoding: try JSONEncoder().encode(Recipe(modelKind: model.rawValue, modelVersion: "timeline-frame-v2/" + model.rawValue + "@" + (ModelManifest.vlmPin(forRepo: model.sourceRepo)?.revision ?? "unversioned"), intervalSeconds: 10, timelineMode: request.timelineMode)), as: UTF8.self)
            let now = Date().timeIntervalSince1970
            try await database.pool.write { db in
                for fileID in unique {
                    guard let kind = try String.fetchOne(db, sql: "SELECT kind FROM files WHERE id=?", arguments: [fileID]), kind == "video" else { throw CatalogStore.InvalidRequest() }
                }
                try db.execute(sql: "INSERT INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,created_at,updated_at) VALUES(?,'timelineSample',?,?,'queued',?,?)", arguments: [id,encodedIDs,recipe,now,now])
            }
            await schedule(id, database: database, sink: sink)
            if request.timelineMode == "moments" {
                return CatalogResponse(requestID: request.requestID, status: "ok",
                                       message: "Queued significant-moment drafts from overlapping frame sequences and on-device speech. Review every proposal; sampled frames can still miss actions or hidden outcomes.",
                                       jobs: try await CatalogStore.jobs(database))
            }
            return CatalogResponse(requestID: request.requestID, status: "ok", message: "Visual sampling uses low-resolution change signals to select a representative moment in each ten-second window, along with a starting frame. Fast events can still be missed. When on-device speech recognition is available, timestamped transcript passages are added.", jobs: try await CatalogStore.jobs(database))
        } catch {
            return CatalogResponse(requestID: request.requestID, status: "error", message: error.localizedDescription)
        }
    }

    func recover(database: Database) async {
        VideoFrameWorker.sweepAbandonedFrames()
        TimelineSpeechTranscription.sweepAbandonedAudio()
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
            else if id == runningID {
            samplerTask?.cancel()
            sequenceTask?.cancel()
                signalTask?.cancel()
                speechTask?.cancel()
                await DeepAnalyze.shared.requestCancel()
            }
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
            let job: (ids: [Int64], model: String, version: String, index: Int, intervalSeconds: Double, timelineMode: String?) = try await database.pool.write { db in
                guard let row = try Row.fetchOne(db, sql: "SELECT * FROM catalog_jobs WHERE id=? AND state='queued' AND kind='timelineSample'", arguments: [id]) else { throw Interrupted() }
                let json: String = row["file_ids_json"]
                let checkpoint: String = row["checkpoint_json"]
                let index = Int(checkpoint) ?? 0
                try db.execute(sql: "UPDATE catalog_jobs SET state='running',error=NULL,updated_at=? WHERE id=?", arguments: [Date().timeIntervalSince1970,id])
                let recipeJSON: String = row["recipe_json"]
                let recipe = try JSONDecoder().decode(Recipe.self, from: Data(recipeJSON.utf8))
                if let mode = recipe.timelineMode, !["sampled", "moments"].contains(mode) { throw CatalogStore.InvalidRequest() }
                return (try JSONDecoder().decode([Int64].self, from: Data(json.utf8)), recipe.modelKind, recipe.modelVersion, index, recipe.intervalSeconds, recipe.timelineMode)
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
                var signals: [TimelineSignalAnalysis.Signal] = []
                var signalStart = 0.0
                while signalStart < duration {
                    try await requireRunning(id, database: database)
                    guard case .ready(let activeModel) = await DeepAnalyze.shared.loadState,
                          activeModel.rawValue == job.model else { throw ModelChanged() }
                    let signalEnd = min(duration, signalStart + 60)
                    try await recordSignalCoverage(
                        fileID: fileID,
                        sourceRevision: source.1,
                        start: signalStart,
                        end: signalEnd,
                        status: "incomplete",
                        database: database
                    )
                    let chunk: [TimelineSignalAnalysis.Signal]
                    do {
                        chunk = try await scanSignals(
                            source: url,
                            start: max(0, signalStart - 1),
                            end: signalEnd
                        )
                    } catch is VideoFrameWorker.SourceRevisionChanged {
                        throw SourceChanged()
                    } catch {
                        try await requireRunning(id, database: database)
                        break
                    }
                    try await recordSignalCoverage(
                        fileID: fileID,
                        sourceRevision: source.1,
                        start: signalStart,
                        end: signalEnd,
                        status: "sampled",
                        database: database
                    )
                    let previousSignalTime = signals.last?.seconds ?? -1
                    signals.append(contentsOf: chunk.filter {
                        $0.seconds >= signalStart && $0.seconds > previousSignalTime + 0.1
                    })
                    signalStart = signalEnd
                }

                let sampleTimes = TimelineSignalAnalysis.sampleTimes(
                    duration: duration,
                    intervalSeconds: job.intervalSeconds,
                    signals: signals
                )
                for seconds in sampleTimes {
                    try await requireRunning(id, database: database)
                    guard case .ready(let activeModel) = await DeepAnalyze.shared.loadState, activeModel.rawValue == job.model else { throw ModelChanged() }
                    let evidenceID = "frame:\(fileID):\(source.1):\(job.version):\(Int(seconds * 1_000))"
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
                }
                if job.timelineMode == "moments" {
                    try await analyzeMoments(fileID: fileID, sourceURL: url, sourceRevision: source.1,
                                             duration: duration, signals: signals, model: job.model,
                                             modelVersion: job.version, fileIndex: index, totalFiles: job.ids.count,
                                             jobID: id, database: database, sink: sink)
                }
                try await transcribeSpeech(
                    fileID: fileID,
                    sourceURL: url,
                    sourceRevision: source.1,
                    duration: duration,
                    fileIndex: index,
                    totalFiles: job.ids.count,
                    jobID: id,
                    database: database,
                    sink: sink
                )
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

    private func analyzeMoments(fileID: Int64, sourceURL: URL, sourceRevision: String, duration: Double,
                                signals: [TimelineSignalAnalysis.Signal], model: String, modelVersion: String,
                                fileIndex: Int, totalFiles: Int, jobID: String, database: Database, sink: IPCSink) async throws {
        let windows = TimelineMomentAnalysis.windows(duration: duration, signals: signals)
        guard !windows.isEmpty else { throw UnreadableVideo() }
        let version = "timeline-moment-sequence-v1/" + modelVersion
        for (index, window) in windows.enumerated() {
            try await requireRunning(jobID, database: database)
            guard case .ready(let activeModel) = await DeepAnalyze.shared.loadState,
                  activeModel.rawValue == model else { throw ModelChanged() }
            let coverageID = "moment-window:\(fileID):\(sourceRevision):\(version):\(Int(window.start * 1_000))"
            let processed = try await database.pool.read { db in
                try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM catalog_coverage WHERE id=? AND status='sampled' AND source_revision=? AND model_version=?)",
                                   arguments: [coverageID,sourceRevision,version]) ?? false
            }
            if !processed {
                try await database.pool.write { db in
                    guard try CatalogStore.revision(db, fileID: fileID) == sourceRevision else { throw SourceChanged() }
                    try db.execute(sql: "INSERT OR REPLACE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?,?,?,?,'incomplete',?,?)",
                                   arguments: [coverageID,fileID,window.start,window.end,sourceRevision,version])
                }
                var frames: [VideoFrameWorker.Sample] = []
                defer { for frame in frames { try? FileManager.default.removeItem(at: frame.url) } }
                frames = try await sampleSequence(source: sourceURL, times: window.times)
                try await requireRunning(jobID, database: database)
                guard frames.allSatisfy({ $0.metadata.revision == sourceRevision }) else { throw SourceChanged() }
                let distinct = try TimelineMomentAnalysis.distinctFrameIndices(
                    times: frames.map { $0.metadata.seconds }, window: window
                )
                let urls = distinct.map { frames[$0].url }
                let times = distinct.map { frames[$0].metadata.seconds }
                let result = await DeepAnalyze.shared.runCancellableAnalysis {
                    await DeepAnalyze.shared.analyzeMomentSequence(imageURLs: urls, times: times)
                }
                try await requireRunning(jobID, database: database)
                guard case .ready(let activeModel) = await DeepAnalyze.shared.loadState,
                      activeModel.rawValue == model else { throw ModelChanged() }
                guard !DeepAnalyzeRunner.isAnalysisFailure(result) else { throw UnreadableVideo() }
                let verifiedFrame = try await sample(source: sourceURL, seconds: window.times.last!)
                defer { try? FileManager.default.removeItem(at: verifiedFrame.url) }
                guard verifiedFrame.metadata.revision == sourceRevision else { throw SourceChanged() }
                let moments = try TimelineMomentAnalysis.parse(result.description, frameCount: distinct.count)
                let chapters = TimelineMomentAnalysis.chapters(moments: moments, times: times, window: window,
                                                               fileID: fileID, sourceRevision: sourceRevision, modelVersion: version)
                try await Self.persistMomentWindow(chapters: chapters, coverageID: coverageID, window: window,
                                                   fileID: fileID, sourceRevision: sourceRevision, modelVersion: version,
                                                   jobID: jobID, database: database)
            }
            let progress = min(1, (Double(fileIndex) + 0.7 * Double(index + 1) / Double(windows.count)) / Double(max(1, totalFiles)))
            try await database.pool.write { db in
                try db.execute(sql: "UPDATE catalog_jobs SET progress=MAX(progress,?),updated_at=? WHERE id=? AND state='running'",
                               arguments: [progress,Date().timeIntervalSince1970,jobID])
            }
            await publish(jobID, database: database, sink: sink)
        }
    }

    static func persistMomentWindow(chapters: [CatalogChapter], coverageID: String, window: TimelineMomentAnalysis.Window,
                                     fileID: Int64, sourceRevision: String, modelVersion: String,
                                     jobID: String, database: Database) async throws {
        try await database.pool.write { db in
            guard try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [jobID]) == "running" else { throw Interrupted() }
            guard try CatalogStore.revision(db, fileID: fileID) == sourceRevision else { throw SourceChanged() }
            let prefix = "timeline-moment:\(fileID):\(sourceRevision):\(Int(window.start * 1_000)):"
            try db.execute(sql: "DELETE FROM catalog_chapters WHERE file_id=? AND user_edited=0 AND substr(id,1,?)=?",
                           arguments: [fileID,prefix.count,prefix])
            for chapter in chapters {
                let edited = try Bool.fetchOne(db, sql: "SELECT user_edited FROM catalog_chapters WHERE id=?", arguments: [chapter.id]) ?? false
                if !edited { try CatalogStore.upsertChapter(db, chapter: chapter) }
            }
            try db.execute(sql: "INSERT OR REPLACE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?,?,?,?,'sampled',?,?)",
                           arguments: [coverageID,fileID,window.start,window.end,sourceRevision,modelVersion])
        }
    }

    private func requireRunning(_ id: String, database: Database) async throws {
        let state = try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [id]) }
        guard state == "running" else { throw Interrupted() }
    }

    private func transcribeSpeech(
        fileID: Int64,
        sourceURL: URL,
        sourceRevision: String,
        duration: Double,
        fileIndex: Int,
        totalFiles: Int,
        jobID: String,
        database: Database,
        sink: IPCSink
    ) async throws {
        guard duration.isFinite, duration > 0,
              await TimelineSpeechTranscription.hasAudioTrack(url: sourceURL) else { return }

        let modelVersion = TimelineSpeechTranscription.modelVersion
        let chunks = TimelineSpeechTranscription.chunks(duration: duration)
        guard !chunks.isEmpty else { return }
        TimelineSpeechTranscription.sweepAbandonedAudio()
        let coverageID = "speech-coverage:\(fileID):\(sourceRevision):\(modelVersion)"
        try await database.pool.write { db in
            guard try CatalogStore.revision(db, fileID: fileID) == sourceRevision else { throw SourceChanged() }
            try db.execute(sql: "INSERT OR IGNORE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?,?,0,?,'incomplete',?,?)", arguments: [coverageID,fileID,duration,sourceRevision,modelVersion])
        }
        guard await TimelineSpeechTranscription.isAvailableOnDevice() else { return }

        var fullyProcessed = true
        for (index, chunk) in chunks.enumerated() {
            try await requireRunning(jobID, database: database)
            let chunkID = "\(coverageID):chunk:\(index)"
            let alreadyVerified = try await database.pool.read { db in
                try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM catalog_coverage WHERE id=? AND status='verified' AND source_revision=? AND model_version=?)", arguments: [chunkID,sourceRevision,modelVersion]) ?? false
            }
            if alreadyVerified {
                try await publishSpeechProgress(
                    jobID: jobID,
                    fileIndex: fileIndex,
                    totalFiles: totalFiles,
                    chunkIndex: index,
                    chunkCount: chunks.count,
                    database: database,
                    sink: sink
                )
                continue
            }
            let task = Task {
                await TimelineSpeechTranscription.transcribe(videoURL: sourceURL, chunk: chunk, mediaDuration: duration)
            }
            speechTask = task
            let passages = await task.value
            speechTask = nil
            try await requireRunning(jobID, database: database)

            try await persistSpeechChunk(
                fileID: fileID,
                sourceRevision: sourceRevision,
                modelVersion: modelVersion,
                chunkID: chunkID,
                chunk: chunk,
                passages: passages,
                database: database
            )
            fullyProcessed = fullyProcessed && passages != nil
            try await publishSpeechProgress(
                jobID: jobID,
                fileIndex: fileIndex,
                totalFiles: totalFiles,
                chunkIndex: index,
                chunkCount: chunks.count,
                database: database,
                sink: sink
            )
        }

        let coverageStatus = fullyProcessed ? "verified" : "incomplete"
        try await database.pool.write { db in
            guard try CatalogStore.revision(db, fileID: fileID) == sourceRevision else { throw SourceChanged() }
            try db.execute(sql: "UPDATE catalog_coverage SET status=? WHERE id=?", arguments: [coverageStatus,coverageID])
        }
    }

    private func publishSpeechProgress(
        jobID: String,
        fileIndex: Int,
        totalFiles: Int,
        chunkIndex: Int,
        chunkCount: Int,
        database: Database,
        sink: IPCSink
    ) async throws {
        let progress = min(1, (Double(fileIndex) + Double(chunkIndex + 1) / Double(chunkCount)) / Double(totalFiles))
        try await database.pool.write { db in
            try db.execute(sql: "UPDATE catalog_jobs SET progress=?,updated_at=? WHERE id=? AND state='running'", arguments: [progress,Date().timeIntervalSince1970,jobID])
        }
        await publish(jobID, database: database, sink: sink)
    }

    func persistSpeechChunk(
        fileID: Int64,
        sourceRevision: String,
        modelVersion: String,
        chunkID: String,
        chunk: TimelineSpeechTranscription.Chunk,
        passages: [TimelineSpeechTranscription.Passage]?,
        database: Database
    ) async throws {
        let status = passages == nil ? "incomplete" : "verified"
        try await database.pool.write { db in
            guard try CatalogStore.revision(db, fileID: fileID) == sourceRevision else { throw SourceChanged() }
            try db.execute(sql: "INSERT OR REPLACE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?,?,?,?,?,?,?)", arguments: [chunkID,fileID,chunk.extractStart,chunk.extractEnd,status,sourceRevision,modelVersion])
            guard let passages else { return }
            for (passageIndex, passage) in passages.enumerated() {
                let startMillis = Int((passage.startSeconds * 1000).rounded())
                let passageID = "speech:\(fileID):\(sourceRevision):\(modelVersion):\(startMillis):\(passageIndex)"
                try db.execute(sql: "INSERT OR REPLACE INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES(?,?,?,?,?,?,?,?)", arguments: [passageID,fileID,passage.startSeconds,passage.endSeconds,passage.text,sourceRevision,modelVersion,passage.confidence])
            }
        }
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

    private func scanSignals(source: URL, start: Double, end: Double) async throws -> [TimelineSignalAnalysis.Signal] {
        let task = Task {
            try await VideoFrameWorker.scanSignals(source: source, start: start, end: end, interval: 1)
        }
        signalTask = task
        defer { signalTask = nil }
        return try await task.value
    }

    private func recordSignalCoverage(
        fileID: Int64,
        sourceRevision: String,
        start: Double,
        end: Double,
        status: String,
        database: Database
    ) async throws {
        guard start.isFinite, end.isFinite, end > start,
              ["incomplete", "sampled"].contains(status) else { throw CatalogStore.InvalidRequest() }
        let id = "visual-change:\(fileID):\(sourceRevision):signal-v1:\(Int(start * 1_000))"
        try await database.pool.write { db in
            guard try CatalogStore.revision(db, fileID: fileID) == sourceRevision else { throw SourceChanged() }
            try db.execute(
                sql: "INSERT OR REPLACE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?,?,?,?,?,?,?)",
                arguments: [id, fileID, start, end, status, sourceRevision, "visual-change-signal-v1"]
            )
        }
    }

    private func sample(source: URL, seconds: Double) async throws -> VideoFrameWorker.Sample {
        let task = Task { try await VideoFrameWorker.sample(source: source, seconds: seconds) }
        samplerTask = task
        defer { samplerTask = nil }
        return try await task.value
    }
    private func sampleSequence(source: URL, times: [Double]) async throws -> [VideoFrameWorker.Sample] {
        let task = Task { try await VideoFrameWorker.sampleSequence(source: source, times: times) }
        sequenceTask = task
        defer { sequenceTask = nil }
        return try await task.value
    }

    private struct Interrupted: Error {}
    private struct UnreadableVideo: LocalizedError { var errorDescription: String? { "A video frame could not be read or analyzed. Partial coverage remains explicitly incomplete." } }
    private struct SourceChanged: LocalizedError { var errorDescription: String? { "The source changed during analysis. Rescan it before continuing." } }
    private struct ModelChanged: LocalizedError { var errorDescription: String? { "The loaded model changed. Load the original model before continuing." } }
}
