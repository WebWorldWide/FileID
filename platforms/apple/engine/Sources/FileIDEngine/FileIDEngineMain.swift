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
import CoreGraphics
import Darwin
import FileIDShared
import AsyncAlgorithms
import GRDB

@main
struct FileIDEngineMain {
    static let databaseOpenFailureMessage = "FileID could not open its local library database. Check available disk space and permissions, then relaunch."

    static func main() async {
        ORTPrivacy.disableTelemetryBeforeLoad()
        if Array(CommandLine.arguments.dropFirst()) == ["--model-catalog-status"] {
            let entries = MLXModelCatalog.entries
            let vision = entries.filter { $0.role == .vision }.count
            if let data = try? JSONSerialization.data(withJSONObject: ["catalogEntries":entries.count,"vision":vision,"text":entries.count - vision], options:[.sortedKeys]) {
                print(String(decoding: data, as: UTF8.self))
            }
            exit(entries.count >= 200 ? 0 : 1)
        }

        if CommandLine.arguments.dropFirst().first == "--sample-video" {
            exit(await VideoFrameWorker.run(arguments: Array(CommandLine.arguments.dropFirst(2))))
        }
        if CommandLine.arguments.dropFirst().first == "--sample-video-sequence" {
            exit(await VideoFrameWorker.runSequence(arguments: Array(CommandLine.arguments.dropFirst(2))))
        }
        if CommandLine.arguments.dropFirst().first == "--sample-video-pool" {
            exit(await VideoFramePool.run(arguments: Array(CommandLine.arguments.dropFirst(2))))
        }
        if CommandLine.arguments.dropFirst().first == "--scan-video-signals" {
            exit(await VideoFrameWorker.runSignals(arguments: Array(CommandLine.arguments.dropFirst(2))))
        }
        if CommandLine.arguments.dropFirst().first == "--convert-video" {
            exit(await VideoConversionWorker.run(arguments: Array(CommandLine.arguments.dropFirst(2))))
        }
        if CommandLine.arguments.dropFirst().first == "--export-photo" {
            exit(await VideoFrameWorker.runPhoto(arguments: Array(CommandLine.arguments.dropFirst(2))))
        }
        // Must run before ANY library can write to fd 2 (and before
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
        let databaseURL = ProcessInfo.processInfo.environment["FILEID_DATABASE_PATH"]
            .map { URL(fileURLWithPath: $0) } ?? Database.defaultURL
        let database: Database?
        do {
            database = try Database(at: databaseURL)
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
                message: databaseOpenFailureMessage
            )))
            let nsError = error as NSError
            JSONLog.shared.error(
                ev: "db_open_failed",
                path: redactPathForLog(databaseURL.path),
                error: "\(nsError.domain) \(nsError.code)"
            )
            database = nil
        }

        // Crash recovery: any scan_sessions row left in 'running' state means
        // a prior engine run died mid-scan (kill -9, OOM, panic). Mark them
        // 'crashed' with a count of how many files made it before the crash.
        // M5+ work: actually offer to resume from `last_file_index`. For now
        // we just surface the recovery cleanly so the user knows what happened.
        if let database {
            await detectCrashedSessions(database: database)
            await TimelineAnalysis.shared.recover(database: database)
            await MediaTools.shared.recover(database: database)
            await CatalogTakeGrouping.recover(database: database)
            await FaceRefreshJob.recover(database: database)
            try? await database.vectorIndex.recover()
            await TrashJournal.reconcile(database: database)
            await PathMoveJournal.reconcile(database: database)
        }

        // Engine ready handshake. App waits for this before sending the first
        // command, so it knows the pipe is live and the engine started clean.
        await sink.emit(.ready(EngineInfo(
            version: "0.1.3",
            pid: ProcessInfo.processInfo.processIdentifier,
            workerCap: Hardware.workerCap,
            physicalMemoryGB: Hardware.physicalMemoryGB
        )))
        JSONLog.shared.info(ev: "engine_ready",
                            extra: ["pid": AnyCodable(ProcessInfo.processInfo.processIdentifier),
                                    "workers": AnyCodable(Hardware.workerCap)])
        AccurateOCRWarmup.shared.startWhenIdle()
        QueryCommands.prewarmTextEncoder()

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
                if let snap = await coordinator.periodicSnapshot() {
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
        // it cancel cleanly and flush its terminal restructureApplyResult.
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
        // before the hard exit drops it.
        await sink.drainAndClose()

        // Hard-exit. MLX's static destructors SEGV during normal atexit
        // teardown on macOS 26; `_exit` skips them. Our work is already
        // flushed to disk above.
        Darwin._exit(0)
    }

    private static func requireLicenseAcceptance(
        for kind: AIModelKind,
        sink: IPCSink,
        emitDeepAnalyzeTerminal: Bool = false
    ) async -> Bool {
        guard ModelLicenseAcceptance.isAccepted(for: kind) else {
            await sink.emit(.error(EngineError(
                kind: "model_license_not_accepted",
                message: "Open FileID and accept the \(kind.licenseName) before downloading or using \(kind.displayName).",
                modelKind: kind.rawValue
            )))
            if emitDeepAnalyzeTerminal {
                await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                    processed: 0, failed: 1, totalSeconds: 0,
                    modelKind: kind.rawValue, cancelled: false
                )))
            }
            return false
        }
        return true
    }

    /// Per-command dispatcher. `startScan` runs the scan in a detached task so
    /// the command loop stays responsive to subsequent pause/cancel commands.
    static func dispatch(_ cmd: IPCCommand, coordinator: ScanCoordinator,
                          sink: IPCSink, database: Database?) async {
        switch cmd.payload {
        case .chatRequest(let request):
            guard let database else {
                await sink.emit(.chatResponse(ChatResponse(requestID: request.requestID, conversationID: request.conversationID, status: "error", message: "The catalog database is unavailable.")))
                return
            }
            Task { await ChatService.shared.handle(request, database: database, sink: sink) }
        case .toolRequest(let request):
            guard let database else {
                await sink.emit(.toolResponse(ToolResponse(requestID: request.requestID, status: "error", message: "The catalog database is unavailable.")))
                return
            }
            Task { await sink.emit(.toolResponse(await MediaTools.shared.handle(request, database: database))) }
        case .catalogRequest(let request):
            guard let database else {
                await sink.emit(.catalogResponse(CatalogResponse(requestID: request.requestID, status: "error", message: "The catalog database is unavailable.")))
                return
            }
            if ["search", "autoTakeGroups", "scoreTakes", "rankTakesWithGoal", "takeGroupProposals"].contains(request.action) {
                Task { await sink.emit(.catalogResponse(await CatalogStore.handle(request, database: database))) }
                return
            }
            let response: CatalogResponse
            if request.action == "enqueueTimeline" {
                response = await TimelineAnalysis.shared.enqueue(request, database: database, sink: sink)
            } else if request.action == "enqueueTakeGrouping" {
                response = await CatalogTakeGrouping.shared.enqueue(request, database: database, sink: sink)
            } else if request.action == "enqueueFaceRefresh" {
                response = await FaceRefreshJob.shared.enqueue(request, database: database, sink: sink)
            } else if ["pauseJob", "resumeJob", "cancelJob"].contains(request.action) {
                let kind = try? await database.pool.read { db in try String.fetchOne(db, sql: "SELECT kind FROM catalog_jobs WHERE id=?", arguments: [request.jobID ?? ""]) }
                if kind == "timelineSample" {
                    response = await TimelineAnalysis.shared.control(request, database: database, sink: sink)
                } else if kind == CatalogTakeGrouping.kind {
                    response = await CatalogTakeGrouping.shared.control(request, database: database, sink: sink)
                } else if kind == FaceRefreshJob.kind {
                    response = await FaceRefreshJob.shared.control(request, database: database, sink: sink)
                } else if kind == CatalogIndexJob.kind {
                    response = await CatalogStore.handle(request, database: database)
                } else {
                    response = CatalogResponse(requestID: request.requestID, status: "error", message: "This job has no supported worker in this engine.")
                }
            } else {
                response = await CatalogStore.handle(request, database: database)
            }
            await sink.emit(.catalogResponse(response))
        case .grantFolderAccess(let rootPath, let rootBookmark):
            do {
                try SecurityScopedAccessRegistry.shared.replaceRootAccess(path: rootPath, bookmark: rootBookmark)
            } catch {
                await sink.emit(.error(EngineError(
                    kind: "folder_access_denied",
                    message: "FileID couldn't restore access to the selected folder. Choose it again."
                )))
            }
        case .startScan(let rootPath, let rootBookmark, let rootDisplay, let rescan, let excludedPaths):
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot scan."
                )))
                // A bare error leaves the app's auto-pilot stuck on "Scanning…"
                // forever — it advances only on a scan-terminal event. Emit an
                // empty scanComplete so the assistant leaves the scanning state.
                await sink.emit(.scanComplete(ScanComplete(
                    sessionID: "", totalFiles: 0, processedFiles: 0,
                    failedFiles: 0, totalSeconds: 0
                )))
                return
            }
            // Reject a start-scan while one is already queued or running. Mirrors
            // the Windows engine's scan_session.rs guard. Prevents unbounded job
            // queue growth from a misbehaving or rapidly-clicking app client.
            if !(await JobQueue.shared.waitUntilIdle(category: .scan)) {
                await sink.emit(.error(EngineError(
                    kind: "scan_already_queued",
                    message: "A scan is already running or queued — cancel it first."
                )))
                return
            }
            // Retain selected-root access for follow-on catalog jobs.
            let rootLease: SecurityScopedAccessLease
            do {
                rootLease = try SecurityScopedAccessRegistry.shared.acquire(
                    path: rootPath,
                    bookmark: rootBookmark
                )
            } catch {
                await sink.emit(.error(EngineError(
                    kind: "folder_access_denied",
                    message: "FileID couldn't access that folder. Choose it again."
                )))
                await sink.emit(.scanComplete(ScanComplete(
                    sessionID: "", totalFiles: 0, processedFiles: 0,
                    failedFiles: 0, totalSeconds: 0
                )))
                return
            }
            let authorizedRoot = rootLease.path
            let displayPath = rootDisplay ?? authorizedRoot
            // Allocate the scan epoch synchronously here (the command loop
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
                    defer { rootLease.release() }
                    await runScan(rootPath: authorizedRoot, displayPath: displayPath,
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
        case .cancelRestructure:
            await coordinator.requestRestructureCancel()
            JSONLog.shared.info(ev: "restructure_cancel_requested")
        case .healthCheck(let requestID):
            guard HealthCheckResult.isValidRequestID(requestID) else {
                await sink.emit(.error(EngineError(
                    kind: "invalid_health_request",
                    message: "Health request ID must contain 1–128 ASCII letters, digits, underscores or hyphens."
                )))
                return
            }
            await sink.emit(.healthCheckResult(HealthCheckResult(
                requestID: requestID,
                pid: ProcessInfo.processInfo.processIdentifier
            )))
        case .listRoots(let requestID):
            guard HealthCheckResult.isValidRequestID(requestID) else {
                await sink.emit(.error(EngineError(
                    kind: "invalid_list_roots_request",
                    message: "listRoots needs a 1–128 character ASCII request ID."
                )))
                return
            }
            guard let database else { await sink.emit(.rootsList(RootsList(requestID: requestID, roots: []))); return }
            do {
                let roots = try await database.pool.read { try LibraryRoots.list($0) }
                await sink.emit(.rootsList(RootsList(requestID: requestID, roots: roots)))
            } catch {
                await sink.emit(.error(EngineError(kind: "list_roots_failed", message: error.localizedDescription)))
            }
        case .listModels(let requestID, let role):
            let roles: Set<String> = ["vision", "text", "embedding", "reranker", "speech"]
            guard HealthCheckResult.isValidRequestID(requestID), role.map(roles.contains) ?? true else {
                await sink.emit(.error(EngineError(
                    kind: "invalid_list_models_request",
                    message: "listModels needs a 1–128 character ASCII request ID and a known role."
                )))
                return
            }
            let memoryGB = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824
            let benchmarks = ModelBenchmarkStore.all()
            // Favorites plus the bundled MLX catalog, every entry pinned to the
            // locked mlx-swift-lm runtime; only vision and text roles exist on macOS.
            let models: [ModelCatalogItem] = AIModelKind.allCases.compactMap { kind in
                let kindRole = kind.supportsVision ? "vision" : "text"
                guard role.map({ $0 == kindRole }) ?? true else { return nil }
                let catalog = kind.catalogEntry
                let isFavorite = AIModelKind.favorites.contains(kind)
                let quantization = catalog.map { "\($0.quantization.bits)bit" } ?? (kind.displayName.contains("8-bit") ? "8bit" : "4bit")
                return ModelCatalogItem(
                    id: kind.rawValue, name: kind.displayName, family: kind.sourceRepo.components(separatedBy: "/").last ?? kind.rawValue,
                    upstream: catalog?.bases.first?.repo ?? kind.sourceRepo, role: kindRole, format: "mlx", quantization: quantization,
                    license: kind.licensePolicyKey, termsRequired: kind.licensePolicyKey == "Gemma", parametersBillion: nil,
                    downloadBytes: kind.approxBytes, estimatedResidentGB: kind.ramBudgetGB,
                    tier: ModelCatalogItem.tier(forResidentGB: kind.ramBudgetGB), status: isFavorite ? "curated" : "candidate", runnable: true,
                    installed: kind.isInstalledOnDisk(), fits: kind.ramBudgetGB <= memoryGB * 0.6,
                    benchmark: benchmarks[kind.rawValue]
                )
            }
            await sink.emit(.modelCatalog(ModelCatalog(requestID: requestID, physicalMemoryGB: memoryGB, vramMB: 0, models: models)))
        case .benchmarkModel(let requestID, let modelKind, _):
            let reportedKind = String(modelKind.prefix(128))
            guard HealthCheckResult.isValidRequestID(requestID), let kind = AIModelKind(rawValue: modelKind) else {
                await sink.emit(.benchmarkResult(BenchmarkResult(
                    requestID: String(requestID.prefix(128)), modelKind: reportedKind, status: "error",
                    message: "benchmarkModel needs a valid request ID and model kind.")))
                return
            }
            guard ModelLicenseAcceptance.isAccepted(for: kind) else {
                await sink.emit(.benchmarkResult(BenchmarkResult(
                    requestID: requestID, modelKind: reportedKind, status: "error",
                    message: "Accept the source terms for \(kind.displayName) before benchmarking it.")))
                return
            }
            let claimed = await ModelBenchmarkSlot.shared.claim {
                Task.detached(priority: .userInitiated) {
                    SleepGuard.shared.begin(reason: "Model benchmark")
                    defer { SleepGuard.shared.end() }
                    JSONLog.shared.info(ev: "benchmark_started", extra: ["kind": AnyCodable(kind.rawValue)])
                    do {
                        let benchmark = try await ModelBenchmarkRunner.run(kind: kind) { phase, completed, total in
                            await sink.emit(.benchmarkProgress(BenchmarkProgress(
                                requestID: requestID, modelKind: reportedKind, phase: phase, completed: completed, total: total)))
                        }
                        ModelBenchmarkStore.save(benchmark, for: kind.rawValue)
                        await sink.emit(.benchmarkResult(BenchmarkResult(
                            requestID: requestID, modelKind: reportedKind, status: "ok", benchmark: benchmark)))
                        JSONLog.shared.info(ev: "benchmark_done", extra: ["kind": AnyCodable(kind.rawValue)])
                    } catch is CancellationError {
                        await sink.emit(.benchmarkResult(BenchmarkResult(
                            requestID: requestID, modelKind: reportedKind, status: "cancelled",
                            message: "Benchmark cancelled; no result was stored.")))
                    } catch {
                        await sink.emit(.benchmarkResult(BenchmarkResult(
                            requestID: requestID, modelKind: reportedKind, status: "error",
                            message: String(error.localizedDescription.prefix(2_000)))))
                    }
                    await ModelBenchmarkSlot.shared.finish()
                }
            }
            if !claimed {
                await sink.emit(.benchmarkResult(BenchmarkResult(
                    requestID: requestID, modelKind: reportedKind, status: "error",
                    message: "Another benchmark is already running.")))
            }
        case .cancelBenchmark:
            await ModelBenchmarkSlot.shared.cancel()
        case .requestStatus:
            if let snap = await coordinator.snapshot() {
                await sink.emit(.progress(snap))
            }
        case .shutdown:
            SecurityScopedAccessRegistry.shared.releaseRootAccess()
            JSONLog.shared.info(ev: "shutdown_requested")
            // Cancel any active scan so paused/in-flight workers exit
            // deterministically. Without this, a scan paused at shutdown
            // leaves its workers spinning in the pause-poll loop forever and
            // `awaitActiveScan()` (in main) blocks the clean exit indefinitely.
            // requestShutdown (not requestCancel) also trips the dedicated
            // shutdown mirror so a clustering pass started after a cancelled scan
            // aborts promptly instead of running to its persist first.
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
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_failed", message: "Database unavailable for Deep Analyze.",
                    modelKind: modelKind
                )))
                await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                    processed: 0, failed: 1, totalSeconds: 0,
                    modelKind: modelKind, cancelled: false
                )))
                return
            }
            guard let kind = AIModelKind(rawValue: modelKind) else {
                await sink.emit(.error(EngineError(
                    kind: "unknown_model", message: "Unknown Deep Analyze model: \(modelKind).",
                    modelKind: modelKind
                )))
                await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                    processed: 0, failed: 1, totalSeconds: 0,
                    modelKind: modelKind, cancelled: false
                )))
                return
            }
            guard await requireLicenseAcceptance(
                for: kind, sink: sink, emitDeepAnalyzeTerminal: true
            ) else { return }
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
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_failed", message: "Database unavailable for Deep Analyze.",
                    modelKind: modelKind
                )))
                await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                    processed: 0, failed: 1, totalSeconds: 0,
                    modelKind: modelKind, cancelled: false
                )))
                return
            }
            guard let kind = AIModelKind(rawValue: modelKind) else {
                await sink.emit(.error(EngineError(
                    kind: "unknown_model", message: "Unknown Deep Analyze model: \(modelKind).",
                    modelKind: modelKind
                )))
                await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                    processed: 0, failed: 1, totalSeconds: 0,
                    modelKind: modelKind, cancelled: false
                )))
                return
            }
            guard await requireLicenseAcceptance(
                for: kind, sink: sink, emitDeepAnalyzeTerminal: true
            ) else { return }
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
        case .deepAnalyzeAll(let modelKind, let skipExisting, let tagsOnly, let proposeRenames, let fileIDs, let excludedFolders):
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_failed", message: "Database unavailable for Deep Analyze.",
                    modelKind: modelKind
                )))
                await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                    processed: 0, failed: 1, totalSeconds: 0,
                    modelKind: modelKind, cancelled: false
                )))
                return
            }
            guard let kind = AIModelKind(rawValue: modelKind) else {
                await sink.emit(.error(EngineError(
                    kind: "unknown_model", message: "Unknown Deep Analyze model: \(modelKind).",
                    modelKind: modelKind
                )))
                await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                    processed: 0, failed: 1, totalSeconds: 0,
                    modelKind: modelKind, cancelled: false
                )))
                return
            }
            guard await requireLicenseAcceptance(
                for: kind, sink: sink, emitDeepAnalyzeTerminal: true
            ) else { return }
            if let fileIDs, fileIDs.count > 10_000 {
                await sink.emit(.error(EngineError(
                    kind: "deep_analyze_selection_too_large",
                    message: "Analyze Selected accepts at most 10,000 files per run. Reduce the selection and try again.",
                    modelKind: modelKind
                )))
                await sink.emit(.deepAnalyzeComplete(DeepAnalyzeComplete(
                    processed: 0, failed: 1, totalSeconds: 0,
                    modelKind: modelKind, cancelled: false
                )))
                return
            }
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
                // excludedFolders applies ONLY to the whole-library path — an
                // explicit fileIDs selection is a deliberate per-file action
                // and is never silently filtered (schema; mirrors the Rust +
                // C# engines).
                let scope: DeepAnalyzeScope = if let fileIDs {
                    .selected(fileIDs: fileIDs, skipExisting: skipExisting)
                } else {
                    .wholeLibrary(skipExisting: skipExisting, excludedFolders: excludedFolders ?? [])
                }
                await DeepAnalyzeRunner.run(database: database, sink: sink,
                                             scope: scope,
                                             modelKind: kind,
                                             tagsOnly: tagsOnly ?? false,
                                             proposeRenames: proposeRenames ?? true)
            })
        case .deepAnalyzeCancel:
            if await JobQueue.shared.hasActive(category: .deepAnalyze) {
                await DeepAnalyze.shared.requestCancel()
                JSONLog.shared.info(ev: "deep_analyze_cancel_requested")
            } else {
                await DeepAnalyze.shared.clearCancel()
                JSONLog.shared.info(ev: "deep_analyze_cancel_ignored_idle")
            }
        case .prewarmModel(let modelKey):
            guard let kind = AIModelKind(rawValue: modelKey) else {
                // Canonical cross-platform kind for "engine doesn't recognize
                // this model id" — matches the Windows engine's `unknown_model`
                // (was macOS `prewarm_invalid_kind`). Stamp `modelKind` so the
                // app can route it to the right install slot. (003)
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
            await DeepAnalyze.shared.startPrewarm {
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
            }
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
                                truncated: true,
                                confidenceCounts: legacyPlan.confidenceCounts)
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
        case .applyRestructure(let libraryRoot, let moves, let useSymlinks, let planID):
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot apply a restructure."
                )))
                return
            }
            // Fail closed instead of silently converting a symlink-preview
            // request into permanent real moves: macOS has no symlink-apply
            // engine path (no shortcut-mode journal to undo it with), so the
            // prior "accepted for wire parity and ignored" behavior meant a
            // caller asking for a reversible preview got irreversible moves
            // with no warning. Checked BEFORE reserveRestructure() so a
            // rejected request never wedges later applies/undos on
            // restructure_busy. (see DECISIONS.md)
            guard !useSymlinks else {
                await sink.emit(.error(EngineError(
                    kind: "apply_restructure",
                    message: "Symlink-preview mode is not supported by the macOS engine; it only performs real on-disk moves. Send useSymlinks: false."
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
                        applied: result.moved, failed: result.failed, privilegeError: nil,
                        cancelled: result.cancelled)))
                } catch {
                    if let journalError = error as? Restructure.UndoJournalError {
                        let result = journalError.result
                        await sink.emit(.restructureApplyResult(RestructureApplyResult(
                            applied: result.moved, failed: result.failed, privilegeError: nil,
                            cancelled: result.cancelled)))
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

        case .undoRestructure(let libraryRoot, let shortcutUndoToken):
            // Reverse the last apply by replaying the engine's on-disk undo
            // journal. Same machinery as apply (real moves, cancellable, terminal
            // restructureApplyResult), so register it the same way.
            //
            // A non-nil shortcutUndoToken asks to undo only ONE shortcut-mode
            // (symlink) apply run, never the real-move journal (schema:
            // shortcutUndoToken). macOS has no symlink-apply mode at all (see the
            // useSymlinks rejection above), so there is no shortcut journal to
            // replay — fail closed rather than silently falling through to
            // undoLast() and replaying the real-move journal instead, which
            // would violate that contract. Checked BEFORE reserveRestructure()
            // so a rejected request can't wedge restructure_busy for every
            // later apply/undo (ScanCoordinator's reservation has no timeout).
            // (see DECISIONS.md)
            guard shortcutUndoToken == nil else {
                // EngineClient's undoRestructureInFlight flag is cleared ONLY by
                // a terminal restructureApplyResult (app) — an .error
                // alone would leave it latched and mis-attribute the NEXT apply's
                // result to this rejected undo. Emit both, mirroring the
                // UndoJournalError catch arm above.
                await sink.emit(.restructureApplyResult(RestructureApplyResult(
                    applied: 0, failed: 0, privilegeError: nil, cancelled: false)))
                await sink.emit(.error(EngineError(
                    kind: "undo_restructure",
                    message: "Shortcut-mode restructure is not supported by the macOS engine; there is no shortcut undo journal to replay."
                )))
                return
            }
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
                    applied: result.moved, failed: result.failed, privilegeError: nil,
                    cancelled: result.cancelled)))
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
            let identities = exactIdentities ?? []
            let exact = Dictionary(identities.map { ($0.fileID, $0) }, uniquingKeysWith: { first, _ in first })
            guard exact.count == identities.count else {
                await sink.emit(.bulkActionResult(BulkActionResult(
                    action: "trashFiles", succeeded: 0, failed: fileIDs.count,
                    messages: fileIDs.map { item($0, ok: false, "The request named a file's duplicate evidence twice. Nothing was moved.") })))
                return
            }
            guard let database else { await emitDbUnavailable(sink, action: "trashFiles"); return }
            await sink.emit(.bulkActionResult(await trashFiles(database: database, fileIDs: fileIDs, exact: exact)))
        case .restoreFromTrash(let batchID):
            guard let database else { await emitDbUnavailable(sink, action: "restoreFromTrash"); return }
            await sink.emit(.bulkActionResult(await TrashJournal.restore(batchID: batchID, database: database)))
        case .mergeClusters(let sourcePersonID, let destinationPersonID):
            guard let database else { await emitDbUnavailable(sink, action: "mergeClusters"); return }
            await sink.emit(.bulkActionResult(await mergeClusters(database: database, source: sourcePersonID, destination: destinationPersonID)))
        case .revertMerge(let sourcePersonID, let destinationPersonID, let faceIDsToRevert):
            guard let database else { await emitDbUnavailable(sink, action: "revertMerge"); return }
            await sink.emit(.bulkActionResult(await revertMerge(
                database: database, source: sourcePersonID, destination: destinationPersonID, faceIDs: faceIDsToRevert)))
        case .renamePerson(let personID, let title, let firstName, let middleName, let lastName, let suffix):
            guard let database else { await emitDbUnavailable(sink, action: "renamePerson"); return }
            await sink.emit(.bulkActionResult(await renamePerson(database: database, personID: personID, title: title, firstName: firstName, middleName: middleName, lastName: lastName, suffix: suffix)))
        case .reassignFace(let faceID, let destinationPersonID, let createNewPerson):
            guard let database else { await emitDbUnavailable(sink, action: "reassignFace"); return }
            await sink.emit(.bulkActionResult(await reassignFace(
                database: database, faceID: faceID,
                destinationPersonID: destinationPersonID, createNewPerson: createNewPerson
            )))
        case .listFaceReview(let requestID, let personID, let offset, let limit, let queue):
            await emitFaceReview(.list(requestID: requestID, personID: personID, offset: offset, limit: limit, queue: queue),
                                 database: database, sink: sink)
        case .resolveFaceReview(let requestID, let faceID, let personID, let resolution, let otherPersonID):
            await emitFaceReview(.resolve(requestID: requestID, faceID: faceID, personID: personID, resolution: resolution,
                                          otherPersonID: otherPersonID),
                                 database: database, sink: sink)
        case .undoFaceReview(let requestID, let operationID):
            await emitFaceReview(.undo(requestID: requestID, operationID: operationID), database: database, sink: sink)
        case .markPersonsAsUnknown(let personIDs):
            guard let database else { await emitDbUnavailable(sink, action: "markPersonsAsUnknown"); return }
            await sink.emit(.bulkActionResult(await markPersonsAsUnknown(database: database, personIDs: personIDs)))
        case .markPersonsDifferent(let sourcePersonID, let destinationPersonID,
                                   let sourceAnchorFaceID, let destinationAnchorFaceID):
            guard let database else { await emitDbUnavailable(sink, action: "markPersonsDifferent"); return }
            await sink.emit(.bulkActionResult(await markPersonsDifferent(
                database: database,
                sourcePersonID: sourcePersonID,
                destinationPersonID: destinationPersonID,
                sourceAnchorFaceID: sourceAnchorFaceID,
                destinationAnchorFaceID: destinationAnchorFaceID
            )))
        case .wipeLibrary:
            guard let database else { await sink.emit(.libraryWiped(LibraryWiped(ok: false, message: "Database unavailable."))); return }
            await sink.emit(.libraryWiped(await wipeLibrary(database: database)))
        case .findMergeSuggestions:
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot find merge suggestions."
                )))
                await sink.emit(.error(EngineError(
                    kind: "find_merge_suggestions_failed",
                    message: "Merge suggestions are unavailable because the library database could not be opened."
                )))
                return
            }
            Task.detached(priority: .userInitiated) {
                await sink.emit(await QueryCommands.mergeSuggestions(database: database))
            }
        case .embedTextQuery(let query, let queryID):
            Task.detached(priority: .userInitiated) {
                await sink.emit(QueryCommands.textEmbedding(query: query, queryID: queryID))
            }
        case .embedImageQuery(let fileID, let queryID):
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot embed the file query."
                )))
                return
            }
            Task.detached(priority: .userInitiated) {
                await sink.emit(await QueryCommands.imageEmbedding(database: database, fileID: fileID, queryID: queryID))
            }
        case .generateVideoThumbnail(let path, let modifiedAt, let maxPixelSize):
            Task.detached(priority: .utility) {
                if let event = await QueryCommands.videoThumbnail(path: path, modifiedAt: modifiedAt, maxPixelSize: maxPixelSize) {
                    await sink.emit(event)
                }
            }
        case .verifyCudaPack:
            await sink.emit(.error(EngineError(
                kind: "not_applicable_on_platform",
                message: "CUDA isn't available on Apple Silicon — the AppleProvider EP selects ANE/Metal automatically."
            )))
        }
    }

    static func emitFaceReview(_ command: FaceReview.Command, database: Database?, sink: IPCSink) async {
        guard HealthCheckResult.isValidRequestID(command.requestID) else {
            await sink.emit(.error(EngineError(
                kind: "invalid_face_review_request",
                message: "Face review requests need a 1–128 character ASCII request ID."
            )))
            return
        }
        await sink.emit(.faceReview(await FaceReview.reply(command, database: database)))
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
        var sizeBytes: Int64 = 0
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
                    sql: "SELECT id, path_text, file_ref, size_bytes FROM files WHERE id IN (\(placeholders))",
                    arguments: StatementArguments(chunk))
                for row in rows {
                    let id: Int64 = row["id"]
                    result[id] = BulkFileState(path: row["path_text"], fileRef: row["file_ref"], sizeBytes: row["size_bytes"] ?? 0)
                }
            }
            return result
        }
    }

    struct PlannedRename: Sendable {
        let slot: Int
        let fileID: Int64
        let oldURL: URL
        let newURL: URL
        let newName: String
        var sidecars: [(url: URL, suffix: String)] = []
    }

    static func renameFiles(database: Database, renames: [RenameEntry]) async -> BulkActionResult {
        await renameFiles(database: database, renames: renames) { try FileManager.default.moveItem(at: $0, to: $1) }
    }

    /// Renames each file in place. Companions in the batch (same folder, same
    /// stem, at least one RAW, JPEG/HEIC or MOV) move together: when one member
    /// can't be renamed, the members already moved are put back and the whole
    /// group is reported failed. Uncatalogued `.xmp`/`.aae` sidecars follow.
    static func renameFiles(database: Database, renames: [RenameEntry], move: (URL, URL) throws -> Void) async -> BulkActionResult {
        SleepGuard.shared.begin(reason: "Bulk rename")
        defer { SleepGuard.shared.end() }
        let states: [Int64: BulkFileState]
        do {
            states = try await fetchBulkStates(database: database,
                                               fileIDs: renames.map(\.fileID))
        } catch {
            return BulkActionResult(
                action: "renameFiles", succeeded: 0, failed: renames.count,
                messages: renames.map { item($0.fileID, ok: false, error.localizedDescription) })
        }
        var results = [BulkActionItem?](repeating: nil, count: renames.count)
        var planned: [PlannedRename] = []
        var rejected: [(url: URL, reason: String)] = []
        for (slot, rename) in renames.enumerated() {
            switch planRename(rename, slot: slot, state: states[rename.fileID]) {
            case .success(let plan): planned.append(plan)
            case .failure(let refusal):
                results[slot] = item(rename.fileID, ok: false, refusal.message)
                if let state = states[rename.fileID] { rejected.append((URL(fileURLWithPath: state.path), refusal.message)) }
            }
        }

        var counts: [SmartNameStyle.Key: (count: Int, primary: Bool)] = [:]
        for url in planned.map(\.oldURL) + rejected.map(\.url) {
            guard let (key, primary) = companionKey(url) else { continue }
            let known = counts[key] ?? (0, false)
            counts[key] = (known.count + 1, known.primary || primary)
        }
        let stems = counts
        func groupOf(_ url: URL) -> SmartNameStyle.Key? {
            guard let key = companionKey(url)?.key, let stem = stems[key], stem.count >= 2, stem.primary else { return nil }
            return key
        }
        var blocked: [SmartNameStyle.Key: String] = [:]
        for (url, reason) in rejected {
            if let key = groupOf(url), blocked[key] == nil {
                blocked[key] = "Not renamed: \(url.lastPathComponent) couldn't be renamed, so the group keeps its names. \(reason)"
            }
        }
        planned.removeAll { plan in
            guard let key = groupOf(plan.oldURL), let why = blocked[key] else { return false }
            results[plan.slot] = item(plan.fileID, ok: false, why)
            return true
        }
        await planFollowingSidecars(database: database, planned: &planned)

        var units: [[Int]] = []
        var unitOf: [SmartNameStyle.Key: Int] = [:]
        for (index, plan) in planned.enumerated() {
            guard let key = groupOf(plan.oldURL) else { units.append([index]); continue }
            if let unit = unitOf[key] { units[unit].append(index) } else { unitOf[key] = units.count; units.append([index]) }
        }
        for unit in units {
            var moved: [(index: Int, intent: String)] = []
            var failure: (index: Int, message: String)?
            for index in unit {
                do { moved.append((index, try moveRenamed(planned[index], move: move))) } catch {
                    failure = (index, (error as? BulkRefusal)?.message ?? error.localizedDescription)
                    break
                }
            }
            guard let failure else {
                for (index, intent) in moved {
                    results[planned[index].slot] = await commitRename(database: database, planned[index], intent: intent, notes: [], followSidecars: true)
                }
                continue
            }
            let bad = planned[failure.index]
            results[bad.slot] = item(bad.fileID, ok: false, unit.count > 1 ? "\(failure.message) The group keeps its names." : failure.message)
            let why = "Not renamed: \(bad.oldURL.lastPathComponent) couldn't be renamed, so the group keeps its names. \(failure.message)"
            for (index, intent) in moved.reversed() {
                let plan = planned[index]
                do {
                    try FileManager.default.moveItem(at: plan.newURL, to: plan.oldURL)
                    PathMoveJournal.finish(intent)
                    results[plan.slot] = item(plan.fileID, ok: false, why)
                } catch {
                    let note = "Couldn't be put back after \(bad.oldURL.lastPathComponent) failed: \(error.localizedDescription)"
                    results[plan.slot] = await commitRename(database: database, plan, intent: intent, notes: [note], followSidecars: false)
                }
            }
            for index in unit where results[planned[index].slot] == nil {
                results[planned[index].slot] = item(planned[index].fileID, ok: false, why)
            }
        }
        let messages = results.compactMap { $0 }
        return BulkActionResult(action: "renameFiles", succeeded: messages.filter(\.ok).count, failed: messages.filter { !$0.ok }.count, messages: messages)
    }

    static func planRename(_ rename: RenameEntry, slot: Int, state: BulkFileState?) -> Result<PlannedRename, BulkRefusal> {
        let trimmed = rename.newName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !trimmed.contains("/"), !trimmed.contains("\0") else { return .failure(BulkRefusal("Invalid filename.")) }
        guard let state else { return .failure(BulkRefusal("File row not found.")) }
        let oldURL = URL(fileURLWithPath: state.path)
        guard state.fileRef != nil else { return .failure(BulkRefusal("Source identity is unavailable; rescan before renaming.")) }
        guard !Restructure.fileRefSwapped(dbRef: state.fileRef, currentRef: Discovery.inode(of: oldURL)) else {
            return .failure(BulkRefusal("File changed since it was indexed."))
        }
        let newURL = oldURL.deletingLastPathComponent().appendingPathComponent(trimmed)
        guard newURL.path != oldURL.path else { return .failure(BulkRefusal("Destination already exists.")) }
        return .success(PlannedRename(slot: slot, fileID: rename.fileID, oldURL: oldURL, newURL: newURL, newName: trimmed))
    }

    /// Same folder and same lowercased companion stem, plus whether the file is
    /// a RAW, JPEG/HEIC or MOV rather than an `.xmp`/`.aae` sidecar.
    static func companionKey(_ url: URL) -> (key: SmartNameStyle.Key, primary: Bool)? {
        guard let parts = SmartNameCompanions.base(url.lastPathComponent) else { return nil }
        return (SmartNameStyle.key(url.deletingLastPathComponent().path + "\u{0}" + SmartNameStyle.lower(parts.stem)), parts.role != .sidecar)
    }

    /// Uncatalogued `.xmp`/`.aae` sidecars that follow each planned rename (see
    /// `SmartNameCompanions.followingSidecars`). Each folder is read once.
    static func planFollowingSidecars(database: Database, planned: inout [PlannedRename]) async {
        var folders: [(dir: URL, members: [Int])] = []
        var folderIndex: [String: Int] = [:]
        for (index, plan) in planned.enumerated() {
            let dir = plan.oldURL.deletingLastPathComponent()
            if let at = folderIndex[dir.path] { folders[at].members.append(index) } else {
                folderIndex[dir.path] = folders.count
                folders.append((dir, [index]))
            }
        }
        var candidates: [(owner: Int, url: URL, suffix: String)] = []
        for folder in folders {
            let names = folder.members.map { planned[$0].oldURL.lastPathComponent }
            guard names.contains(where: { SmartNameCompanions.base($0).map { $0.role != .sidecar } ?? false }),
                  let listing = try? FileManager.default.contentsOfDirectory(atPath: folder.dir.path) else { continue }
            for found in SmartNameCompanions.followingSidecars(planned: names, listing: listing) {
                let url = folder.dir.appendingPathComponent(found.name)
                var info = stat()
                guard lstat(url.path, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG else { continue }
                candidates.append((folder.members[found.owner], url, found.suffix))
            }
        }
        guard !candidates.isEmpty else { return }
        let paths = candidates.map(\.url.path)
        guard let catalogued = try? await database.pool.read({ db in
            Set(try String.fetchAll(db, sql: "SELECT path_text FROM files WHERE path_text IN (\(paths.map { _ in "?" }.joined(separator: ",")))",
                                    arguments: StatementArguments(paths)))
        }) else { return }
        for candidate in candidates where !catalogued.contains(candidate.url.path) {
            planned[candidate.owner].sidecars.append((candidate.url, candidate.suffix))
        }
    }

    static func moveRenamed(_ plan: PlannedRename, move: (URL, URL) throws -> Void) throws -> String {
        guard !FileManager.default.fileExists(atPath: plan.newURL.path) || isCaseOnlyRename(from: plan.oldURL, to: plan.newURL) else {
            throw BulkRefusal("Destination already exists.")
        }
        try ReadOnlyLocations.requireSourceMutation(plan.oldURL)
        try ReadOnlyLocations.requireSourceMutation(plan.newURL)
        let intent = PathMoveJournal.begin(fileID: plan.fileID, from: plan.oldURL.path, to: plan.newURL.path, fileRef: Discovery.inode(of: plan.oldURL))
        do { try move(plan.oldURL, plan.newURL) } catch {
            PathMoveJournal.finish(intent)
            throw error
        }
        return intent
    }

    static func commitRename(database: Database, _ plan: PlannedRename, intent: String, notes: [String], followSidecars: Bool) async -> BulkActionItem {
        defer { PathMoveJournal.finish(intent) }
        let notes = notes + (followSidecars ? moveFollowingSidecars(plan) : [])
        let note = notes.isEmpty ? nil : notes.joined(separator: " ")
        let (fileID, from, to, newName, oldExtension) = (plan.fileID, plan.oldURL.path, plan.newURL.path, plan.newName, plan.oldURL.pathExtension)
        do {
            try await database.pool.write { db in
                try PathMoveJournal.updatePath(db, fileID: fileID, from: from, to: to)
                guard db.changesCount == 1 else {
                    throw CocoaError(.fileWriteUnknown)
                }
                if let proposed = try String.fetchOne(db, sql: "SELECT vlm_proposed_name FROM files WHERE id = ?", arguments: [fileID]),
                   appliesProposedName(proposed, newName: newName, originalExtension: oldExtension) {
                    try db.execute(sql: "UPDATE files SET vlm_proposed_name = NULL WHERE id = ?", arguments: [fileID])
                }
            }
            return item(fileID, ok: true, note)
        } catch {
            return item(fileID, ok: false, [error.localizedDescription, note].compactMap { $0 }.joined(separator: " "))
        }
    }

    /// Moves each sidecar planned for `plan` to `<new stem><its old suffix>`.
    /// Best-effort: an occupied target or an error leaves it in place and returns a note.
    static func moveFollowingSidecars(_ plan: PlannedRename) -> [String] {
        let stem = SmartNameStyle.splitName(plan.newURL.lastPathComponent).stem
        let dir = plan.newURL.deletingLastPathComponent()
        var notes: [String] = []
        for sidecar in plan.sidecars {
            let target = dir.appendingPathComponent(stem + sidecar.suffix)
            guard target.path != sidecar.url.path else { continue }
            let (old, new) = (sidecar.url.lastPathComponent, target.lastPathComponent)
            do {
                try ReadOnlyLocations.requireSourceMutation(sidecar.url)
                try ReadOnlyLocations.requireSourceMutation(target)
                try renameNoReplace(sidecar.url, target)
            } catch let error as POSIXError where error.code == .EEXIST {
                notes.append("\(old) kept its name because \(new) already exists.")
            } catch {
                notes.append("\(old) kept its name: \(error.localizedDescription)")
            }
        }
        return notes
    }

    /// `rename` that never replaces an existing file; volumes without
    /// RENAME_EXCL (exFAT, SMB) fall back to a checked move.
    static func renameNoReplace(_ source: URL, _ target: URL) throws {
        if renamex_np(source.path, target.path, UInt32(RENAME_EXCL)) == 0 { return }
        let code = errno
        guard code == ENOTSUP || code == EINVAL else { throw POSIXError(POSIXErrorCode(rawValue: code) ?? .EIO) }
        guard !FileManager.default.fileExists(atPath: target.path) || isCaseOnlyRename(from: source, to: target) else { throw POSIXError(.EEXIST) }
        try FileManager.default.moveItem(at: source, to: target)
    }

    /// On a case-insensitive volume the destination of `Beach.jpg → beach.jpg`
    /// "exists" because it is the file itself. Requiring the same inode and a
    /// case-only difference keeps a second hard link from passing as the same slot.
    static func isCaseOnlyRename(from oldURL: URL, to newURL: URL) -> Bool {
        oldURL.lastPathComponent != newURL.lastPathComponent
            && oldURL.lastPathComponent.caseInsensitiveCompare(newURL.lastPathComponent) == .orderedSame
            && Discovery.inode(of: oldURL).map { $0 == Discovery.inode(of: newURL) } == true
    }

    /// True when `newName` is the suggested name taken as-is or with the
    /// `_2`…`_99` bump the app adds when that name is taken.
    static func appliesProposedName(_ proposed: String, newName: String, originalExtension ext: String) -> Bool {
        let proposed = proposed.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !proposed.isEmpty else { return false }
        let suffix = ext.isEmpty ? "" : "." + ext
        guard newName.hasSuffix(suffix) else { return false }
        let stem = String(newName.dropLast(suffix.count))
        if stem == proposed { return true }
        guard stem.hasPrefix(proposed + "_") else { return false }
        let bump = stem.dropFirst(proposed.count + 1)
        return bump.allSatisfy { $0.isASCII && $0.isNumber } && Int(bump).map { (2...99).contains($0) } == true
    }

    static func orderedUniqueFileIDs(_ fileIDs: [Int64]) -> [Int64] {
        var seen = Set<Int64>()
        return fileIDs.filter { seen.insert($0).inserted }
    }

    /// Cleanup's exact-duplicate evidence, re-proved right before the move (mirrors the
    /// shared engine's `validate_trash_candidate_at`): the copy and the kept file must
    /// both still hash to the SHA-256 the app verified, with equal sizes. Returns the
    /// copy's identity to re-check immediately before Trash, or the reason to refuse.
    static func verifyExactDuplicate(_ expected: ExactTrashIdentity, state: BulkFileState) -> Result<ExactFileDigest.CacheKey, BulkRefusal> {
        let copy = URL(fileURLWithPath: state.path), kept = URL(fileURLWithPath: expected.keeperPath)
        guard Restructure.pathsEqual(expected.path, state.path), expected.sizeBytes == state.sizeBytes,
              expected.sizeBytes == expected.keeperSizeBytes, expected.sha256Hex == expected.keeperSha256Hex,
              !Restructure.pathsEqual(expected.path, expected.keeperPath) else {
            return .failure(BulkRefusal("The duplicate check doesn't match this file any more. Nothing was moved."))
        }
        guard let keeper = ExactFileDigest.computeWithIdentity(url: kept, expectedSize: expected.keeperSizeBytes),
              hex(keeper.digest) == expected.keeperSha256Hex else {
            return .failure(BulkRefusal("The copy you're keeping changed or moved. Nothing was moved."))
        }
        guard let victim = ExactFileDigest.computeWithIdentity(url: copy, expectedSize: expected.sizeBytes),
              hex(victim.digest) == expected.sha256Hex else {
            return .failure(BulkRefusal("This copy changed since it was checked. Nothing was moved."))
        }
        return .success(victim.identity)
    }

    struct BulkRefusal: Error { let message: String; init(_ message: String) { self.message = message } }

    private static func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }

    static func trashFiles(
        database: Database, fileIDs: [Int64], exact: [Int64: ExactTrashIdentity] = [:],
        journal journalURL: URL = TrashJournal.defaultURL,
        trash: (URL) throws -> URL? = { url in
            var landed: NSURL?
            try FileManager.default.trashItem(at: url, resultingItemURL: &landed)
            return landed as URL?
        }
    ) async -> BulkActionResult {
        SleepGuard.shared.begin(reason: "Bulk trash")
        defer { SleepGuard.shared.end() }
        let uniqueIDs = orderedUniqueFileIDs(fileIDs)
        var messages: [BulkActionItem] = []
        var journal: [TrashJournal.Item] = []
        let batchID = UUID().uuidString.lowercased(), createdAt = Date().timeIntervalSince1970
        var journaled = false
        var dirty = false
        var lastJournalWrite = Date.distantPast
        func writeJournal() {
            guard dirty else { return }
            if (try? TrashJournal.upsert(TrashJournal.Batch(id: batchID, createdAt: createdAt, items: journal), at: journalURL)) != nil {
                journaled = !journal.isEmpty
                dirty = false
            }
            lastJournalWrite = Date()
        }
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
            var verifiedCopy: ExactFileDigest.CacheKey?
            if let expected = exact[id] {
                switch verifyExactDuplicate(expected, state: state) {
                case .success(let identity): verifiedCopy = identity
                case .failure(let refusal):
                    messages.append(item(id, ok: false, refusal.message))
                    continue
                }
            }
            do {
                try ReadOnlyLocations.requireSourceMutation(url)
                let rows = (try? await database.pool.read { db in try TrashJournal.snapshot(db, fileID: id) }) ?? []
                if let verifiedCopy, !ExactFileDigest.pathStillMatches(verifiedCopy) {
                    messages.append(item(id, ok: false, "This copy changed since it was checked. Nothing was moved."))
                    continue
                }
                // Write-ahead: record the file before it moves, so a kill right after the move
                // still leaves an entry; restore finds it in the Trash by inode. Each rewrite is
                // O(batch), so past the first files it is capped at ~4/s to keep thousands-file
                // batches linear (Finder Put Back covers that gap).
                journal.append(TrashJournal.Item(fileID: id, originalPath: state.path, trashedPath: "", rows: rows))
                dirty = true
                if journal.count <= 64 || Date().timeIntervalSince(lastJournalWrite) >= 0.25 { writeJournal() }
                let landed: URL?
                do { landed = try trash(url) } catch { journal.removeLast(); throw error }
                if let landed {
                    journal[journal.count - 1].trashedPath = landed.path
                } else {
                    journal.removeLast()
                }
                dirty = true
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
        writeJournal()
        let action = journaled ? "trashFiles:" + batchID : "trashFiles"
        return BulkActionResult(action: action, succeeded: messages.filter(\.ok).count, failed: messages.filter { !$0.ok }.count, messages: messages)
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

    /// Mirrors the shared engine's `apply_revert_merge`: faces still owned by the
    /// destination move back to the source (recreated under its old id when the
    /// merge deleted it), faces already back count as restored, anything else is
    /// stale. The merge's `user-merged` verdict is dropped once any face returns.
    static func revertMerge(database: Database, source: Int64, destination: Int64, faceIDs: [Int64]) async -> BulkActionResult {
        let requested = Array(Set(faceIDs)).sorted()
        guard !requested.isEmpty, source != destination else {
            return BulkActionResult(action: "revertMerge", succeeded: 0, failed: 1,
                                    messages: [item(ok: false, "The merge undo has no faces to move back.")])
        }
        do {
            let (personID, restored, stale): (Int64?, Int, Int) = try await database.pool.write { db in
                let sourceExists = try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM persons WHERE id = ?)", arguments: [source]) ?? false
                let owners = try requested.map { face in
                    (face, try Int64.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id = ?", arguments: [face]))
                }
                let alreadySource = owners.contains { $0.1 == source }
                let destinationOwned = owners.contains { $0.1 == destination }
                let now = Date().timeIntervalSince1970
                let newPerson: Int64?
                if sourceExists && alreadySource {
                    newPerson = source
                } else if !destinationOwned {
                    newPerson = nil
                } else if !sourceExists {
                    try db.execute(sql: "INSERT INTO persons (id, is_unknown, created_at) VALUES (?, 0, ?)", arguments: [source, now])
                    newPerson = source
                } else {
                    try db.execute(sql: "INSERT INTO persons (is_unknown, created_at) VALUES (0, ?)", arguments: [now])
                    newPerson = db.lastInsertedRowID
                }
                var restored = 0, stale = 0
                for (face, owner) in owners {
                    if let newPerson, owner == newPerson {
                        restored += 1
                    } else if let newPerson, owner == destination {
                        try db.execute(sql: "UPDATE face_prints SET person_id = ? WHERE id = ? AND person_id = ?",
                                       arguments: [newPerson, face, destination])
                        if db.changesCount == 1 { restored += 1 } else { stale += 1 }
                    } else {
                        stale += 1
                    }
                }
                if restored > 0 {
                    try db.execute(sql: """
                        DELETE FROM face_verifications
                        WHERE person_a = ? AND person_b = ? AND same_person = 1 AND vlm_model = 'user-merged'
                        """, arguments: [min(source, destination), max(source, destination)])
                }
                for person in Set([destination, newPerson].compactMap { $0 }) {
                    try db.execute(sql: """
                        UPDATE persons
                        SET file_count = (SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id = ?1),
                            representative_face_id = COALESCE(
                                (SELECT fp.id FROM face_prints fp WHERE fp.person_id = ?1 AND fp.arcface_embedding IS NOT NULL
                                 ORDER BY COALESCE(fp.face_quality, 0) DESC LIMIT 1),
                                (SELECT fp.id FROM face_prints fp WHERE fp.person_id = ?1
                                 ORDER BY COALESCE(fp.face_quality, 0) DESC LIMIT 1))
                        WHERE id = ?1
                        """, arguments: [person])
                }
                return (newPerson, restored, stale)
            }
            let target = personID.map { "person #\($0)" } ?? "a restored person"
            let message = stale == 0
                ? "Restored \(restored) face print(s) to \(target)"
                : "Restored \(restored) face print(s) to \(target); skipped \(stale) stale or missing face(s)"
            return BulkActionResult(action: "revertMerge", succeeded: stale == 0 ? 1 : 0, failed: stale == 0 ? 0 : 1,
                                    messages: [item(ok: stale == 0, message)])
        } catch {
            return BulkActionResult(action: "revertMerge", succeeded: 0, failed: 1, messages: [item(ok: false, error.localizedDescription)])
        }
    }

    /// Mirrors the shared engine: move one face to an existing or new person,
    /// recompute both people's counts/representatives, drop an emptied
    /// unnamed source and its person-level (face-less) verdicts.
    static func reassignFace(database: Database, faceID: Int64, destinationPersonID: Int64?, createNewPerson: Bool) async -> BulkActionResult {
        func failure(_ message: String) -> BulkActionResult {
            BulkActionResult(action: "reassignFace", succeeded: 0, failed: 1, messages: [item(faceID, ok: false, message)])
        }
        if createNewPerson && destinationPersonID != nil {
            return failure("Choose either a new person or an existing destination, not both.")
        }
        do {
            let outcome: Result<Int64?, ReassignFaceRefusal> = try await database.pool.write { db in
                guard let previousRow = try Row.fetchOne(db, sql: "SELECT person_id FROM face_prints WHERE id = ?", arguments: [faceID]) else {
                    return .failure(.missingFace)
                }
                let previous: Int64? = previousRow["person_id"]
                let destination: Int64?
                if createNewPerson {
                    try db.execute(sql: "INSERT INTO persons (name, is_unknown, created_at) VALUES (NULL, 0, ?)",
                                   arguments: [Date().timeIntervalSince1970])
                    destination = db.lastInsertedRowID
                } else if let destinationPersonID {
                    let exists = try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM persons WHERE id = ?)", arguments: [destinationPersonID]) ?? false
                    guard exists else { return .failure(.missingDestination) }
                    destination = destinationPersonID
                } else {
                    destination = nil
                }
                if destination == previous { return .success(nil) }
                try db.execute(sql: "UPDATE face_prints SET person_id = ? WHERE id = ?", arguments: [destination, faceID])
                for personID in Set([previous, destination].compactMap { $0 }) {
                    try db.execute(sql: """
                        UPDATE persons
                        SET file_count = (SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id = ?1),
                            representative_face_id = COALESCE(
                                (SELECT fp.id FROM face_prints fp WHERE fp.person_id = ?1 AND fp.arcface_embedding IS NOT NULL
                                 ORDER BY COALESCE(fp.face_quality, 0) DESC LIMIT 1),
                                (SELECT fp.id FROM face_prints fp WHERE fp.person_id = ?1
                                 ORDER BY COALESCE(fp.face_quality, 0) DESC LIMIT 1)),
                            centroid = NULL, anchor_radius = NULL, last_clustered_at = NULL
                        WHERE id = ?1
                        """, arguments: [personID])
                }
                try FaceReview.recordReassignment(db, faceID: faceID, previous: previous, destination: destination)
                if let previous {
                    try db.execute(sql: """
                        DELETE FROM persons
                        WHERE id = ?1
                          AND NOT EXISTS (SELECT 1 FROM face_prints WHERE person_id = ?1)
                          AND COALESCE(TRIM(name), '') = '' AND COALESCE(TRIM(title), '') = ''
                          AND COALESCE(TRIM(first_name), '') = '' AND COALESCE(TRIM(middle_name), '') = ''
                          AND COALESCE(TRIM(last_name), '') = '' AND COALESCE(TRIM(suffix), '') = ''
                        """, arguments: [previous])
                    if db.changesCount > 0 {
                        try db.execute(sql: """
                            DELETE FROM face_verifications
                            WHERE (person_a = ?1 OR person_b = ?1) AND (face_a IS NULL OR face_b IS NULL)
                            """, arguments: [previous])
                    }
                }
                return .success(destination)
            }
            switch outcome {
            case .success(let destination):
                return BulkActionResult(action: "reassignFace", succeeded: 1, failed: 0,
                                        messages: [item(faceID, ok: true, destination.map { "Assigned to person #\($0)." })])
            case .failure(let refusal):
                return failure(refusal.message)
            }
        } catch {
            return failure(error.localizedDescription)
        }
    }

    private enum ReassignFaceRefusal: Error {
        case missingFace, missingDestination
        var message: String {
            switch self {
            case .missingFace: return "This face no longer exists; refresh People and try again."
            case .missingDestination: return "The destination person no longer exists; refresh People and try again."
            }
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
            let changed = try await database.pool.write { db -> Int in
                try db.execute(sql: """
                    UPDATE persons
                    SET title = ?, first_name = ?, middle_name = ?, last_name = ?, suffix = ?, name = ?, is_unknown = 0
                    WHERE id = ?
                    """, arguments: [clean(title), clean(firstName), clean(middleName), clean(lastName), clean(suffix), display.isEmpty ? nil : display, personID])
                return db.changesCount
            }
            guard changed > 0 else {
                return BulkActionResult(action: "renamePerson", succeeded: 0, failed: 1, messages: [item(personID, ok: false, missingPersonMessage)])
            }
            return BulkActionResult(action: "renamePerson", succeeded: 1, failed: 0, messages: [item(personID, ok: true)])
        } catch {
            return BulkActionResult(action: "renamePerson", succeeded: 0, failed: 1, messages: [item(personID, ok: false, error.localizedDescription)])
        }
    }

    static let missingPersonMessage = "This person no longer exists; refresh People and try again."

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
                        messages.append(db.changesCount > 0 ? item(id, ok: true) : item(id, ok: false, missingPersonMessage))
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

    static func markPersonsDifferent(
        database: Database,
        sourcePersonID: Int64,
        destinationPersonID: Int64,
        sourceAnchorFaceID: Int64,
        destinationAnchorFaceID: Int64
    ) async -> BulkActionResult {
        do {
            try await database.pool.write { db in
                let requested = [
                    (personID: sourcePersonID, faceID: sourceAnchorFaceID),
                    (personID: destinationPersonID, faceID: destinationAnchorFaceID)
                ]
                var anchors: [(faceID: Int64, fileID: Int64, bbox: String)] = []
                for entry in requested {
                    guard let row = try Row.fetchOne(db, sql: """
                        SELECT person_id, file_id, bbox FROM face_prints WHERE id = ?
                        """, arguments: [entry.faceID]),
                          (row["person_id"] as Int64?) == entry.personID,
                          let fileID: Int64 = row["file_id"],
                          let bbox: String = row["bbox"] else {
                        throw NSError(
                            domain: "FileID.MarkPersonsDifferent",
                            code: 1,
                            userInfo: [NSLocalizedDescriptionKey:
                                "The suggested face pair is stale; refresh People and try again."]
                        )
                    }
                    anchors.append((entry.faceID, fileID, bbox))
                }
                anchors.sort { $0.faceID < $1.faceID }
                let personA = min(sourcePersonID, destinationPersonID)
                let personB = max(sourcePersonID, destinationPersonID)
                try db.execute(sql: """
                    INSERT OR REPLACE INTO face_verifications
                        (person_a, person_b, same_person, confidence, vlm_model, verified_at,
                         face_a, face_b, file_a, bbox_a, file_b, bbox_b)
                    VALUES (?, ?, 0, 1.0, 'user-verified', ?, ?, ?, ?, ?, ?, ?)
                    """, arguments: [
                        personA, personB, Date().timeIntervalSince1970,
                        anchors[0].faceID, anchors[1].faceID,
                        anchors[0].fileID, anchors[0].bbox,
                        anchors[1].fileID, anchors[1].bbox
                    ])
            }
            return BulkActionResult(
                action: "markPersonsDifferent", succeeded: 1, failed: 0,
                messages: [item(nil, ok: true)])
        } catch {
            return BulkActionResult(
                action: "markPersonsDifferent", succeeded: 0, failed: 1,
                messages: [item(nil, ok: false, error.localizedDescription)])
        }
    }

    static func wipeLibrary(database: Database) async -> LibraryWiped {
        do {
            // foreign_keys is a no-op inside a transaction, so toggle it outside one.
            try await database.pool.writeWithoutTransaction { db in
                try db.execute(sql: "PRAGMA foreign_keys = OFF")
                defer { try? db.execute(sql: "PRAGMA foreign_keys = ON") }
                try db.inTransaction {
                    let virtualRows = try Row.fetchAll(db, sql: "SELECT name, instr(lower(replace(sql,' ','')),'content=')>0 AS external FROM sqlite_master WHERE type='table' AND sql LIKE 'CREATE VIRTUAL TABLE%'")
                    let virtuals: [String] = virtualRows.map { $0["name"] }
                    let tables = try String.fetchAll(db, sql: """
                        SELECT name FROM sqlite_master
                        WHERE type='table' AND sql LIKE 'CREATE TABLE%'
                          AND name NOT LIKE 'sqlite_%' AND name <> 'grdb_migrations'
                          AND name <> 'catalog_vector_state'
                        """).filter { table in
                            !virtuals.contains { v in table.hasPrefix(v + "_") }
                        }
                    for table in tables {
                        try db.execute(sql: "DELETE FROM \"\(table.replacingOccurrences(of: "\"", with: "\"\""))\"")
                    }
                    // 'delete-all' exists only for contentless/external-content FTS5.
                    for row in virtualRows {
                        let q = (row["name"] as String).replacingOccurrences(of: "\"", with: "\"\"")
                        let external: Bool = row["external"]
                        try db.execute(sql: external ? "INSERT INTO \"\(q)\"(\"\(q)\") VALUES('delete-all')" : "DELETE FROM \"\(q)\"")
                    }
                    try? db.execute(sql: "DELETE FROM sqlite_sequence")
                    // The vector-change triggers and the HNSW cache need these seed rows;
                    // a fresh instance id invalidates any index cached before the wipe.
                    // Deleting tables fires the vector triggers, so clear their log last.
                    try db.execute(sql: "DELETE FROM catalog_vector_changes")
                    try db.execute(sql: "UPDATE catalog_vector_state SET instance_id=hex(randomblob(16)),generation=0")
                    try db.execute(sql: "UPDATE catalog_vector_state SET nonce=instance_id")
                    return .commit
                }
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
        let url = URL(fileURLWithPath: ScanRootGuard.canonicalPath(rootPath))

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

        let session = await coordinator.startSession(rootDisplayPath: (rootPath as NSString).lastPathComponent, epoch: epoch)
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
                let kind = url.path.hasPrefix("/Volumes/") ? "external" : "internal"
                try db.execute(sql: """
                    INSERT INTO library_roots(path,display,kind,added_at,last_status) VALUES(?,?,?,?,'running') ON CONFLICT(path) DO UPDATE SET last_status='running'
                    """, arguments: [url.path, url.path, kind, Date().timeIntervalSince1970])
            }
        } catch {
            JSONLog.shared.warn(ev: "scan_session_insert_failed", sess: session.id, error: "\(error)")
        }

        // A cancelScan that landed in the start window is preserved by the
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
        let changeMark = FileSystemChanges.currentEventID()
        let catalogHadRows = await CaptureDateBackfill.catalogHasRows(database: database, root: url)
        let datesCurrent = rescan || !catalogHadRows

        // Pre-warm both ANE-bound models on the main task before workers
        // start so all workers don't race the cold-start slow path
        // simultaneously. Each is a no-op if the model isn't installed.
        await Task.detached(priority: .userInitiated) {
            MobileCLIPService.shared.preWarm()
            // Loads Vision's classifier and face models once, instead of in
            // every worker on its first file.
            if let blank = CGContext(data: nil, width: 64, height: 64, bitsPerComponent: 8, bytesPerRow: 0,
                                     space: CGColorSpaceCreateDeviceRGB(),
                                     bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)?.makeImage() {
                _ = VisionWorker().runPrimaryPass(blank)
            }
            // RAM++ primary tagger (macOS lockstep) — no-op if not installed, in
            // which case Tagging falls back to the Vision scene classifier.
            RamPlusService.shared.preWarm()
            // The face embedder is not loaded here: the scan only stores face boxes, and
            // building its CoreML session delayed the first file by about 2 s.
            // Face clustering loads it when it runs.
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
                if let notice = Discovery.shortfallNotice(await discovery.lastShortfall) {
                    await sink.emit(.error(notice))
                }
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
                        // Cancellation while waiting for a resource or Vision
                        // worker stops this loop before another file is pulled.
                        let tagged: TaggedFile?
                        do {
                            tagged = try await Self.processScanFile(disc, pool: pool)
                        } catch is CancellationError {
                            break
                        } catch {
                            JSONLog.shared.warn(ev: "scan_worker_scheduler_failed", error: "\(error)")
                            break
                        }
                        guard let tagged else { break }
                        await taggedChan.send(tagged)
                    }
                }
            }
        }
        // All workers done — signal writer that no more results are coming.
        taggedChan.finish()
        await writerTask.value

        // Files added, renamed, edited or deleted while the scan ran: the walk
        // above never saw the new names, and rows of files deleted after they
        // were processed carry this scan's time, so the orphan sweep would keep
        // them. One incremental follow-up walk picks up the new and changed files
        // (a rename heals by inode) and stamps every file still present, so the
        // sweep below, keyed to the follow-up's start, drops the deleted ones.
        // Also retried here: photos whose Vision pass timed out, which happens while
        // a newly installed engine compiles Vision's Neural Engine models (first
        // scan after an update). The follow-up walk re-picks failed rows.
        var sweepStart = scanStart
        let timedOut = await Self.visionTimeouts(database: database, root: url, since: scanStart)
        if await !coordinator.isCancelled,
           timedOut > 0 || FileSystemChanges.changed(under: url, since: changeMark, ignoring: [Discovery.engineStatePrefix]) == true {
            sweepStart = Date()
            var changedFiles: [DiscoveredFile] = []
            _ = await discovery.walkStreaming(
                root: url, database: database, forceReprocess: false,
                excludedPaths: effectiveExcludedPaths,
                cancelCheck: { ScanCoordinator.isCancelledSync() }
            ) { changedFiles.append($0) }
            await coordinator.addFiles(changedFiles.count, retried: timedOut)
            let followUpChan = AsyncChannel<TaggedFile>()
            let followUpWriter = Task.detached(priority: .userInitiated) {
                await dbWriter.drain(followUpChan)
            }
            for start in stride(from: 0, to: changedFiles.count, by: workerCap) {
                if ScanCoordinator.isCancelledSync() { break }
                let chunk = changedFiles[start..<min(start + workerCap, changedFiles.count)]
                let tagged = await withTaskGroup(of: TaggedFile?.self) { group -> [TaggedFile] in
                    for file in chunk {
                        group.addTask { (try? await Self.processScanFile(file, pool: pool)) ?? nil }
                    }
                    var results: [TaggedFile] = []
                    for await result in group { if let result { results.append(result) } }
                    return results
                }
                for file in tagged { await followUpChan.send(file) }
            }
            followUpChan.finish()
            await followUpWriter.value
            JSONLog.shared.info(ev: "scan_followup", sess: session.id,
                                extra: ["files": AnyCodable(changedFiles.count), "visionRetries": AnyCodable(timedOut)])
        }

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
                                scanStart: sweepStart, sink: sink, sessionID: session.id)
            if datesCurrent { CaptureDateBackfill.markDone(root: url) } else {
                await CaptureDateBackfill.runIfNeeded(database: database, root: url,
                                                       isCancelled: { ScanCoordinator.isCancelledSync() })
            }
            await autoEnqueueFaceClusteringIfNeeded(database: database, sink: sink)
        }

        let totalDur = Date().timeIntervalSince(scanStart)
        await markSessionFinal(database: database, session: session,
                                coordinator: coordinator, sink: sink,
                                totalSeconds: totalDur)
    }

    static let visionTimeoutMessage = "Vision pass timed out (will retry next scan)"

    /// Photos under `root` whose Vision pass timed out during this scan.
    private static func visionTimeouts(database: Database, root: URL, since: Date) async -> Int {
        let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        return (try? await database.pool.read { db in
            try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM files WHERE failed = 1 AND error_message = ? AND scanned_at >= ?
                  AND substr(path_text, 1, ?) = ?
                """, arguments: [visionTimeoutMessage, since.timeIntervalSince1970, prefix.utf8.count, prefix])
        }) ?? 0
    }

    private static func processScanFile(
        _ discovered: DiscoveredFile,
        pool: VisionWorkerPool
    ) async throws -> TaggedFile? {
        try await ScanTaggingAdmission.withBackgroundCPU(
            isCancelled: {
                ScanCoordinator.isCancelledSync() || ScanCoordinator.isShuttingDownSync()
            }
        ) { admissionWaitMs in
            guard var tagged = await pool.with({ worker in
                await Tagging.processFile(discovered: discovered, worker: worker)
            }) else { return nil }
            tagged.admissionWaitMs = admissionWaitMs
            return tagged
        }
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
        guard ScanRootGuard.isReachable(scanRootPath) else {
            JSONLog.shared.warn(ev: "orphan_sweep_skipped_root_unreachable", sess: sessionID,
                                error: "scan root missing, empty or unmounted; \(candidates.count) rows kept")
            return
        }

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
                // Needles SQLite cannot match on its own — see the fallback below.
                var unicodeNeedles: [(exact: String, prefix: String)] = []
                for excluded in excludedPaths {
                    let prefix = excluded.hasSuffix("/") ? excluded : excluded + "/"
                    let upper = prefixUpperBound(prefix)
                    if excluded.contains(where: { !$0.isASCII }) {
                        unicodeNeedles.append((exact: excluded, prefix: prefix))
                    }
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

                // The query above delegates case folding to SQLite's built-in
                // lower(), which folds ASCII A-Z only — no ICU extension is
                // loaded (Database.swift). The needle, by contrast, was folded by
                // Swift over the full Unicode range (normalizeExcludedPaths /
                // Discovery.normalizedExclusionPath). For a path holding a
                // non-ASCII uppercase letter — "Fotos/Ärchiv", "Документы" — the
                // needle reads "ärchiv" while lower(path_search) still reads
                // "Ärchiv", so the equality misses AND the BINARY range bound
                // misses at the first differing byte. The purge then deleted
                // nothing and reported success, leaving every already-indexed
                // file under an excluded folder — with its tags, faces and OCR —
                // visible in the library forever.
                //
                // Only such needles pay for this pass, so ASCII libraries keep
                // the indexed query and scan nothing extra.
                if !unicodeNeedles.isEmpty {
                    let rows = try Row.fetchAll(
                        db, sql: "SELECT id, path_search FROM files WHERE path_search IS NOT NULL")
                    for row in rows {
                        guard let stored: String = row["path_search"],
                            let id: Int64 = row["id"]
                        else { continue }
                        let folded = stored.precomposedStringWithCanonicalMapping.lowercased()
                        if unicodeNeedles.contains(where: {
                            folded == $0.exact || folded.hasPrefix($0.prefix)
                        }) {
                            ids.insert(id)
                        }
                    }
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
            var normalized = ScanRootGuard.canonicalPath(trimmed)
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
    /// plus full-plan confidence totals. Categories sort by descending count,
    /// then category for a stable order. Per-move `tier`
    /// (Anchor/Mixed/Junk) and the rolled-up `folderClassifications` are derived
    /// from `Restructure.classifyFolders` so the app renders the
    /// engine-authoritative Tidy/Keep tiles instead of its local heuristic
    /// fallback (the "null on older engines" path).
    static func restructurePlan(
        from plan: Restructure.PlanResult, libraryRoot: String
    ) -> RestructurePlan {
        // The tier map + Keep/Tidy/Junk counts are engine-authoritative and already
        // computed by `proposeAll` on the FULL pre-strip set with the semantic-claim
        // exemption — recomputing them here on the stripped `proposals`
        // would undercount the "Keep" tile (the stripped anchor folders are gone) and
        // diverge from the Windows engine. This mapper just stamps each surviving
        // move's tier by its source-folder parent. (lockstep)
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
        let confidenceCounts = Restructure.confidenceCounts(
            plan.proposals.lazy.map(\.confidence))
        return RestructurePlan(
            libraryRoot: libraryRoot, moves: moves,
            categoryCounts: categoryCounts,
            folderClassifications: FolderClassificationCounts(
                anchorFolders: plan.anchorFolders, mixedFolders: plan.mixedFolders,
                junkFolders: plan.junkFolders),
            confidenceCounts: confidenceCounts)
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
        // Shield the terminal write from the cancellation.
        let status = cancelled ? "cancelled" : "completed"
        let completedAt = Date().timeIntervalSince1970
        let sessionID = session.id
        do {
            try await database.writeUncancellable { db in
                try db.execute(sql: """
                    UPDATE scan_sessions SET status = ?, completed_at = ?
                    WHERE id = ?
                    """, arguments: [status, completedAt, sessionID])
                try db.execute(sql: """
                    UPDATE library_roots SET last_scan_at=?,last_status=? WHERE path=(SELECT root_path FROM scan_sessions WHERE id=?)
                    """, arguments: [completedAt, status, sessionID])
            }
        } catch {
            JSONLog.shared.warn(ev: "scan_session_update_failed",
                                sess: session.id, error: "\(error)")
        }
        await coordinator.setPhase(finalPhase)
        if let terminalProgress = await coordinator.snapshot() {
            await sink.emit(.progress(terminalProgress))
        }
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
