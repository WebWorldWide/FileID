// FileIDEngine — child process spawned by the FileID SwiftUI app.
// Reads IPC commands from stdin (newline-delimited JSON), emits events
// over the fd-2 pipe in the same format (dup'ed aside at startup by
// IPCTransport.bootstrap; raw fd 2 is repointed at engine-stderr.log so
// MLX/Metal diagnostics can't corrupt the event stream). Owns the
// SQLite database, scan pipeline, ANE/GPU model loading, and all
// on-device inference (CLIP, SFace, MLX VLMs).
//
// Lifetime: bound to the parent. Pipe close → LineReader EOF → clean
// exit. A getppid() poll catches force-quits where the pipe lingers.
import Foundation
import Darwin
import FileIDShared
import AsyncAlgorithms
import GRDB

@main
struct FileIDEngineMain {
    static func main() async {
        // U4: must run before ANY library can write to fd 2 (and before
        // the IPCSink singleton captures its wire handle).
        IPCTransport.bootstrap()

        // Ignore SIGPIPE — writes to a closed parent pipe shouldn't crash
        // the engine; the LineReader will detect the closed pipe on the next
        // read and we'll exit cleanly through the normal command loop.
        signal(SIGPIPE, SIG_IGN)

        // Parent-death watchdog. Belt-and-suspenders complement to stdin-EOF
        // detection: when the SwiftUI app force-quits, stdin sometimes stays
        // open long enough to leave the engine running indefinitely (orphaned
        // and reparented to launchd). Polling getppid() every 5s catches that
        // — when ppid flips to 1, the parent is gone and we exit.
        Task.detached(priority: .background) {
            while true {
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                if getppid() == 1 {
                    JSONLog.shared.info(ev: "parent_died_exiting")
                    JSONLog.shared.flush()
                    Darwin._exit(0)
                }
            }
        }

        let coordinator = ScanCoordinator()
        let sink = IPCSink.shared
        await JobQueue.shared.attachSink(sink)

        // Open the database ONCE for the engine's lifetime. GRDB explicitly
        // forbids more than one DatabasePool to the same file — opening a
        // pool per scan triggers SQLITE_BUSY when a prior pool is still alive
        // (which was the "database is locked" symptom from spam-clicking
        // Start). We open here, hand the same Database to every runScan.
        let database: Database?
        do {
            database = try Database(at: Database.defaultURL)
        } catch let error as DatabaseOpenError {
            // Migration identifiers, not user paths — safe to log raw.
            await sink.emit(.error(EngineError(
                kind: "db_newer_than_engine",
                message: "\(error). Update FileID, or wipe the library to rescan."
            )))
            JSONLog.shared.error(ev: "db_newer_than_engine", error: "\(error)")
            database = nil
        } catch {
            await sink.emit(.error(EngineError(
                kind: "db_open_failed",
                message: "Could not open database at \(Database.defaultURL.path): \(error)"
            )))
            JSONLog.shared.error(ev: "db_open_failed", error: "\(error)")
            database = nil
        }

        // Crash recovery: any scan_sessions row left in 'running' state means
        // a prior engine run died mid-scan (kill -9, OOM, panic). Mark them
        // 'crashed' with a count of how many files made it before the crash.
        // M5+ work: actually offer to resume from `last_file_index`. For now
        // we just surface the recovery cleanly so the user knows what happened.
        if let database {
            await detectCrashedSessions(database: database)
        }

        // Engine ready handshake. App waits for this before sending the first
        // command, so it knows the pipe is live and the engine started clean.
        await sink.emit(.ready(EngineInfo(
            version: "0.1.1",
            pid: ProcessInfo.processInfo.processIdentifier,
            workerCap: Hardware.workerCap,
            physicalMemoryGB: Hardware.physicalMemoryGB
        )))
        JSONLog.shared.info(ev: "engine_ready",
                            extra: ["pid": AnyCodable(ProcessInfo.processInfo.processIdentifier),
                                    "workers": AnyCodable(Hardware.workerCap)])

        // Capability check: Deep Analyze needs mlx.metallib next to the
        // engine binary (run.sh copies it in from .build/cache). Without
        // it, MLX would crash deep in GPU kernel load with an opaque
        // error during the first VLM inference. Surface this immediately
        // so the UI can disable Deep Analyze with a clear message instead
        // of letting the user wait for the crash.
        if !DeepAnalyzeCapability.metallibPresent() {
            JSONLog.shared.warn(ev: "engine_capability_warning",
                                error: "mlx.metallib missing — Deep Analyze unavailable")
            await sink.emit(.error(EngineError(
                kind: "deep_analyze_unavailable",
                message: "Deep Analyze isn't available on this build because mlx.metallib wasn't compiled. Run ./run.sh — it will fail with install instructions if cmake or the Metal Toolchain are missing."
            )))
        }

        // Periodic progress emitter — 1 Hz until the program exits.
        let progressTicker = Task.detached(priority: .background) {
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                if let snap = await coordinator.snapshot() {
                    await sink.emit(.progress(snap))
                }
            }
        }

        // Per-line decode errors get logged and skipped — a single
        // rogue line shouldn't kill the engine when a slightly-newer
        // app sends an IPCCommand case the engine doesn't know yet.
        let stdin = FileHandle.standardInput
        let commands = LineReader.readResults(from: stdin, as: IPCCommand.self)
        // The label is load-bearing: an unlabeled `break` here exits the
        // SWITCH, not the loop — the engine then sat in the read loop
        // until stdin EOF and never honored the shutdown command on its
        // own (caught by ScanCancellationTests the first time CI actually
        // executed it).
        commandLoop: for await item in commands {
            switch item {
            case .success(let cmd):
                await dispatch(cmd, coordinator: coordinator, sink: sink, database: database)
                if case .shutdown = cmd.payload { break commandLoop }
            case .failure(let err):
                JSONLog.shared.warn(ev: "command_decode_failed", error: "\(err)")
                await sink.emit(.error(EngineError(kind: "command_decode_failed",
                                                    message: "\(err)")))
            }
        }

        await coordinator.awaitActiveScan()
        // Also drain an in-flight restructure apply so a shutdown mid-apply lets
        // it cancel cleanly and flush its terminal restructureApplyResult. (F-C6-013)
        await coordinator.awaitActiveRestructure()
        progressTicker.cancel()

        // GRDB checkpoints on connection close (via atexit handlers we
        // skip). Force one before fast-exit so the WAL doesn't carry
        // late writes into the next launch.
        if let database {
            try? await database.pool.write { db in
                try db.execute(sql: "PRAGMA wal_checkpoint(TRUNCATE)")
            }
        }

        JSONLog.shared.info(ev: "engine_exit")
        JSONLog.shared.flush()

        // Deliver any IPC event still buffered (e.g. a terminal scanComplete /
        // restructureApplyResult the detached drainer hadn't flushed yet)
        // before the hard exit drops it. (F-C3-040)
        await sink.drainAndClose()

        // Hard-exit. MLX's static destructors SEGV during normal atexit
        // teardown on macOS 26; `_exit` skips them. Our work is already
        // flushed to disk above.
        Darwin._exit(0)
    }

    private static func requireLicenseAcceptance(
        for kind: AIModelKind,
        sink: IPCSink
    ) async -> Bool {
        guard ModelLicenseAcceptance.isAccepted(for: kind) else {
            await sink.emit(.error(EngineError(
                kind: "model_license_not_accepted",
                message: "Open FileID and accept the \(kind.licenseName) before downloading or using \(kind.displayName).",
                modelKind: kind.rawValue
            )))
            return false
        }
        return true
    }

    /// Per-command dispatcher. `startScan` runs the scan in a detached task so
    /// the command loop stays responsive to subsequent pause/cancel commands.
    static func dispatch(_ cmd: IPCCommand, coordinator: ScanCoordinator,
                          sink: IPCSink, database: Database?) async {
        switch cmd.payload {
        case .startScan(let rootPath, let rootDisplay, let rescan, let excludedPaths):
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot scan."
                )))
                // A bare error leaves the app's auto-pilot stuck on "Scanning…"
                // forever — it advances only on a scan-terminal event. Emit an
                // empty scanComplete so the assistant leaves the scanning state.
                // (F-C3-032)
                await sink.emit(.scanComplete(ScanComplete(
                    sessionID: "", totalFiles: 0, processedFiles: 0,
                    failedFiles: 0, totalSeconds: 0
                )))
                return
            }
            // Reject a start-scan while one is already queued or running. Mirrors
            // the Windows engine's scan_session.rs guard. Prevents unbounded job
            // queue growth from a misbehaving or rapidly-clicking app client.
            if await JobQueue.shared.hasActive(category: .scan) {
                await sink.emit(.error(EngineError(
                    kind: "scan_already_queued",
                    message: "A scan is already running or queued — cancel it first."
                )))
                return
            }
            // The app resolves the security-scoped bookmark to a filesystem
            // path before sending, so the engine receives a ready-to-walk
            // path. `rootDisplay` defaults to `rootPath` when omitted.
            let displayPath = rootDisplay ?? rootPath
            // R4-11: allocate the scan epoch synchronously here (the command loop
            // is serial), BEFORE the next command is read, so a quick cancelScan is
            // attributed to this scan and survives startSession.
            let epoch = await coordinator.nextScanEpoch()
            // Enqueue — runs immediately if nothing else queued, else
            // waits for predecessors to finish.
            let title = "Scan \((displayPath as NSString).lastPathComponent)"
            await JobQueue.shared.enqueue(.init(
                category: .scan,
                title: title,
                etaSeconds: nil  // unknown until discovery completes
            ) {
                let task = Task.detached(priority: .userInitiated) {
                    await runScan(rootPath: rootPath, displayPath: displayPath,
                                  rescan: rescan ?? false, epoch: epoch,
                                  excludedPaths: excludedPaths,
                                  coordinator: coordinator, sink: sink,
                                  database: database)
                }
                await coordinator.setActiveScan(task)
                await task.value   // block the queued job until scan finishes
            })
        case .pauseScan:
            await coordinator.requestPause()
            JSONLog.shared.info(ev: "pause_requested")
        case .resumeScan:
            await coordinator.requestResume()
            JSONLog.shared.info(ev: "resume_requested")
        case .cancelScan:
            await coordinator.requestCancel()
            JSONLog.shared.info(ev: "cancel_requested")
        case .requestStatus:
            if let snap = await coordinator.snapshot() {
                await sink.emit(.progress(snap))
            }
        case .shutdown:
            JSONLog.shared.info(ev: "shutdown_requested")
            // Cancel any active scan so paused/in-flight workers exit
            // deterministically. Without this, a scan paused at shutdown
            // leaves its workers spinning in the pause-poll loop forever and
            // `awaitActiveScan()` (in main) blocks the clean exit indefinitely.
            // requestShutdown (not requestCancel) also trips the dedicated
            // shutdown mirror so a clustering pass started after a cancelled scan
            // aborts promptly instead of running to its persist first. (R-07)
            await coordinator.requestShutdown()
        case .runFaceClustering:
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot cluster faces."
                )))
                return
            }
            // Duplicate-command parity with the Windows engine
            // (face_clustering_busy bounce) — same shape as the
            // deep-analyze gate below.
            if await JobQueue.shared.hasActive(category: .faceCluster) {
                await sink.emit(.error(EngineError(
                    kind: "face_clustering_busy",
                    message: "Face clustering is already running."
                )))
                return
            }
            await JobQueue.shared.enqueue(.init(
                category: .faceCluster,
                title: "Cluster faces",
                etaSeconds: nil
            ) {
                JSONLog.shared.info(ev: "face_cluster_requested")
                SleepGuard.shared.begin(reason: "Face clustering")
                let summary = await FaceClustering.runClustering(database: database, sink: sink)
                SleepGuard.shared.end()
                await sink.emit(.faceClusteringComplete(summary))
            })
        case .deepAnalyzeFile(let fileID, let modelKind):
            guard let database, let kind = AIModelKind(rawValue: modelKind) else {
                await sink.emit(.error(EngineError(
                    kind: "deep_invalid",
                    message: "Database unavailable or unknown model kind \(modelKind)."
                )))
                return
            }
            guard await requireLicenseAcceptance(for: kind, sink: sink) else { return }
            // Duplicate-command parity with the Windows engine: a second
            // deep-analyze while one is queued/running is rejected, not
            // silently queued (the app disables its buttons in-flight, so
            // this only fires for misbehaving callers).
            if await JobQueue.shared.hasActive(category: .deepAnalyze) {
                await sink.emit(.error(EngineError(
                    kind: "deep_analyze_already_running",
                    message: "A Deep Analyze pass is already running — wait for it to finish or cancel it first."
                )))
                return
            }
            // Immediate "received" signal — the UI's startingCard listens
            // for this so the user sees acknowledgement the moment they
            // click. Without it, there's a multi-second silent gap while
            // the runner waits its turn in JobQueue + cold-loads the VLM.
            await sink.emit(.deepAnalyzeStarting(DeepAnalyzeStarting(
                modelKind: modelKind, phase: .queued, message: "Queued"
            )))
            await JobQueue.shared.enqueue(.init(
                category: .deepAnalyze,
                title: "Deep Analyze 1 file (\(kind.displayName))",
                etaSeconds: kind.secondsPerImage + 10  // +10 for model load if cold
            ) {
                await DeepAnalyzeRunner.run(database: database, sink: sink,
                                             scope: .singleFile(fileID),
                                             modelKind: kind)
            })
        case .deepAnalyzeFolder(let prefix, let modelKind):
            guard let database, let kind = AIModelKind(rawValue: modelKind) else {
                await sink.emit(.error(EngineError(
                    kind: "deep_invalid",
                    message: "Database unavailable or unknown model kind \(modelKind)."
                )))
                return
            }
            guard await requireLicenseAcceptance(for: kind, sink: sink) else { return }
            if await JobQueue.shared.hasActive(category: .deepAnalyze) {
                await sink.emit(.error(EngineError(
                    kind: "deep_analyze_already_running",
                    message: "A Deep Analyze pass is already running — wait for it to finish or cancel it first."
                )))
                return
            }
            await sink.emit(.deepAnalyzeStarting(DeepAnalyzeStarting(
                modelKind: modelKind, phase: .queued, message: "Queued"
            )))
            await JobQueue.shared.enqueue(.init(
                category: .deepAnalyze,
                title: "Deep Analyze folder (\(kind.displayName))",
                etaSeconds: nil
            ) {
                await DeepAnalyzeRunner.run(database: database, sink: sink,
                                             scope: .folder(prefix: prefix),
                                             modelKind: kind)
            })
        case .deepAnalyzeAll(let modelKind, let skipExisting, let tagsOnly, let proposeRenames):
            guard let database, let kind = AIModelKind(rawValue: modelKind) else {
                await sink.emit(.error(EngineError(
                    kind: "deep_invalid",
                    message: "Database unavailable or unknown model kind \(modelKind)."
                )))
                return
            }
            guard await requireLicenseAcceptance(for: kind, sink: sink) else { return }
            if await JobQueue.shared.hasActive(category: .deepAnalyze) {
                await sink.emit(.error(EngineError(
                    kind: "deep_analyze_already_running",
                    message: "A Deep Analyze pass is already running — wait for it to finish or cancel it first."
                )))
                return
            }
            await sink.emit(.deepAnalyzeStarting(DeepAnalyzeStarting(
                modelKind: modelKind, phase: .queued, message: "Queued"
            )))
            await JobQueue.shared.enqueue(.init(
                category: .deepAnalyze,
                title: "Deep Analyze entire library (\(kind.displayName))",
                etaSeconds: nil
            ) {
                await DeepAnalyzeRunner.run(database: database, sink: sink,
                                             scope: .wholeLibrary(skipExisting: skipExisting),
                                             modelKind: kind,
                                             tagsOnly: tagsOnly ?? false,
                                             proposeRenames: proposeRenames ?? true)
            })
        case .deepAnalyzeCancel:
            await DeepAnalyze.shared.requestCancel()
            JSONLog.shared.info(ev: "deep_analyze_cancel_requested")
        case .prewarmModel(let modelKey):
            guard let kind = AIModelKind(rawValue: modelKey) else {
                // Canonical cross-platform kind for "engine doesn't recognize
                // this model id" — matches the Windows engine's `unknown_model`
                // (was macOS `prewarm_invalid_kind`). Stamp `modelKind` so the
                // app can route it to the right install slot. (audit F-C2-003)
                await sink.emit(.error(EngineError(
                    kind: "unknown_model",
                    message: "This FileID engine doesn't recognize the model '\(modelKey)'. Reinstall or rebuild FileID so the app and engine match, then try again.",
                    modelKind: modelKey
                )))
                return
            }
            guard await requireLicenseAcceptance(for: kind, sink: sink) else { return }
            // Prewarm runs OUTSIDE JobQueue. JobQueue serializes
            // user-facing pipeline jobs (scan, cluster, analyze) that
            // touch the database; a multi-GB model download has no
            // such conflict and would otherwise block Start Scan
            // behind a download that takes hours.
            JSONLog.shared.info(ev: "prewarm_model_started",
                                extra: ["kind": AnyCodable(kind.rawValue)])
            let work = Task.detached(priority: .userInitiated) {
                SleepGuard.shared.begin(reason: "Model prewarm")
                defer { SleepGuard.shared.end() }
                do {
                    try await DeepAnalyze.shared.ensureLoaded(kind: kind) { frac, msg, done, total in
                        Task {
                            await sink.emit(.modelDownloadProgress(ModelDownloadProgress(
                                modelKind: kind.rawValue, fraction: frac, message: msg,
                                bytesDone: done > 0 ? done : nil,
                                totalBytes: total > 0 ? total : nil
                            )))
                        }
                    }
                    await DeepAnalyze.shared.markInstalledSentinel(kind: kind)
                    await sink.emit(.modelDownloadProgress(ModelDownloadProgress(
                        modelKind: kind.rawValue, fraction: 1.0,
                        message: "\(kind.displayName) ready."
                    )))
                    JSONLog.shared.info(ev: "prewarm_model_done",
                                        extra: ["kind": AnyCodable(kind.rawValue)])
                } catch is CancellationError {
                    await sink.emit(.error(EngineError(
                        kind: "prewarm_cancelled",
                        message: "Prewarm \(kind.displayName) cancelled."
                    )))
                    JSONLog.shared.info(ev: "prewarm_model_cancelled",
                                        extra: ["kind": AnyCodable(kind.rawValue)])
                } catch {
                    let isIntegrity: Bool
                    if case StreamingDownloadError.checksumMismatch = error {
                        isIntegrity = true
                    } else {
                        isIntegrity = false
                    }
                    await sink.emit(.error(EngineError(
                        kind: isIntegrity ? "model_integrity_failed" : "prewarm_failed",
                        message: "Prewarm \(kind.displayName) failed: \(error.localizedDescription)",
                        modelKind: isIntegrity ? kind.rawValue : nil
                    )))
                }
                await DeepAnalyze.shared.setPrewarmTask(nil)
            }
            await DeepAnalyze.shared.setPrewarmTask(work)
        case .cancelPrewarm:
            await DeepAnalyze.shared.cancelPrewarm()
            JSONLog.shared.info(ev: "prewarm_cancel_requested")

        // ── Restructure butler (Windows-parity, wired) ───────────
        // Port of commands/restructure.rs: compute the plan / apply the
        // moves through the engine's Restructure butler and emit the
        // corresponding reply event. Run detached so the command loop stays
        // responsive to pause/cancel/shutdown during a long plan computation.
        case .planRestructure(let libraryRoot, let supportsPagedPlans):
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot plan a restructure."
                )))
                return
            }
            guard let restructureToken = await coordinator.reserveRestructure() else {
                await sink.emit(.error(EngineError(
                    kind: "restructure_busy",
                    message: "Another restructure operation is already running.")))
                return
            }
            let planRoot = URL(fileURLWithPath: libraryRoot)
            let planTask = Task.detached(priority: .userInitiated) {
                SleepGuard.shared.begin(reason: "Restructure planning")
                defer { SleepGuard.shared.end() }
                JSONLog.shared.info(ev: "plan_restructure_requested",
                                    path: redactPathForLog(libraryRoot))
                do {
                    let plan: RestructurePlan
                    if supportsPagedPlans == true,
                       let largePlan = try await Restructure.proposeLargeStoredIfNeeded(
                           database: database, libraryRoot: planRoot) {
                        plan = largePlan
                    } else {
                        let planResult = try await Restructure.proposeAll(
                            database: database, libraryRoot: planRoot)
                        var legacyPlan = Self.restructurePlan(
                            from: planResult, libraryRoot: libraryRoot)
                        if supportsPagedPlans == true,
                           legacyPlan.moves.count > Restructure.storedPlanPreviewCap {
                            let stored = try Restructure.storePlan(
                                libraryRoot: libraryRoot, moves: legacyPlan.moves)
                            legacyPlan = RestructurePlan(
                                libraryRoot: legacyPlan.libraryRoot,
                                moves: stored.preview,
                                categoryCounts: legacyPlan.categoryCounts,
                                folderClassifications: legacyPlan.folderClassifications,
                                planID: stored.planID,
                                totalMoves: legacyPlan.moves.count,
                                truncated: true)
                        }
                        plan = legacyPlan
                    }
                    await sink.emit(.restructurePlan(plan))
                    JSONLog.shared.info(ev: "plan_restructure_done",
                                        extra: ["moves": AnyCodable(
                                            plan.totalMoves ?? plan.moves.count)])
                } catch {
                    // Terminal error so the Restructure tab's "Computing plan…"
                    // status recovers instead of awaiting forever (mirrors
                    // restructure.rs's plan_restructure_failed JoinError arm).
                    JSONLog.shared.warn(ev: "plan_restructure_failed", error: "\(error)")
                    await sink.emit(.error(EngineError(
                        kind: "plan_restructure_failed",
                        message: "Restructure planning did not complete: \(error)"
                    )))
                }
                await coordinator.finishRestructure(token: restructureToken)
            }
            await coordinator.attachRestructure(planTask, token: restructureToken)
        case .applyRestructure(let libraryRoot, let moves, _, let planID):
            // macOS performs real filesystem moves; the Windows engine's
            // symlink-preview mode has no macOS equivalent, so `useSymlinks`
            // is accepted for wire parity and ignored.
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot apply a restructure."
                )))
                return
            }
            let applyRoot = URL(fileURLWithPath: libraryRoot)
            let proposals = moves.map { m in
                RestructureProposal(
                    fileID: m.fileID, oldPath: m.source, newPath: m.destination,
                    bucket: m.category, confidence: m.confidence, reason: m.reason)
            }
            guard let restructureToken = await coordinator.reserveRestructure() else {
                await sink.emit(.error(EngineError(
                    kind: "restructure_busy",
                    message: "Another restructure apply or undo is already running.")))
                return
            }
            let applyTask = Task.detached(priority: .userInitiated) {
                SleepGuard.shared.begin(reason: "Restructure apply")
                defer { SleepGuard.shared.end() }
                JSONLog.shared.info(ev: "apply_restructure_requested",
                                    extra: ["moves": AnyCodable(proposals.count),
                                            "storedPlan": AnyCodable(planID != nil)])
                do {
                    let result: Restructure.ApplyResult
                    if let planID {
                        guard moves.isEmpty else {
                            throw CocoaError(.fileReadCorruptFile)
                        }
                        result = try await Restructure.applyStoredPlan(
                            planID: planID, expectedRoot: libraryRoot,
                            database: database, libraryRoot: applyRoot)
                    } else {
                        result = try await Restructure.apply(
                            proposals: proposals, database: database, libraryRoot: applyRoot)
                    }
                    await sink.emit(.restructureApplyResult(RestructureApplyResult(
                        applied: result.moved, failed: result.failed, privilegeError: nil)))
                } catch {
                    if let journalError = error as? Restructure.UndoJournalError {
                        let result = journalError.result
                        await sink.emit(.restructureApplyResult(RestructureApplyResult(
                            applied: result.moved, failed: result.failed, privilegeError: nil)))
                        await sink.emit(.error(EngineError(
                            kind: "restructure_undo_journal",
                            message: journalError.localizedDescription)))
                    } else {
                        await sink.emit(.error(EngineError(
                            kind: "apply_restructure",
                            message: "Apply failed: \(error.localizedDescription)")))
                    }
                }
                await coordinator.finishRestructure(token: restructureToken)
            }
            await coordinator.attachRestructure(applyTask, token: restructureToken)

        case .undoRestructure(let libraryRoot):
            // Reverse the last apply by replaying the engine's on-disk undo
            // journal. Same machinery as apply (real moves, cancellable, terminal
            // restructureApplyResult), so register it the same way. (R2)
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot undo a restructure."
                )))
                return
            }
            guard let restructureToken = await coordinator.reserveRestructure() else {
                await sink.emit(.error(EngineError(
                    kind: "restructure_busy",
                    message: "Another restructure apply or undo is already running.")))
                return
            }
            let undoRoot = URL(fileURLWithPath: libraryRoot)
            let undoTask = Task.detached(priority: .userInitiated) {
                SleepGuard.shared.begin(reason: "Restructure undo")
                defer { SleepGuard.shared.end() }
                JSONLog.shared.info(ev: "undo_restructure_requested")
                let result = await Restructure.undoLast(database: database, libraryRoot: undoRoot)
                await sink.emit(.restructureApplyResult(RestructureApplyResult(
                    applied: result.moved, failed: result.failed, privilegeError: nil)))
                await coordinator.finishRestructure(token: restructureToken)
            }
            await coordinator.attachRestructure(undoTask, token: restructureToken)

        case .purgeExcluded(let excludedPaths):
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot purge excluded folders."
                )))
                return
            }
            let normalized = normalizeExcludedPaths(excludedPaths)
            let deleted = await purgeExcludedRows(database: database, excludedPaths: normalized,
                                                 sink: sink, sessionID: nil)
            if deleted >= 0 {
                await sink.emit(.bulkActionResult(BulkActionResult(
                    action: "purgeExcluded",
                    succeeded: deleted,
                    failed: 0,
                    messages: []
                )))
            }

        // ── Cross-platform bulk actions ──────────────────────────
        case .applyTags(let fileIDs, let tags, let mode):
            guard let database else { await emitDbUnavailable(sink, action: "applyTags"); return }
            await sink.emit(.bulkActionResult(await applyTags(database: database, fileIDs: fileIDs, tags: tags, mode: mode)))
        case .renameFiles(let renames):
            guard let database else { await emitDbUnavailable(sink, action: "renameFiles"); return }
            await sink.emit(.bulkActionResult(await renameFiles(database: database, renames: renames)))
        case .trashFiles(let fileIDs, let exactIdentities):
            guard exactIdentities == nil else {
                await sink.emit(.bulkActionResult(BulkActionResult(
                    action: "trashFiles", succeeded: 0, failed: fileIDs.count,
                    messages: fileIDs.map {
                        item($0, ok: false, "Exact Trash evidence is not accepted by the macOS engine; use the native Cleanup flow.")
                    })))
                return
            }
            guard let database else { await emitDbUnavailable(sink, action: "trashFiles"); return }
            await sink.emit(.bulkActionResult(await trashFiles(database: database, fileIDs: fileIDs)))
        case .mergeClusters(let sourcePersonID, let destinationPersonID):
            guard let database else { await emitDbUnavailable(sink, action: "mergeClusters"); return }
            await sink.emit(.bulkActionResult(await mergeClusters(database: database, source: sourcePersonID, destination: destinationPersonID)))
        case .renamePerson(let personID, let title, let firstName, let middleName, let lastName, let suffix):
            guard let database else { await emitDbUnavailable(sink, action: "renamePerson"); return }
            await sink.emit(.bulkActionResult(await renamePerson(database: database, personID: personID, title: title, firstName: firstName, middleName: middleName, lastName: lastName, suffix: suffix)))
        case .markPersonsAsUnknown(let personIDs):
            guard let database else { await emitDbUnavailable(sink, action: "markPersonsAsUnknown"); return }
            await sink.emit(.bulkActionResult(await markPersonsAsUnknown(database: database, personIDs: personIDs)))
        case .wipeLibrary:
            guard let database else { await sink.emit(.libraryWiped(LibraryWiped(ok: false, message: "Database unavailable."))); return }
            await sink.emit(.libraryWiped(await wipeLibrary(database: database)))
        case .findMergeSuggestions,
             .embedTextQuery,
             .embedImageQuery,
             .restoreFromTrash,
             .revertMerge,
             .markPersonsDifferent,
             .generateVideoThumbnail:
            await sink.emit(.error(EngineError(
                kind: "not_implemented_yet",
                message: "This IPC command is not implemented by the macOS engine yet."
            )))
        case .verifyCudaPack:
            await sink.emit(.error(EngineError(
                kind: "not_applicable_on_platform",
                message: "CUDA isn't available on Apple Silicon — the AppleProvider EP selects ANE/Metal automatically."
            )))
        }
    }

    static func emitDbUnavailable(_ sink: IPCSink, action: String) async {
        await sink.emit(.bulkActionResult(BulkActionResult(
            action: action,
            succeeded: 0,
            failed: 0,
            messages: [BulkActionItem(ok: false, message: "Database unavailable.")]
        )))
    }

    static func item(_ id: Int64? = nil, ok: Bool, _ message: String? = nil) -> BulkActionItem {
        BulkActionItem(fileID: id, ok: ok, message: message)
    }

    static func applyTags(database: Database, fileIDs: [Int64], tags: [String], mode: String) async -> BulkActionResult {
        let clean = Array(Set(tags.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty })).sorted()
        guard !fileIDs.isEmpty, !clean.isEmpty else {
            return BulkActionResult(action: "applyTags", succeeded: 0, failed: fileIDs.count, messages: [item(nil, ok: false, "No files or tags supplied.")])
        }
        do {
            let lower = mode.lowercased()
            let messages = try await database.pool.write { db -> [BulkActionItem] in
                var messages: [BulkActionItem] = []
                for id in fileIDs {
                    do {
                        if lower == "replace" {
                            try db.execute(sql: "DELETE FROM tags WHERE file_id = ? AND source = 'user'", arguments: [id])
                        }
                        for tag in clean {
                            if lower == "remove" {
                                try db.execute(sql: "DELETE FROM tags WHERE file_id = ? AND tag = ? AND source = 'user'", arguments: [id, tag])
                            } else {
                                try db.execute(sql: "INSERT OR IGNORE INTO tags(file_id, tag, source, score) VALUES (?, ?, 'user', NULL)", arguments: [id, tag])
                            }
                        }
                        messages.append(item(id, ok: true))
                    } catch {
                        messages.append(item(id, ok: false, error.localizedDescription))
                    }
                }
                return messages
            }
            return BulkActionResult(action: "applyTags", succeeded: messages.filter(\.ok).count, failed: messages.filter { !$0.ok }.count, messages: messages)
        } catch {
            return BulkActionResult(action: "applyTags", succeeded: 0, failed: fileIDs.count, messages: [item(nil, ok: false, error.localizedDescription)])
        }
    }

    static let bulkMutationChunkSize = 500

    struct BulkFileState: Sendable {
        let path: String
        let fileRef: Int64?
    }

    static func fetchBulkStates(
        database: Database, fileIDs: [Int64]
    ) async throws -> [Int64: BulkFileState] {
        let ids = Array(Set(fileIDs))
        guard !ids.isEmpty else { return [:] }
        return try await database.pool.read { db in
            var result: [Int64: BulkFileState] = [:]
            for start in stride(from: 0, to: ids.count, by: bulkMutationChunkSize) {
                let chunk = ids[start..<min(start + bulkMutationChunkSize, ids.count)]
                let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                let rows = try Row.fetchAll(
                    db,
                    sql: "SELECT id, path_text, file_ref FROM files WHERE id IN (\(placeholders))",
                    arguments: StatementArguments(chunk))
                for row in rows {
                    let id: Int64 = row["id"]
                    result[id] = BulkFileState(path: row["path_text"], fileRef: row["file_ref"])
                }
            }
            return result
        }
    }

    static func renameFiles(database: Database, renames: [RenameEntry]) async -> BulkActionResult {
        SleepGuard.shared.begin(reason: "Bulk rename")
        defer { SleepGuard.shared.end() }
        var messages: [BulkActionItem] = []
        var states: [Int64: BulkFileState]
        do {
            states = try await fetchBulkStates(database: database,
                                               fileIDs: renames.map(\.fileID))
        } catch {
            return BulkActionResult(
                action: "renameFiles", succeeded: 0, failed: renames.count,
                messages: renames.map { item($0.fileID, ok: false, error.localizedDescription) })
        }
        for rename in renames {
            let trimmed = rename.newName.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty, !trimmed.contains("/"), !trimmed.contains("\0") else {
                messages.append(item(rename.fileID, ok: false, "Invalid filename."))
                continue
            }
            guard let state = states[rename.fileID] else {
                messages.append(item(rename.fileID, ok: false, "File row not found."))
                continue
            }
            let oldURL = URL(fileURLWithPath: state.path)
            guard !Restructure.fileRefSwapped(
                dbRef: state.fileRef, currentRef: Discovery.inode(of: oldURL)) else {
                messages.append(item(rename.fileID, ok: false, "File changed since it was indexed."))
                continue
            }
            let newURL = oldURL.deletingLastPathComponent().appendingPathComponent(trimmed)
            guard !FileManager.default.fileExists(atPath: newURL.path) else {
                messages.append(item(rename.fileID, ok: false, "Destination already exists."))
                continue
            }
            do {
                try FileManager.default.moveItem(at: oldURL, to: newURL)
                let ext = newURL.pathExtension.lowercased()
                try await database.pool.write { db in
                    try db.execute(
                        sql: "UPDATE OR ABORT files SET path_text = ?, path_hash = ?, path_search = ?, extension = ? WHERE id = ? AND path_text = ?",
                        arguments: [newURL.path, StablePathHash.hash(newURL.path),
                                    newURL.path.precomposedStringWithCanonicalMapping,
                                    ext.isEmpty ? nil : ext, rename.fileID, state.path])
                    guard db.changesCount == 1 else {
                        throw CocoaError(.fileWriteUnknown)
                    }
                }
                states[rename.fileID] = BulkFileState(
                    path: newURL.path, fileRef: state.fileRef)
                messages.append(item(rename.fileID, ok: true))
            } catch {
                messages.append(item(rename.fileID, ok: false, error.localizedDescription))
            }
        }
        return BulkActionResult(action: "renameFiles", succeeded: messages.filter(\.ok).count, failed: messages.filter { !$0.ok }.count, messages: messages)
    }

    static func orderedUniqueFileIDs(_ fileIDs: [Int64]) -> [Int64] {
        var seen = Set<Int64>()
        return fileIDs.filter { seen.insert($0).inserted }
    }

    static func trashFiles(database: Database, fileIDs: [Int64]) async -> BulkActionResult {
        SleepGuard.shared.begin(reason: "Bulk trash")
        defer { SleepGuard.shared.end() }
        let uniqueIDs = orderedUniqueFileIDs(fileIDs)
        var messages: [BulkActionItem] = []
        let states: [Int64: BulkFileState]
        do {
            states = try await fetchBulkStates(database: database, fileIDs: uniqueIDs)
        } catch {
            return BulkActionResult(
                action: "trashFiles", succeeded: 0, failed: uniqueIDs.count,
                messages: uniqueIDs.map { item($0, ok: false, error.localizedDescription) })
        }
        for id in uniqueIDs {
            guard let state = states[id] else {
                messages.append(item(id, ok: false, "File row not found."))
                continue
            }
            let url = URL(fileURLWithPath: state.path)
            guard !Restructure.fileRefSwapped(
                dbRef: state.fileRef, currentRef: Discovery.inode(of: url)) else {
                messages.append(item(id, ok: false, "File changed since it was indexed."))
                continue
            }
            do {
                try FileManager.default.trashItem(at: url, resultingItemURL: nil)
                try await database.pool.write { db in
                    try db.execute(sql: "DELETE FROM files WHERE id = ?", arguments: [id])
                    guard db.changesCount == 1 else {
                        throw CocoaError(.fileWriteUnknown)
                    }
                }
                messages.append(item(id, ok: true))
            } catch {
                messages.append(item(id, ok: false, error.localizedDescription))
            }
        }
        return BulkActionResult(action: "trashFiles", succeeded: messages.filter(\.ok).count, failed: messages.filter { !$0.ok }.count, messages: messages)
    }

    static func mergeClusters(database: Database, source: Int64, destination: Int64) async -> BulkActionResult {
        if source == destination {
            return BulkActionResult(action: "mergeClusters", succeeded: 1, failed: 0, messages: [item(source, ok: true, "No-op self merge.")])
        }
        do {
            _ = try await database.mergePersons(target: destination, sources: [source])
            return BulkActionResult(action: "mergeClusters", succeeded: 1, failed: 0, messages: [item(source, ok: true)])
        } catch {
            return BulkActionResult(action: "mergeClusters", succeeded: 0, failed: 1, messages: [item(source, ok: false, error.localizedDescription)])
        }
    }

    static func renamePerson(database: Database, personID: Int64, title: String?, firstName: String?, middleName: String?, lastName: String?, suffix: String?) async -> BulkActionResult {
        let clean: @Sendable (String?) -> String? = { value in
            let t = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            return t.isEmpty ? nil : t
        }
        let parts = [clean(title), clean(firstName), clean(middleName), clean(lastName), clean(suffix)].compactMap { $0 }
        let display = parts.joined(separator: " ")
        do {
            try await database.pool.write { db in
                try db.execute(sql: """
                    UPDATE persons
                    SET title = ?, first_name = ?, middle_name = ?, last_name = ?, suffix = ?, name = ?, is_unknown = 0
                    WHERE id = ?
                    """, arguments: [clean(title), clean(firstName), clean(middleName), clean(lastName), clean(suffix), display.isEmpty ? nil : display, personID])
            }
            return BulkActionResult(action: "renamePerson", succeeded: 1, failed: 0, messages: [item(personID, ok: true)])
        } catch {
            return BulkActionResult(action: "renamePerson", succeeded: 0, failed: 1, messages: [item(personID, ok: false, error.localizedDescription)])
        }
    }

    static func markPersonsAsUnknown(database: Database, personIDs: [Int64]) async -> BulkActionResult {
        do {
            let messages = try await database.pool.write { db -> [BulkActionItem] in
                var messages: [BulkActionItem] = []
                for id in personIDs {
                    do {
                        try db.execute(sql: """
                            UPDATE persons
                            SET name = NULL, title = NULL, first_name = NULL, middle_name = NULL,
                                last_name = NULL, suffix = NULL, is_unknown = 1
                            WHERE id = ?
                            """, arguments: [id])
                        messages.append(item(id, ok: true))
                    } catch {
                        messages.append(item(id, ok: false, error.localizedDescription))
                    }
                }
                return messages
            }
            return BulkActionResult(action: "markPersonsAsUnknown", succeeded: messages.filter(\.ok).count, failed: messages.filter { !$0.ok }.count, messages: messages)
        } catch {
            return BulkActionResult(action: "markPersonsAsUnknown", succeeded: 0, failed: personIDs.count, messages: [item(nil, ok: false, error.localizedDescription)])
        }
    }

    static func wipeLibrary(database: Database) async -> LibraryWiped {
        do {
            try await database.pool.write { db in
                let virtuals = try String.fetchAll(db, sql: "SELECT name FROM sqlite_master WHERE type='table' AND sql LIKE 'CREATE VIRTUAL TABLE%'")
                let tables = try String.fetchAll(db, sql: """
                    SELECT name FROM sqlite_master
                    WHERE type='table' AND sql LIKE 'CREATE TABLE%'
                      AND name NOT LIKE 'sqlite_%' AND name <> 'grdb_migrations'
                    """).filter { table in
                        !virtuals.contains { v in table.hasPrefix(v + "_") }
                    }
                try db.execute(sql: "PRAGMA foreign_keys = OFF")
                for table in tables {
                    try db.execute(sql: "DELETE FROM \"\(table.replacingOccurrences(of: "\"", with: "\"\""))\"")
                }
                for table in virtuals {
                    let q = table.replacingOccurrences(of: "\"", with: "\"\"")
                    try db.execute(sql: "INSERT INTO \"\(q)\"(\"\(q)\") VALUES('delete-all')")
                }
                try? db.execute(sql: "DELETE FROM sqlite_sequence")
                try db.execute(sql: "PRAGMA foreign_keys = ON")
            }
            return LibraryWiped(ok: true)
        } catch {
            return LibraryWiped(ok: false, message: error.localizedDescription)
        }
    }

    /// Run discovery against an already-resolved filesystem path. The app
    /// resolves its security-scoped bookmark to a path and starts accessing
    /// the scoped resource before sending `startScan`, so the engine just
    /// walks the path it's given.
    /// `database` is the engine's single shared `Database` (one DatabasePool
    /// per engine process — opening more would trigger SQLITE_BUSY).
    static func runScan(
        rootPath: String, displayPath: String, rescan: Bool, epoch: Int,
        excludedPaths: [String]?, coordinator: ScanCoordinator, sink: IPCSink,
        database: Database
    ) async {
        let url = URL(fileURLWithPath: rootPath)
        // The path arrives already resolved from the app side. Re-establish
        // security-scoped access in case the app handed off scope (no-op /
        // false outside a sandbox, which is fine for CLI dev runs + tests).
        let hasScope = url.startAccessingSecurityScopedResource()
        defer { if hasScope { url.stopAccessingSecurityScopedResource() } }
        if !hasScope {
            JSONLog.shared.info(ev: "no_security_scope", path: redactPathForLog(url.path),
                                extra: ["reason": AnyCodable("ok in unsandboxed contexts")])
        }

        // Hold a no-sleep assertion for the duration of the scan so the
        // system doesn't suspend mid-tag overnight. Released in defer.
        SleepGuard.shared.begin(reason: "Scanning \(url.lastPathComponent)")
        defer { SleepGuard.shared.end() }

        let effectiveExcludedPaths = Discovery.resolvedExclusionPaths(
            root: url, rawPaths: excludedPaths)
        if let excludedPaths, !excludedPaths.isEmpty {
            JSONLog.shared.info(
                ev: "start_scan_exclusions",
                path: redactPathForLog(url.path),
                extra: ["requested": AnyCodable(excludedPaths.count),
                        "effective": AnyCodable(effectiveExcludedPaths.count)])
        }

        let session = await coordinator.startSession(rootDisplayPath: url.lastPathComponent, epoch: epoch)
        await sink.emit(.phaseChanged(.discovering))
        JSONLog.shared.info(ev: "scan_started", sess: session.id, path: redactPathForLog(url.path))

        // Database is the engine-shared instance (opened once at engine
        // startup). Create a row in scan_sessions so a crash mid-scan can be
        // recovered by reading status='running' on next startup.
        do {
            try await database.pool.write { db in
                try db.execute(sql: """
                    INSERT INTO scan_sessions (id, root_path, started_at, status)
                    VALUES (?, ?, ?, 'running')
                    """, arguments: [session.id, url.path, Date().timeIntervalSince1970])
            }
        } catch {
            JSONLog.shared.warn(ev: "scan_session_insert_failed", sess: session.id, error: "\(error)")
        }

        // R4-11: a cancelScan that landed in the start window is preserved by the
        // epoch check in startSession; honor it before spending the discovery walk.
        // markSessionFinal emits the cancelled terminal phase via the shielded
        // writer so the app's UI returns to idle.
        if await coordinator.isCancelled {
            JSONLog.shared.info(ev: "scan_cancelled_at_start", sess: session.id)
            await markSessionFinal(database: database, session: session,
                                   coordinator: coordinator, sink: sink,
                                   totalSeconds: 0)
            return
        }

        if !effectiveExcludedPaths.isEmpty {
            await purgeExcludedRows(database: database, excludedPaths: effectiveExcludedPaths,
                                    sink: sink, sessionID: session.id)
        }

        // Stage A — Discovery. Files are streamed directly into the tagging
        // workers below. The old `walk` path retained and sorted every
        // DiscoveredFile first, which duplicated all paths and metadata and
        // exhausted memory on million-file libraries. Directory enumeration is
        // already depth-first, preserving the useful same-folder I/O locality
        // without the global O(N) sort.
        let discovery = Discovery()
        let scanStart = Date()

        // Pre-warm both ANE-bound models on the main task before workers
        // start so all workers don't race the cold-start slow path
        // simultaneously. Each is a no-op if the model isn't installed.
        await Task.detached(priority: .userInitiated) {
            MobileCLIPService.shared.preWarm()
            // RAM++ primary tagger (macOS lockstep) — no-op if not installed, in
            // which case Tagging falls back to the Vision scene classifier.
            RamPlusService.shared.preWarm()
            // Pick whichever ArcFace variant the user has on disk —
            // iResNet50 takes precedence when both are present.
            for kind in FaceEmbedderKind.installedKinds() {
                ArcFaceService.shared.preWarm(kind)
                break
            }
        }.value

        // Unbuffered (rendezvous) async channels: each `send` suspends until a
        // consumer calls `next`. They are NOT bounded buffers — see the cancel
        // handling in ScanCoordinator.requestCancel() for why that matters.
        let discoveryChan = AsyncChannel<DiscoveredFile>()
        let taggedChan    = AsyncChannel<TaggedFile>()
        let workerCap     = Hardware.workerCap
        let pool          = VisionWorkerPool(count: workerCap)
        // `rescan: true` forces a full reprocess even of size+mtime-unchanged
        // files, mirroring the Windows engine's empty skip set.
        let dbWriter      = DBWriter(db: database, sink: sink,
                                     coordinator: coordinator, sessionID: session.id,
                                     forceReprocess: rescan)

        // DBWriter task — runs in parallel with tagging. Drains taggedChan
        // until it finishes, then exits.
        let writerTask = Task.detached(priority: .userInitiated) {
            await dbWriter.drain(taggedChan)
        }

        // Producer + N workers in one TaskGroup so we know when all workers
        // finish (and can then close taggedChan to signal EOF to writer).
        await withTaskGroup(of: Void.self) { group in
            // Producer: enumerate and feed each file immediately. AsyncChannel
            // is a rendezvous channel, so the producer cannot outrun the worker
            // pool and resident discovery state stays O(worker count). The
            // active scan task is cancelled by requestCancel(), which also
            // unblocks a producer suspended in send.
            group.addTask {
                defer { discoveryChan.finish() }
                var taggingStarted = false
                let discovered = await discovery.walkStreaming(
                    root: url,
                    database: database,
                    forceReprocess: rescan,
                    excludedPaths: effectiveExcludedPaths,
                    cancelCheck: { ScanCoordinator.isCancelledSync() },
                    progress: { count in
                        Task { await coordinator.bumpDiscovered(to: count) }
                    }
                ) { file in
                    if !Task.isCancelled && !ScanCoordinator.isCancelledSync() {
                        if !taggingStarted {
                            taggingStarted = true
                            await coordinator.beginTagging()
                            await sink.emit(.phaseChanged(.tagging))
                        }
                        await discoveryChan.send(file)
                    }
                }
                let discoveryDur = Date().timeIntervalSince(scanStart)
                await coordinator.bumpDiscovered(to: discovered)
                await coordinator.setTotal(discovered)
                JSONLog.shared.info(
                    ev: "discovery_complete", sess: session.id,
                    extra: [
                        "files": AnyCodable(discovered),
                        "seconds": AnyCodable(discoveryDur),
                        "ratePerSec": AnyCodable(
                            discoveryDur > 0 ? Double(discovered) / discoveryDur : 0)
                    ])
                await sink.emit(.discoveryComplete(totalFiles: discovered))
            }
            // Workers — N concurrent. Each pulls files until the channel
            // closes, processes via the Vision pool, pushes to tagged.
            // Honors cancel + pause via the sync mirrors on ScanCoordinator.
            for _ in 0..<workerCap {
                group.addTask {
                    for await disc in discoveryChan {
                        if ScanCoordinator.isCancelledSync() { break }
                        // Pause-poll: if paused, sleep in 200ms slices until
                        // unpaused or cancelled. Cheap when not paused (one
                        // sync mirror read per file).
                        while ScanCoordinator.isPausedSync() {
                            if ScanCoordinator.isCancelledSync() { break }
                            try? await Task.sleep(nanoseconds: 200_000_000)
                        }
                        if ScanCoordinator.isCancelledSync() { break }
                        // nil only when this task was cancelled while waiting for
                        // a Vision worker — stop pulling files in that case.
                        guard let tagged = await pool.with({ worker in
                            await Tagging.processFile(discovered: disc, worker: worker)
                        }) else { break }
                        await taggedChan.send(tagged)
                    }
                }
            }
        }
        // All workers done — signal writer that no more results are coming.
        taggedChan.finish()
        await writerTask.value

        // Stage D — post-scan orphan sweep + auto-enqueue face clustering.
        // Files the user deleted from Finder leave behind DB rows that show
        // up in Library as broken tiles. Walk the rows under THIS scan root,
        // stat each one, drop the misses. Capped at 5000 rows per sweep so
        // a giant library doesn't stall completion. Only runs when the scan
        // completed normally (a cancelled scan didn't visit every file, so
        // its rows could legitimately be "missing" only because they weren't
        // reached).
        //
        // After orphan sweep, queue face clustering automatically if any
        // bbox-only face_prints rows exist (i.e. the scan detected faces
        // but they haven't been clustered into Persons yet). This way the
        // user doesn't have to remember to click "Run Face Clustering" —
        // and Deep Analyze can use real names immediately ("Adam playing
        // basketball" instead of "child playing basketball").
        if await !coordinator.isCancelled {
            await coordinator.setPhase(.postScan)
            await sink.emit(.phaseChanged(.postScan))
            await sweepOrphans(database: database, scanRootPath: url.path,
                                scanStart: scanStart, sink: sink, sessionID: session.id)
            await autoEnqueueFaceClusteringIfNeeded(database: database, sink: sink)
        }

        let totalDur = Date().timeIntervalSince(scanStart)
        await markSessionFinal(database: database, session: session,
                                coordinator: coordinator, sink: sink,
                                totalSeconds: totalDur)
    }

    /// Post-scan orphan sweep: delete rows under `scanRootPath` whose file
    /// no longer exists on disk. Bounded at 5000 rows per scan so a large
    /// library can't stall the post-scan phase. The DB's ON DELETE CASCADE
    /// handles tags / ocr_text / face_prints / clip_embeddings.
    private static func sweepOrphans(
        database: Database,
        scanRootPath: String,
        scanStart: Date,
        sink: IPCSink,
        sessionID: String
    ) async {
        struct CandidateRow: Sendable { let id: Int64; let path: String }
        let prefix = scanRootPath.hasSuffix("/") ? scanRootPath : scanRootPath + "/"
        // Match descendants with a half-open prefix RANGE, not LIKE. A real
        // folder named e.g. "100%" or "a_b" contains LIKE wildcards, and an
        // unescaped `LIKE prefix||'%'` would then match — and DELETE — rows for
        // files OUTSIDE the scanned tree. The upper bound is the prefix with
        // its final scalar bumped by one (SQLite's default BINARY collation
        // compares byte-wise, so this captures exactly the prefix subtree).
        let prefixUpper: String = {
            var s = prefix
            guard let last = s.popLast(),
                  let next = UnicodeScalar(last.unicodeScalars.first!.value + 1) else {
                return prefix  // unreachable for a non-empty "…/" prefix
            }
            return s + String(next)
        }()
        let cap = 5000
        let candidates: [CandidateRow]
        do {
            candidates = try await database.pool.read { db in
                let rows = try GRDB.Row.fetchAll(db, sql: """
                    SELECT id, path_text FROM files
                    WHERE (path_text = ? OR (path_text >= ? AND path_text < ?))
                      AND scanned_at < ?
                    LIMIT \(cap)
                    """, arguments: [
                        scanRootPath,
                        prefix,
                        prefixUpper,
                        scanStart.timeIntervalSince1970
                    ])
                return rows.map { r in
                    CandidateRow(id: r["id"] ?? 0, path: r["path_text"] ?? "")
                }
            }
        } catch {
            JSONLog.shared.warn(ev: "orphan_sweep_query_failed", sess: sessionID, error: "\(error)")
            return
        }
        guard !candidates.isEmpty else { return }

        // Stat off the writer thread; FileManager hits are cheap but blocking.
        let missing: [Int64] = await Task.detached(priority: .background) {
            let fm = FileManager.default
            return candidates.compactMap { row in
                fm.fileExists(atPath: row.path) ? nil : row.id
            }
        }.value
        guard !missing.isEmpty else {
            JSONLog.shared.info(ev: "orphan_sweep", sess: sessionID,
                                extra: ["candidates": AnyCodable(candidates.count),
                                        "deleted": AnyCodable(0)])
            return
        }
        do {
            try await database.pool.write { db in
                let chunks = stride(from: 0, to: missing.count, by: 200).map {
                    Array(missing[$0..<min($0 + 200, missing.count)])
                }
                // Capture persons whose faces are about to be cascade-deleted,
                // so we can reconcile their counts/representative afterward.
                var affectedPersons = Set<Int64>()
                for chunk in chunks {
                    let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                    let pids = try Int64.fetchAll(db, sql: """
                        SELECT DISTINCT person_id FROM face_prints
                        WHERE person_id IS NOT NULL AND file_id IN (\(placeholders))
                        """, arguments: StatementArguments(chunk.map { Int($0) }))
                    affectedPersons.formUnion(pids)
                }
                // Chunk the IN clause to keep the SQL string + bound vars sane.
                for chunk in chunks {
                    let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                    try db.execute(
                        sql: "DELETE FROM files WHERE id IN (\(placeholders))",
                        arguments: StatementArguments(chunk.map { Int($0) })
                    )
                }
                // Reconcile persons: ON DELETE CASCADE removed face rows but
                // leaves persons.file_count stale and representative_face_id
                // dangling at a deleted face. Recompute both in this txn.
                try Self.reconcilePersons(affectedPersons, db: db)
            }
            JSONLog.shared.info(ev: "orphan_sweep", sess: sessionID,
                                extra: ["candidates": AnyCodable(candidates.count),
                                        "deleted": AnyCodable(missing.count),
                                        "capped": AnyCodable(candidates.count >= cap)])
        } catch {
            JSONLog.shared.warn(ev: "orphan_sweep_delete_failed", sess: sessionID,
                                error: "\(error)")
            await sink.emit(.error(EngineError(
                kind: "orphan_sweep_failed",
                message: "Could not delete \(missing.count) orphaned rows: \(error)"
            )))
        }
    }

    /// Delete already-cataloged rows under user-excluded folders before a scan
    /// starts. Files on disk are untouched; DB ON DELETE CASCADE removes tags,
    /// OCR/captions, embeddings, and face rows. Mirrors the Windows exclusion
    /// semantics and keeps Library from showing newly-excluded files.
    @discardableResult
    private static func purgeExcludedRows(
        database: Database,
        excludedPaths: [String],
        sink: IPCSink,
        sessionID: String?
    ) async -> Int {
        guard !excludedPaths.isEmpty else { return 0 }
        do {
            let deleted = try await database.pool.write { db -> Int in
                var ids = Set<Int64>()
                for excluded in excludedPaths {
                    let prefix = excluded.hasSuffix("/") ? excluded : excluded + "/"
                    let upper = prefixUpperBound(prefix)
                    // Match against path_search (the NFC form of path_text), not
                    // path_text itself. The excluded-path needle is NFC-normalized
                    // (normalizeExcludedPaths / Discovery.normalizedExclusionPath both
                    // apply precomposedStringWithCanonicalMapping), but macOS
                    // GUI-created accented folder names are commonly NFD on disk, so
                    // lower(path_text) would byte-mismatch the NFC needle and purge
                    // zero rows for any excluded folder with non-ASCII characters.
                    // path_search is the NFC column every writer maintains for exactly
                    // this normalization-insensitive matching (Database.swift v16).
                    let rows = try Int64.fetchAll(db, sql: """
                        SELECT id FROM files
                        WHERE lower(path_search) = ?
                           OR (lower(path_search) >= ? AND lower(path_search) < ?)
                        """, arguments: [excluded, prefix, upper])
                    ids.formUnion(rows)
                }
                guard !ids.isEmpty else { return 0 }

                let sortedIDs = ids.sorted()
                let chunks = stride(from: 0, to: sortedIDs.count, by: 200).map {
                    Array(sortedIDs[$0..<min($0 + 200, sortedIDs.count)])
                }
                var affectedPersons = Set<Int64>()
                for chunk in chunks {
                    let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                    let pids = try Int64.fetchAll(db, sql: """
                        SELECT DISTINCT person_id FROM face_prints
                        WHERE person_id IS NOT NULL AND file_id IN (\(placeholders))
                        """, arguments: StatementArguments(chunk.map { Int($0) }))
                    affectedPersons.formUnion(pids)
                }
                for chunk in chunks {
                    let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                    try db.execute(
                        sql: "DELETE FROM files WHERE id IN (\(placeholders))",
                        arguments: StatementArguments(chunk.map { Int($0) }))
                }
                try Self.reconcilePersons(affectedPersons, db: db)
                return sortedIDs.count
            }
            JSONLog.shared.info(ev: "purge_excluded_rows", sess: sessionID,
                                extra: ["deleted": AnyCodable(deleted),
                                        "excludedPaths": AnyCodable(excludedPaths.count)])
            return deleted
        } catch {
            JSONLog.shared.warn(ev: "purge_excluded_rows_failed", sess: sessionID,
                                error: "\(error)")
            await sink.emit(.error(EngineError(
                kind: "purge_excluded_failed",
                message: "Could not remove excluded folders from the library: \(error)"
            )))
            return -1
        }
    }

    private static func normalizeExcludedPaths(_ paths: [String]) -> [String] {
        var seen = Set<String>()
        var result: [String] = []
        for path in paths {
            let trimmed = path.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { continue }
            var normalized = URL(fileURLWithPath: trimmed)
                .resolvingSymlinksInPath()
                .standardizedFileURL
                .path
                .precomposedStringWithCanonicalMapping
                .lowercased()
            while normalized.count > 1, normalized.hasSuffix("/") {
                normalized.removeLast()
            }
            guard !seen.contains(normalized) else { continue }
            seen.insert(normalized)
            result.append(normalized)
        }
        return result
    }

    private static func prefixUpperBound(_ prefix: String) -> String {
        var s = prefix
        guard let last = s.popLast(),
              let next = UnicodeScalar(last.unicodeScalars.first!.value + 1) else {
            return prefix
        }
        return s + String(next)
    }

    /// Recompute persons.file_count and repair a dangling
    /// representative_face_id for the given persons, after their faces may
    /// have been removed (cascade delete). Must run inside the caller's write
    /// transaction.
    static func reconcilePersons(_ personIDs: Set<Int64>, db: GRDB.Database) throws {
        for pid in personIDs {
            try db.execute(sql: """
                UPDATE persons
                SET file_count = (SELECT COUNT(DISTINCT file_id)
                                  FROM face_prints WHERE person_id = ?)
                WHERE id = ?
                """, arguments: [pid, pid])
            // If the representative face was deleted (or never set), point it at
            // any surviving face for this person, else NULL.
            try db.execute(sql: """
                UPDATE persons
                SET representative_face_id =
                    (SELECT id FROM face_prints WHERE person_id = ? ORDER BY id LIMIT 1)
                WHERE id = ?
                  AND (representative_face_id IS NULL
                       OR representative_face_id NOT IN
                          (SELECT id FROM face_prints WHERE person_id = ?))
                """, arguments: [pid, pid, pid])
        }
    }

    /// At engine startup, find any scan_sessions left in 'running' status
    /// (= prior engine run crashed mid-scan) and mark them 'crashed' with
    /// telemetry. Cursor is preserved so a future "resume from crash" feature
    /// can pick up where we left off.
    static func detectCrashedSessions(database: Database) async {
        struct CrashedRow: Sendable {
            let id: String; let rootPath: String; let lastFileIndex: Int?
        }
        do {
            let crashed: [CrashedRow] = try await database.pool.read { db in
                let rows = try GRDB.Row.fetchAll(db, sql: """
                    SELECT id, root_path, last_file_index
                    FROM scan_sessions
                    WHERE status = 'running'
                    """)
                return rows.map { r in
                    CrashedRow(
                        id: r["id"] ?? "?",
                        rootPath: r["root_path"] ?? "?",
                        lastFileIndex: r["last_file_index"]
                    )
                }
            }
            guard !crashed.isEmpty else { return }
            try await database.pool.write { db in
                try db.execute(sql: """
                    UPDATE scan_sessions
                    SET status = 'crashed', completed_at = ?
                    WHERE status = 'running'
                    """, arguments: [Date().timeIntervalSince1970])
            }
            for row in crashed {
                JSONLog.shared.warn(
                    ev: "crash_recovery_detected",
                    sess: row.id,
                    path: redactPathForLog(row.rootPath),
                    error: "Previous run died mid-scan; \(row.lastFileIndex ?? 0) files completed before the crash."
                )
            }
        } catch {
            JSONLog.shared.warn(ev: "crash_recovery_failed", error: "\(error)")
        }
    }

    /// Auto-enqueue a face-clustering job after a scan if there are
    /// face_prints rows that haven't been assigned to a person yet.
    /// Idempotent — re-running is harmless. Runs through the queue so
    /// it can't conflict with anything else mid-flight.
    private static func autoEnqueueFaceClusteringIfNeeded(
        database: Database, sink: IPCSink
    ) async {
        // Are there ANY unassigned face_prints rows? Cheap query.
        let needs: Int = (try? await database.pool.read { db in
            try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM face_prints WHERE person_id IS NULL
                """) ?? 0
        }) ?? 0
        guard needs > 0 else { return }
        JSONLog.shared.info(ev: "auto_face_cluster_enqueued",
                            extra: ["unassigned": AnyCodable(needs)])
        await JobQueue.shared.enqueue(.init(
            category: .faceCluster,
            title: "Cluster faces (auto)",
            etaSeconds: nil
        ) {
            SleepGuard.shared.begin(reason: "Face clustering (auto)")
            let summary = await FaceClustering.runClustering(database: database, sink: sink)
            SleepGuard.shared.end()
            await sink.emit(.faceClusteringComplete(summary))
        })
    }

    /// Build the IPC `RestructurePlan` DTO from engine proposals: map each
    /// proposal to a `RestructureMove` and roll up per-bucket category counts
    /// (descending by count, then category for a stable order). Per-move `tier`
    /// (Anchor/Mixed/Junk) and the rolled-up `folderClassifications` are derived
    /// from `Restructure.classifyFolders` so the app renders the
    /// engine-authoritative Tidy/Keep tiles instead of its local heuristic
    /// fallback (the "null on older engines" path). (F-C3-035 wiring)
    static func restructurePlan(
        from plan: Restructure.PlanResult, libraryRoot: String
    ) -> RestructurePlan {
        // The tier map + Keep/Tidy/Junk counts are engine-authoritative and already
        // computed by `proposeAll` on the FULL pre-strip set with the semantic-claim
        // exemption (F-C1-004) — recomputing them here on the stripped `proposals`
        // would undercount the "Keep" tile (the stripped anchor folders are gone) and
        // diverge from the Windows engine. This mapper just stamps each surviving
        // move's tier by its source-folder parent. (audit — lockstep)
        let moves = plan.proposals.map { p in
            let parent = (p.oldPath as NSString).deletingLastPathComponent
            return RestructureMove(
                fileID: p.fileID, source: p.oldPath, destination: p.newPath,
                category: p.bucket, tier: plan.tierByFolder[parent],
                confidence: p.confidence, reason: p.reason)
        }
        var counts: [String: Int] = [:]
        for p in plan.proposals { counts[p.bucket, default: 0] += 1 }
        let categoryCounts = counts
            .map { RestructureCategoryCount(category: $0.key, count: $0.value) }
            .sorted { $0.count != $1.count ? $0.count > $1.count : $0.category < $1.category }
        return RestructurePlan(
            libraryRoot: libraryRoot, moves: moves,
            categoryCounts: categoryCounts,
            folderClassifications: FolderClassificationCounts(
                anchorFolders: plan.anchorFolders, mixedFolders: plan.mixedFolders,
                junkFolders: plan.junkFolders))
    }

    /// Mark the session completed/cancelled in the DB + emit terminal events.
    private static func markSessionFinal(
        database: Database,
        session: ScanCoordinator.Session,
        coordinator: ScanCoordinator,
        sink: IPCSink,
        totalSeconds: Double
    ) async {
        let cancelled = await coordinator.isCancelled
        let finalPhase: ScanPhase = cancelled ? .cancelled : .completed
        let snap = await coordinator.snapshot()
        let processed = snap?.processed ?? 0
        let failed    = snap?.failed ?? 0
        let total     = snap?.total ?? 0
        // The common reason we reach markSessionFinal is a CANCELLED scan,
        // and the scan runs inside a task that requestCancel() cancelled — so a
        // plain pool.write here throws CancellationError and the terminal status
        // is never written (row stuck 'running' → false 'crashed' next launch).
        // Shield the terminal write from the cancellation. (F-C3-031)
        let status = cancelled ? "cancelled" : "completed"
        let completedAt = Date().timeIntervalSince1970
        let sessionID = session.id
        do {
            try await database.writeUncancellable { db in
                try db.execute(sql: """
                    UPDATE scan_sessions SET status = ?, completed_at = ?
                    WHERE id = ?
                    """, arguments: [status, completedAt, sessionID])
            }
        } catch {
            JSONLog.shared.warn(ev: "scan_session_update_failed",
                                sess: session.id, error: "\(error)")
        }
        await coordinator.setPhase(finalPhase)
        JSONLog.shared.info(ev: "scan_finished", sess: session.id,
                            extra: ["totalSeconds": AnyCodable(totalSeconds),
                                    "processed": AnyCodable(processed),
                                    "failed": AnyCodable(failed),
                                    "total": AnyCodable(total),
                                    "cancelled": AnyCodable(cancelled)])
        await sink.emit(.scanComplete(ScanComplete(
            sessionID: session.id,
            totalFiles: total,
            processedFiles: processed,
            failedFiles: failed,
            totalSeconds: totalSeconds
        )))
        JSONLog.shared.flush()
    }
}
