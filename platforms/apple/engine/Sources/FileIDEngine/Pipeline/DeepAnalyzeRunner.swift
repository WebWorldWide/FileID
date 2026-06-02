// DeepAnalyzeRunner — batch driver around `DeepAnalyze.shared`.
//
// Glue between IPC commands and the model. Picks the right set of files
// to process (single / folder prefix / library), runs the VLM serially
// (Deep Analyze is GPU-bound — concurrent calls would just thrash MLX),
// emits progress + per-file events, and persists results to the
// `vlm_*` columns added in migration v3.
import Foundation
import GRDB
import FileIDShared

public enum DeepAnalyzeScope: Sendable {
    case singleFile(Int64)
    case folder(prefix: String)
    case wholeLibrary(skipExisting: Bool)
}

public enum DeepAnalyzeRunner {

    /// Resolve scope → ordered list of (id, path) pairs. Targets images,
    /// videos, and PDFs — for videos we extract a keyframe; for PDFs we
    /// render the first page; for images we feed the file directly. The
    /// VLM captions all three. WholeLibrary skips files already
    /// described by the requested model when `skipExisting=true`.
    public static func resolveTargets(
        database: Database,
        scope: DeepAnalyzeScope,
        modelKey: String
    ) async throws -> [(id: Int64, path: String)] {
        struct Target: Sendable { let id: Int64; let path: String }
        let rows: [Target] = try await database.pool.read { db in
            switch scope {
            case .singleFile(let id):
                let r = try GRDB.Row.fetchOne(db, sql: """
                    SELECT id, path_text FROM files
                    WHERE id = ? AND kind IN ('image', 'pdf', 'video', 'doc') AND failed = 0
                    """, arguments: [id])
                if let r { return [Target(id: r["id"] ?? 0, path: r["path_text"] ?? "")] }
                return []
            case .folder(let prefix):
                let p = prefix.hasSuffix("/") ? prefix : prefix + "/"
                let r = try GRDB.Row.fetchAll(db, sql: """
                    SELECT id, path_text FROM files
                    WHERE kind IN ('image', 'pdf', 'video', 'doc') AND failed = 0
                      AND (path_text = ? OR path_text LIKE ?)
                    ORDER BY scanned_at ASC
                    """, arguments: [prefix, p + "%"])
                return r.map { Target(id: $0["id"] ?? 0, path: $0["path_text"] ?? "") }
            case .wholeLibrary(let skipExisting):
                let sql: String
                let args: StatementArguments
                if skipExisting {
                    sql = """
                        SELECT id, path_text FROM files
                        WHERE kind IN ('image', 'pdf', 'video', 'doc') AND failed = 0
                          AND (vlm_model IS NULL OR vlm_model != ?)
                        ORDER BY scanned_at ASC
                        """
                    args = [modelKey]
                } else {
                    sql = """
                        SELECT id, path_text FROM files
                        WHERE kind IN ('image', 'pdf', 'video', 'doc') AND failed = 0
                        ORDER BY scanned_at ASC
                        """
                    args = []
                }
                let r = try GRDB.Row.fetchAll(db, sql: sql, arguments: args)
                return r.map { Target(id: $0["id"] ?? 0, path: $0["path_text"] ?? "") }
            }
        }
        return rows.map { ($0.id, $0.path) }
    }

    /// Run the batch. Streams progress via `sink`. Holds a SleepGuard
    /// for the duration so the system stays awake (lid-closed friendly).
    public static func run(
        database: Database,
        sink: IPCSink,
        scope: DeepAnalyzeScope,
        modelKind: AIModelKind
    ) async {
        let started = Date()
        let modelKey = modelKind.rawValue

        // No inline face clustering — it's a separate job. When
        // clusters are present captions use real names; otherwise
        // "person" stands in (graceful degrade).

        SleepGuard.shared.begin(reason: "Deep Analyze (\(modelKind.displayName))")
        defer { SleepGuard.shared.end() }

        // Defensive RAM check — the UI hides too-big models, but a stale
        // IPC command (e.g. user picked a big model on a different Mac
        // and it persisted in UserDefaults) could still arrive. Loading
        // a model that won't fit OOM-kills the engine with no recovery
        // for the in-flight job. Reject up front instead.
        let ramGB = Hardware.physicalMemoryGB
        guard modelKind.fits(ramGB: ramGB) else {
            let msg = "\(modelKind.displayName) needs ~\(String(format: "%.1f", modelKind.ramBudgetGB)) GB resident RAM. This Mac has \(Int(ramGB)) GB total — loading would OOM-kill the engine. Pick a smaller model in Settings → AI Models."
            JSONLog.shared.info(ev: "deep_model_too_big",
                                 extra: ["model": AnyCodable(modelKey),
                                         "needGB": AnyCodable(modelKind.ramBudgetGB),
                                         "haveGB": AnyCodable(ramGB)])
            await sink.emit(.error(EngineError(kind: "deep_model_too_big", message: msg)))
            await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                processed: 0, failed: 0,
                totalSeconds: Date().timeIntervalSince(started),
                modelKind: modelKey, cancelled: false
            )))
            return
        }

        // Reset cancellation state from any previous run.
        await DeepAnalyze.shared.clearCancel()

        // 1. Load the model (download if needed, with progress events).
        // Tell the UI we're entering the multi-second cold-load window
        // so the startingCard can update its label from "Queued" to
        // "Loading <model>…". Without this the user stares at the same
        // "Queued" message for 10s.
        await sink.emit(.deepAnalyzeStarting(DeepAnalyzeStarting(
            modelKind: modelKey,
            phase: .loadingModel,
            message: "Loading \(modelKind.displayName)…"
        )))
        do {
            try await DeepAnalyze.shared.ensureLoaded(kind: modelKind) { frac, msg, done, total in
                Task {
                    await sink.emit(.modelDownloadProgress(ModelDownloadProgress(
                        modelKind: modelKey, fraction: frac, message: msg,
                        bytesDone: done > 0 ? done : nil,
                        totalBytes: total > 0 ? total : nil
                    )))
                }
            }
        } catch {
            await sink.emit(.error(EngineError(
                kind: "deep_load_failed",
                message: "Could not load \(modelKind.displayName): \(error.localizedDescription)"
            )))
            await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                processed: 0, failed: 0,
                totalSeconds: Date().timeIntervalSince(started),
                modelKind: modelKey, cancelled: false
            )))
            return
        }

        // 2. Resolve targets.
        await sink.emit(.deepAnalyzeStarting(DeepAnalyzeStarting(
            modelKind: modelKey,
            phase: .resolvingTargets,
            message: "Finding files to analyze…"
        )))
        let targets: [(id: Int64, path: String)]
        do {
            targets = try await resolveTargets(database: database,
                                                scope: scope, modelKey: modelKey)
        } catch {
            await sink.emit(.error(EngineError(
                kind: "deep_targets_failed",
                message: "Could not resolve targets: \(error.localizedDescription)"
            )))
            return
        }
        let total = targets.count
        guard total > 0 else {
            await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                processed: 0, failed: 0,
                totalSeconds: Date().timeIntervalSince(started),
                modelKind: modelKey, cancelled: false
            )))
            return
        }

        JSONLog.shared.info(ev: "deep_analyze_start",
                            extra: ["model": AnyCodable(modelKey),
                                    "total": AnyCodable(total)])

        // 3. Iterate. Serial — VLM is GPU-bound.
        var processed = 0
        var failed    = 0
        var cancelled = false
        let batchStart = Date()

        for (i, target) in targets.enumerated() {
            if await DeepAnalyze.shared.isCancelled() {
                cancelled = true
                break
            }
            // Emit "starting this file" progress so the UI can show what's
            // being analyzed right now.
            let elapsed = Date().timeIntervalSince(batchStart)
            let perFile = i > 0 ? elapsed / Double(i) : modelKind.secondsPerImage
            let remaining = max(0, total - i)
            let eta = perFile * Double(remaining)
            await sink.emit(.deepAnalyzeProgress(DeepAnalyzeProgress(
                processed: i, total: total, etaSeconds: eta,
                currentPath: target.path, modelKind: modelKey
            )))

            // V14.9-L1: per-token live caption accumulator. MLX yields chunks
            // as the model generates; throttle wire emission to 4 Hz so a
            // fast token stream doesn't flood the IPC sink. Mirror of the
            // Windows engine accumulator in main.rs::append_caption_chunk.
            let captionState = CaptionStreamState()
            let sinkRef = sink
            let modelKeyRef = modelKey
            let onToken: @Sendable (String) async -> Void = { chunk in
                let snapshot = await captionState.append(chunk)
                guard await captionState.shouldEmit() else { return }
                await sinkRef.emit(.deepAnalyzeProgress(DeepAnalyzeProgress(
                    processed: i, total: total, etaSeconds: nil,
                    currentPath: target.path, modelKind: modelKeyRef,
                    currentCaption: snapshot
                )))
            }

            // Pull face cluster names (if any) to inject into the prompt.
            let faceNames = (try? await fetchFaceNames(database: database, fileID: target.id)) ?? []
            let url = URL(fileURLWithPath: target.path)
            let result = await DeepAnalyze.shared.analyze(imageURL: url, faceNames: faceNames, onToken: onToken)
            let isFailure = result.description.hasPrefix("Inference failed")
                || result.description.hasPrefix("Could not decode")
                || result.description == "Model not loaded."
            if isFailure {
                failed += 1
            } else {
                processed += 1
                _ = try? await persist(database: database,
                                        fileID: target.id,
                                        description: result.description,
                                        proposedName: result.proposedName,
                                        modelKey: modelKey)
                await sink.emit(.deepAnalyzeFileDone(DeepAnalyzeFileDone(
                    fileID: target.id,
                    description: result.description,
                    proposedName: result.proposedName,
                    modelKind: modelKey
                )))
            }
        }

        let dur = Date().timeIntervalSince(started)
        JSONLog.shared.info(ev: "deep_analyze_done",
                            extra: ["processed": AnyCodable(processed),
                                    "failed": AnyCodable(failed),
                                    "cancelled": AnyCodable(cancelled),
                                    "seconds": AnyCodable(dur)])
        await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
            processed: processed, failed: failed,
            totalSeconds: dur, modelKind: modelKey, cancelled: cancelled
        )))
    }

    private static func persist(
        database: Database,
        fileID: Int64,
        description: String,
        proposedName: String?,
        modelKey: String
    ) async throws {
        try await database.pool.write { db in
            try db.execute(sql: """
                UPDATE files
                SET vlm_description = ?,
                    vlm_proposed_name = ?,
                    vlm_model = ?,
                    vlm_analyzed_at = ?
                WHERE id = ?
                """, arguments: [
                    description,
                    proposedName,
                    modelKey,
                    Date().timeIntervalSince1970,
                    fileID
                ])
        }
    }

    /// Run face clustering inline if any face_prints lack an assignment.
    /// Cheap COUNT first — most repeat Deep Analyze runs will see zero.
    /// Format the structured naming columns into the [Title] [First name]
    /// reference Deep Analyze prompts use. Falls back to first name only,
    /// or to the legacy single-field `name`. Skips clusters flagged as
    /// `is_unknown` — those are explicitly opted out by the user.
    private static func fetchFaceNames(database: Database, fileID: Int64) async throws -> [String] {
        try await database.pool.read { db in
            let rows = try GRDB.Row.fetchAll(db, sql: """
                SELECT DISTINCT
                  persons.title, persons.first_name, persons.name
                FROM persons
                INNER JOIN face_prints ON face_prints.person_id = persons.id
                WHERE face_prints.file_id = ?
                  AND IFNULL(persons.is_unknown, 0) = 0
                """, arguments: [fileID])
            var names: [String] = []
            for r in rows {
                let title: String? = r["title"]
                let first: String? = r["first_name"]
                let legacy: String? = r["name"]
                let formatted = formatPersonRef(title: title, first: first, legacy: legacy)
                if !formatted.isEmpty { names.append(formatted) }
            }
            return names
        }
    }

    private static func formatPersonRef(title: String?, first: String?, legacy: String?) -> String {
        let t = title?.trimmingCharacters(in: .whitespaces) ?? ""
        let f = first?.trimmingCharacters(in: .whitespaces) ?? ""
        if !t.isEmpty && !f.isEmpty { return "\(t) \(f)" }
        if !f.isEmpty { return f }
        if !t.isEmpty { return t }
        return (legacy ?? "").trimmingCharacters(in: .whitespaces)
    }
}

/// V14.9-L1: actor-isolated state for the per-token caption accumulator
/// used by `DeepAnalyzeRunner`. Mirrors the Windows engine's
/// `append_caption_chunk` semantics: trim each chunk, join with exactly
/// one space, and throttle wire emission to 4 Hz so a fast MLX stream
/// doesn't flood the IPC sink. Actor-isolated so the @Sendable callback
/// passed into `DeepAnalyze.shared.analyze` is concurrency-safe.
actor CaptionStreamState {
    private var buffer: String = ""
    private var lastEmit: Date = .distantPast

    /// Append a chunk to the buffer with single-space normalization,
    /// return the post-append snapshot for any caller that wants to emit.
    func append(_ chunk: String) -> String {
        let trimmed = chunk.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return buffer }
        if !buffer.isEmpty && !buffer.hasSuffix(" ") {
            buffer.append(" ")
        }
        buffer.append(trimmed)
        return buffer
    }

    /// Throttle gate — returns true at most every 250 ms.
    func shouldEmit() -> Bool {
        let now = Date()
        if now.timeIntervalSince(lastEmit) >= 0.25 {
            lastEmit = now
            return true
        }
        return false
    }
}
