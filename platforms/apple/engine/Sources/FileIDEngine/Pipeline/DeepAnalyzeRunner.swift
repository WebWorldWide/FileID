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
    /// `excludedFolders` — absolute folder paths to skip (path-segment-boundary
    /// matching via `DeepAnalyzeRunner.exclusionWhereClause`; empty means none).
    /// Never applied to `.selected` — an explicit selection is deliberate and
    /// is never silently filtered (schema `deepAnalyzeAll.excludedFolders`).
    case wholeLibrary(skipExisting: Bool, excludedFolders: [String])
    case selected(fileIDs: [Int64], skipExisting: Bool)
}

public enum DeepAnalyzeRunner {

    /// Resolve scope → ordered list of (id, path) pairs. Targets images,
    /// videos, and PDFs — for videos we extract a keyframe; for PDFs we
    /// render the first page; for images we feed the file directly. The
    /// VLM captions all three. WholeLibrary skips files with a successful
    /// full pass by the requested model when `skipExisting=true`.
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
                    WHERE id = ? AND kind IN ('image', 'pdf', 'video', 'doc', 'audio', 'model') AND failed = 0
                    """, arguments: [id])
                if let r,
                   let rowID: Int64 = r["id"], rowID > 0,
                   let path: String = r["path_text"], !path.isEmpty {
                    return [Target(id: rowID, path: path)]
                }
                return []
            case .folder(let prefix):
                let p = prefix.hasSuffix("/") ? prefix : prefix + "/"
                // F-C3-027: escape LIKE metacharacters in the folder prefix so a
                // folder named with `_` (any char) or `%` (any run) can't
                // over-match a sibling subtree and pull unrelated files into the
                // VLM pass. ESCAPE '\' pairs with escapeLike's backslashing.
                let r = try GRDB.Row.fetchAll(db, sql: """
                    SELECT id, path_text FROM files
                    WHERE kind IN ('image', 'pdf', 'video', 'doc', 'audio', 'model') AND failed = 0
                      AND (path_text = ? OR path_text LIKE ? ESCAPE '\\')
                    ORDER BY scanned_at ASC
                    """, arguments: [prefix, Self.escapeLike(p) + "%"])
                return r.compactMap { row -> Target? in
                    guard let rowID: Int64 = row["id"], rowID > 0,
                          let path: String = row["path_text"], !path.isEmpty else { return nil }
                    return Target(id: rowID, path: path)
                }
            case .selected(let fileIDs, let skipExisting):
                var seen = Set<Int64>()
                let ids = fileIDs.filter { $0 > 0 && seen.insert($0).inserted }
                guard !ids.isEmpty else { return [] }
                let placeholders = ids.map { _ in "?" }.joined(separator: ",")
                let r = try GRDB.Row.fetchAll(db, sql: """
                    SELECT id, path_text, vlm_full_model FROM files
                    WHERE id IN (\(placeholders))
                      AND kind IN ('image', 'pdf', 'video', 'doc', 'audio', 'model')
                      AND failed = 0
                    """, arguments: StatementArguments(ids))
                let targets = r.compactMap { row -> Target? in
                    guard let rowID: Int64 = row["id"], rowID > 0,
                          let path: String = row["path_text"], !path.isEmpty else { return nil }
                    let fullModel: String? = row["vlm_full_model"]
                    if skipExisting && fullModel == modelKey { return nil }
                    return Target(id: rowID, path: path)
                }
                let byID = Dictionary(uniqueKeysWithValues: targets.map { ($0.id, $0) })
                return ids.compactMap { byID[$0] }
            case .wholeLibrary(let skipExisting, let excludedFolders):
                let (exclusionSQL, exclusionParams) = Self.exclusionWhereClause(excludedFolders)
                let sql: String
                let args: StatementArguments
                if skipExisting {
                    sql = """
                        SELECT id, path_text FROM files
                        WHERE kind IN ('image', 'pdf', 'video', 'doc', 'audio', 'model') AND failed = 0
                          AND (vlm_full_model IS NULL OR vlm_full_model != ?)\(exclusionSQL)
                        ORDER BY scanned_at ASC
                        """
                    args = StatementArguments([modelKey] + exclusionParams)
                } else {
                    sql = """
                        SELECT id, path_text FROM files
                        WHERE kind IN ('image', 'pdf', 'video', 'doc', 'audio', 'model') AND failed = 0\(exclusionSQL)
                        ORDER BY scanned_at ASC
                        """
                    args = StatementArguments(exclusionParams)
                }
                let r = try GRDB.Row.fetchAll(db, sql: sql, arguments: args)
                return r.compactMap { row -> Target? in
                    guard let rowID: Int64 = row["id"], rowID > 0,
                          let path: String = row["path_text"], !path.isEmpty else { return nil }
                    return Target(id: rowID, path: path)
                }
            }
        }
        return rows.map { ($0.id, $0.path) }
    }

    /// Backslash-escape SQL LIKE metacharacters so a literal folder prefix
    /// can't be widened by `_`/`%` in the path. Pairs with `ESCAPE '\'`.
    static func escapeLike(_ s: String) -> String {
        var out = s.replacingOccurrences(of: "\\", with: "\\\\")
        out = out.replacingOccurrences(of: "%", with: "\\%")
        out = out.replacingOccurrences(of: "_", with: "\\_")
        return out
    }

    /// Build a `" AND NOT (path_text LIKE ? ESCAPE '\')"` SQL fragment (empty
    /// string when `excludedFolders` is empty) plus its bound params, for
    /// `deepAnalyzeAll`'s whole-library scope. Reuses the same escaped-LIKE-
    /// prefix technique as the `.folder(prefix:)` scope above (`escapeLike` +
    /// a trailing `/` before the wildcard) so excluding "/Users/x/Photos"
    /// does NOT also exclude "/Users/x/PhotosBackup" — a bare `LIKE prefix%`
    /// would get that wrong. Mirrors the Rust engine's `exclusion_where_clause`
    /// range-scan (same boundary guarantee, GRDB-idiomatic technique). No
    /// scan-root containment check: unlike a scan exclusion, a Deep Analyze
    /// exclusion applies library-wide (any absolute folder is a valid
    /// target). Relative paths are silently ignored — the schema requires
    /// absolute folder paths.
    static func exclusionWhereClause(_ excludedFolders: [String]) -> (sql: String, params: [String]) {
        var sql = ""
        var params: [String] = []
        for raw in excludedFolders {
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed.hasPrefix("/") else { continue }
            var folder = trimmed
            while folder.count > 1, folder.hasSuffix("/") {
                folder.removeLast()
            }
            guard !folder.isEmpty else { continue }
            sql += " AND NOT (path_text LIKE ? ESCAPE '\\')"
            params.append(Self.escapeLike(folder + "/") + "%")
        }
        return (sql, params)
    }

    /// Run the batch. Streams progress via `sink`. Holds a SleepGuard
    /// for the duration so the system stays awake (lid-closed friendly).
    public static func run(
        database: Database,
        sink: IPCSink,
        scope: DeepAnalyzeScope,
        modelKind: AIModelKind,
        tagsOnly: Bool = false,
        proposeRenames: Bool = true
    ) async {
        let started = Date()
        let modelKey = modelKind.rawValue
        let completesFullPass = !tagsOnly && proposeRenames

        // No inline face clustering — it's a separate job. When
        // clusters are present captions use real names; otherwise
        // "person" stands in (graceful degrade).

        SleepGuard.shared.begin(reason: "Deep Analyze (\(modelKind.displayName))")
        defer { SleepGuard.shared.end() }

        // Single funnel for every terminal exit: emit exactly one
        // deepAnalyzeComplete and reset the cancel flag. Routing all exits here
        // guarantees no run() path strands the UI (F-C3-028) and that a cancel
        // consumed by THIS run is cleared for the next job — replacing the old
        // run-start clearCancel() that erased a cancel issued while queued
        // (F-C3-025).
        func finish(processed: Int, failed: Int, cancelled: Bool) async {
            await DeepAnalyze.shared.clearCancel()
            await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                processed: processed, failed: failed,
                totalSeconds: Date().timeIntervalSince(started),
                modelKind: modelKey, cancelled: cancelled
            )))
        }

        // F-C3-025: honor a cancel issued while this job sat in the JobQueue.
        // (The old code clearCancel()'d here, erasing that cancel and running
        // the whole pass anyway.) Checked before the RAM probe + cold load so a
        // job the user already cancelled does no work at all.
        if await DeepAnalyze.shared.isCancelled() {
            JSONLog.shared.info(ev: "deep_analyze_cancelled_while_queued",
                                extra: ["model": AnyCodable(modelKey)])
            await finish(processed: 0, failed: 0, cancelled: true)
            return
        }

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
            await finish(processed: 0, failed: 0, cancelled: false)
            return
        }
        let total = targets.count
        guard total > 0 else {
            await finish(processed: 0, failed: 0, cancelled: false)
            return
        }

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
            await finish(processed: 0, failed: 0, cancelled: false)
            return
        }

        // 2. Load the model (download if needed, with progress events).
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
            // F-C3-024: a cancel during the cold load/download cancels the load
            // task; surface that as a user cancel, not an alarming load failure.
            // (await is hoisted out of the `||` — its RHS is a non-async autoclosure.)
            let cancelledDuringLoad = await DeepAnalyze.shared.isCancelled()
            if error is CancellationError || cancelledDuringLoad {
                JSONLog.shared.info(ev: "deep_analyze_cancelled_during_load",
                                    extra: ["model": AnyCodable(modelKey)])
                await finish(processed: 0, failed: 0, cancelled: true)
                return
            }
            if case StreamingDownloadError.checksumMismatch = error {
                await sink.emit(.error(EngineError(
                    kind: "model_integrity_failed",
                    message: "Could not load \(modelKind.displayName): \(error.localizedDescription)",
                    modelKind: modelKey
                )))
            } else {
                await sink.emit(.error(EngineError(
                    kind: "deep_load_failed",
                    message: "Could not load \(modelKind.displayName): \(error.localizedDescription)"
                )))
            }
            await finish(processed: 0, failed: 0, cancelled: false)
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

            let url = URL(fileURLWithPath: target.path)
            // Audio is named from EMBEDDED metadata / on-device transcription (no VLM).
            // 3D models render via the OS QuickLook 3D generator → the VLM (visual
            // understanding), falling back to their embedded object/material names if the
            // render or inference fails. Everything else (image/video/pdf) takes the VLM
            // path. Mirrors the Windows engine (analyze_metadata_named_file + the .obj
            // software rasterizer in rasterize_for_vlm).
            let kind = FileTypes.kind(forExtension: (target.path as NSString).pathExtension)
            var result: DeepAnalyze.AnalysisResult
            if kind == .audio {
                result = await DeepAnalyze.shared.runCancellableAnalysis {
                    await DeepAnalyzeNaming.metadataResult(url: url, kind: kind)
                }
            } else {
                // Pull face cluster names (if any) to inject into the prompt.
                let faceNames = (try? await fetchFaceNames(database: database, fileID: target.id)) ?? []
                result = await DeepAnalyze.shared.runCancellableAnalysis {
                    await DeepAnalyze.shared.analyze(
                        imageURL: url,
                        faceNames: faceNames,
                        onToken: onToken
                    )
                }
                // A 3D model the OS couldn't render (no QuickLook generator) or the VLM
                // couldn't caption → its embedded-name metadata, so it still gets a name.
                if kind == .model, Self.isAnalysisFailure(result.description) {
                    result = await DeepAnalyze.shared.runCancellableAnalysis {
                        await DeepAnalyzeNaming.metadataResult(url: url, kind: kind)
                    }
                }
            }
            if await DeepAnalyze.shared.isCancelled() {
                cancelled = true
                break
            }
            let isFailure = Self.isAnalysisFailure(result.description)
            if isFailure {
                failed += 1
            } else {
                // Success is contingent on the result actually persisting. A
                // swallowed write used to report the file as done while the
                // caption/name never reached the DB (the UI then shows nothing
                // for a file it believes was analyzed).
                do {
                    try await persist(database: database,
                                      fileID: target.id,
                                      description: tagsOnly ? nil : result.description,
                                      proposedName: proposeRenames && !tagsOnly ? result.proposedName : nil,
                                      tags: result.tags,
                                      modelKey: modelKey,
                                      updatesDescription: !tagsOnly,
                                      updatesProposedName: proposeRenames && !tagsOnly,
                                      completesFullPass: completesFullPass)
                    processed += 1
                    await sink.emit(.deepAnalyzeFileDone(DeepAnalyzeFileDone(
                        fileID: target.id,
                        description: result.description,
                        proposedName: proposeRenames && !tagsOnly ? result.proposedName : nil,
                        modelKind: modelKey
                    )))
                } catch {
                    failed += 1
                    JSONLog.shared.warn(ev: "deep_analyze_persist_failed",
                                        error: "\(error)")
                }
            }
        }

        let dur = Date().timeIntervalSince(started)
        JSONLog.shared.info(ev: "deep_analyze_done",
                            extra: ["processed": AnyCodable(processed),
                                    "failed": AnyCodable(failed),
                                    "cancelled": AnyCodable(cancelled),
                                    "seconds": AnyCodable(dur)])
        await finish(processed: processed, failed: failed, cancelled: cancelled)
    }

    /// A `DeepAnalyze.analyze` description that signals the file wasn't usefully analyzed
    /// (no decodable raster, no loaded model, or an inference error) — not a real caption.
    static func isAnalysisFailure(_ description: String) -> Bool {
        description.hasPrefix("Inference failed")
            || description.hasPrefix("Could not decode")
            || description == "Model not loaded."
    }

    static func persist(
        database: Database,
        fileID: Int64,
        description: String?,
        proposedName: String?,
        tags: [String] = [],
        modelKey: String,
        updatesDescription: Bool = true,
        updatesProposedName: Bool = true,
        completesFullPass: Bool = true
    ) async throws {
        let safeDesc = (description?.isEmpty == true) ? nil : description
        let safeName = (proposedName?.isEmpty == true) ? nil : proposedName
        let cleanTags = tags
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        try await database.pool.write { db in
            if completesFullPass {
                try db.execute(sql: """
                    UPDATE files
                    SET vlm_description = CASE
                            WHEN ? THEN ? ELSE vlm_description END,
                        vlm_proposed_name = CASE
                            WHEN ? THEN ? ELSE vlm_proposed_name END,
                        vlm_model = ?,
                        vlm_full_model = ?,
                        vlm_analyzed_at = ?
                    WHERE id = ?
                    """, arguments: [
                        updatesDescription,
                        safeDesc,
                        updatesProposedName,
                        safeName,
                        modelKey,
                        modelKey,
                        Date().timeIntervalSince1970,
                        fileID
                    ])
            } else {
                try db.execute(sql: """
                    UPDATE files
                    SET vlm_description = CASE
                            WHEN ? THEN ? ELSE vlm_description END,
                        vlm_proposed_name = CASE
                            WHEN ? THEN ? ELSE vlm_proposed_name END,
                        vlm_model = CASE
                            WHEN vlm_full_model = ? THEN vlm_model ELSE NULL END,
                        vlm_full_model = CASE
                            WHEN vlm_full_model = ? THEN vlm_full_model ELSE NULL END,
                        vlm_analyzed_at = CASE
                            WHEN vlm_full_model = ? THEN vlm_analyzed_at ELSE NULL END
                    WHERE id = ?
                    """, arguments: [
                        updatesDescription,
                        safeDesc,
                        updatesProposedName,
                        safeName,
                        modelKey,
                        modelKey,
                        modelKey,
                        fileID
                    ])
            }
            try db.execute(sql: "DELETE FROM tags WHERE file_id = ? AND source = 'vlm'",
                           arguments: [fileID])
            for tag in cleanTags {
                try db.execute(sql: """
                    INSERT OR IGNORE INTO tags (file_id, tag, source, score) VALUES (?, ?, 'vlm', NULL)
                    """, arguments: [fileID, tag])
            }
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
