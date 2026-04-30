// FileIDEngine — `fileidd`
//
// Spawned as a child process by the FileID SwiftUI app. Reads commands from
// stdin (newline-delimited JSON), writes events to stdout (same).
//
// Lifetime: bound to the parent process. When the app exits, the pipe closes,
// the LineReader stream finishes, the engine exits cleanly.
//
// Milestone 1 scope: discovery only. No tagging, no DB, no embeddings.
// Future milestones add Stage B (tagging), Stage C (DB writer), Stage D (post-scan).
import Foundation
import Darwin
import FileIDShared
import AsyncAlgorithms
import GRDB

@main
struct FileIDEngineMain {
    static func main() async {
        // Ignore SIGPIPE — writes to a closed parent stdout shouldn't crash
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
            version: "0.1.0-m1",
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

        // Command loop. Blocks reading stdin via LineReader; for each command,
        // dispatches in a detached task so long-running scans don't block
        // subsequent commands (pause/cancel must be responsive).
        let stdin = FileHandle.standardInput
        let commands = LineReader.read(from: stdin, as: IPCCommand.self)
        do {
            for try await cmd in commands {
                await dispatch(cmd, coordinator: coordinator, sink: sink, database: database)
                if case .shutdown = cmd.payload { break }
            }
        } catch {
            JSONLog.shared.error(ev: "command_stream_error", error: "\(error)")
            await sink.emit(.error(EngineError(kind: "ipc_failed", message: "\(error)")))
        }

        // Wait for any in-flight scan to finish before exiting. Without this,
        // the detached scan task would be killed when main returns, stranding
        // the parent without scanComplete events. If shutdown was requested,
        // the scan should have observed coordinator.isCancelled and bailed
        // quickly; if stdin EOF'd because the parent died, we just wait it out.
        await coordinator.awaitActiveScan()
        progressTicker.cancel()
        JSONLog.shared.info(ev: "engine_exit")
        JSONLog.shared.flush()

        // Hard-exit. MLX's static destructors SEGV during normal atexit
        // teardown on macOS 26; `_exit` skips them. Our work is already
        // flushed to disk above.
        Darwin._exit(0)
    }

    /// Per-command dispatcher. `startScan` runs the scan in a detached task so
    /// the command loop stays responsive to subsequent pause/cancel commands.
    static func dispatch(_ cmd: IPCCommand, coordinator: ScanCoordinator,
                          sink: IPCSink, database: Database?) async {
        switch cmd.payload {
        case .startScan(let bookmark, let displayPath):
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot scan."
                )))
                return
            }
            // Enqueue — runs immediately if nothing else queued, else
            // waits for predecessors to finish.
            let title = "Scan \((displayPath as NSString).lastPathComponent)"
            await JobQueue.shared.enqueue(.init(
                category: .scan,
                title: title,
                etaSeconds: nil  // unknown until discovery completes
            ) {
                let task = Task.detached(priority: .userInitiated) {
                    await runScan(bookmark: bookmark, displayPath: displayPath,
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
        case .runFaceClustering:
            guard let database else {
                await sink.emit(.error(EngineError(
                    kind: "db_unavailable",
                    message: "Database failed to open at engine startup; cannot cluster faces."
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
        case .deepAnalyzeAll(let modelKind, let skipExisting):
            guard let database, let kind = AIModelKind(rawValue: modelKind) else {
                await sink.emit(.error(EngineError(
                    kind: "deep_invalid",
                    message: "Database unavailable or unknown model kind \(modelKind)."
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
                                             modelKind: kind)
            })
        case .deepAnalyzeCancel:
            await DeepAnalyze.shared.requestCancel()
            JSONLog.shared.info(ev: "deep_analyze_cancel_requested")
        }
    }

    /// Resolve the security-scoped bookmark and run discovery.
    /// `database` is the engine's single shared `Database` (one DatabasePool
    /// per engine process — opening more would trigger SQLITE_BUSY).
    static func runScan(
        bookmark: Data, displayPath: String,
        coordinator: ScanCoordinator, sink: IPCSink,
        database: Database
    ) async {
        // Bookmark might or might not carry security scope depending on
        // whether the app is sandboxed. Try with scope first (production
        // path), then without (tests / unsandboxed dev runs). Both fail =
        // genuine error.
        var stale = false
        let url: URL
        do {
            url = try URL(
                resolvingBookmarkData: bookmark,
                options: [.withSecurityScope],
                relativeTo: nil,
                bookmarkDataIsStale: &stale
            )
        } catch {
            do {
                url = try URL(
                    resolvingBookmarkData: bookmark,
                    options: [],
                    relativeTo: nil,
                    bookmarkDataIsStale: &stale
                )
            } catch let secondError {
                await sink.emit(.error(EngineError(
                    kind: "bookmark_invalid",
                    message: "Could not resolve bookmark for \(displayPath): \(error) / fallback: \(secondError)"
                )))
                JSONLog.shared.error(ev: "bookmark_invalid", path: redactPathForLog(displayPath),
                                     error: "withScope=\(error) noScope=\(secondError)")
                return
            }
        }
        if stale {
            JSONLog.shared.warn(ev: "bookmark_stale", path: redactPathForLog(url.path))
        }
        // Security-scoped access only matters inside an app sandbox (where
        // the bookmark MUST carry scope, granted via NSOpenPanel). Outside
        // the sandbox (CLI dev runs, tests) this returns false even for valid
        // URLs because there's nothing to scope. Log but proceed.
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

        let session = await coordinator.startSession(rootDisplayPath: url.lastPathComponent)
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
                    """, arguments: [session.id, url.path, Date().timeIntervalSinceReferenceDate])
            }
        } catch {
            JSONLog.shared.warn(ev: "scan_session_insert_failed", sess: session.id, error: "\(error)")
        }

        // Stage A — Discovery. Walk the tree, sort by path for I/O locality.
        // cancelCheck reads the cancel mirror (sync, no actor hop) so the
        // enumerator loop bails out immediately on Cancel — the v1-deferred
        // "cancel during discovery" gap is now closed.
        let discovery = Discovery()
        let scanStart = Date()
        let files = await discovery.walk(
            root: url,
            cancelCheck: { ScanCoordinator.isCancelledSync() },
            progress: { count in
                Task { await coordinator.bumpDiscovered(to: count) }
            }
        )
        let discoveryDur = Date().timeIntervalSince(scanStart)
        await coordinator.bumpDiscovered(to: files.count)
        await coordinator.setTotal(files.count)
        JSONLog.shared.info(ev: "discovery_complete", sess: session.id,
                            extra: ["files": AnyCodable(files.count),
                                    "seconds": AnyCodable(discoveryDur),
                                    "ratePerSec": AnyCodable(discoveryDur > 0 ? Double(files.count) / discoveryDur : 0)])
        await sink.emit(.discoveryComplete(totalFiles: files.count))
        // If the user cancelled during discovery, don't proceed to tagging.
        if await coordinator.isCancelled {
            JSONLog.shared.info(ev: "scan_cancelled_during_discovery", sess: session.id)
            await markSessionFinal(database: database, session: session,
                                    coordinator: coordinator, sink: sink,
                                    totalSeconds: discoveryDur)
            return
        }
        if files.isEmpty {
            await markSessionFinal(database: database, session: session,
                                    coordinator: coordinator, sink: sink,
                                    totalSeconds: discoveryDur)
            return
        }
        await sink.emit(.phaseChanged(.tagging))

        // Pre-warm both ANE-bound models on the main task before workers
        // start so all workers don't race the cold-start slow path
        // simultaneously. Each is a no-op if the model isn't installed.
        await Task.detached(priority: .userInitiated) {
            MobileCLIPService.shared.preWarm()
            // Pick whichever ArcFace variant the user has on disk —
            // iResNet50 takes precedence when both are present.
            for kind in FaceEmbedderKind.installedKinds() {
                ArcFaceService.shared.preWarm(kind)
                break
            }
        }.value

        // Bounded async channels — discovery 1024 (matches batch size),
        // tagged 256 (DBWriter drains in batches of 100 with headroom).
        let discoveryChan = AsyncChannel<DiscoveredFile>()
        let taggedChan    = AsyncChannel<TaggedFile>()
        let workerCap     = Hardware.workerCap
        let pool          = VisionWorkerPool(count: workerCap)
        let dbWriter      = DBWriter(db: database, sink: sink,
                                     coordinator: coordinator, sessionID: session.id)

        // DBWriter task — runs in parallel with tagging. Drains taggedChan
        // until it finishes, then exits.
        let writerTask = Task.detached(priority: .userInitiated) {
            await dbWriter.drain(taggedChan)
        }

        // Producer + N workers in one TaskGroup so we know when all workers
        // finish (and can then close taggedChan to signal EOF to writer).
        await withTaskGroup(of: Void.self) { group in
            // Producer: feed all discovered files into the channel.
            group.addTask {
                for file in files {
                    await discoveryChan.send(file)
                }
                discoveryChan.finish()
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
                        let tagged = await pool.with { worker in
                            await Tagging.processFile(discovered: disc, worker: worker)
                        }
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
        let cap = 5000
        let candidates: [CandidateRow]
        do {
            candidates = try await database.pool.read { db in
                let rows = try GRDB.Row.fetchAll(db, sql: """
                    SELECT id, path_text FROM files
                    WHERE (path_text = ? OR path_text LIKE ?)
                      AND scanned_at < ?
                    LIMIT \(cap)
                    """, arguments: [
                        scanRootPath,
                        prefix + "%",
                        scanStart.timeIntervalSinceReferenceDate
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
                // Chunk the IN clause to keep the SQL string + bound vars sane.
                for chunk in stride(from: 0, to: missing.count, by: 200).map({
                    Array(missing[$0..<min($0 + 200, missing.count)])
                }) {
                    let placeholders = chunk.map { _ in "?" }.joined(separator: ",")
                    try db.execute(
                        sql: "DELETE FROM files WHERE id IN (\(placeholders))",
                        arguments: StatementArguments(chunk.map { Int($0) })
                    )
                }
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
                    """, arguments: [Date().timeIntervalSinceReferenceDate])
            }
            for row in crashed {
                JSONLog.shared.warn(
                    ev: "crash_recovery_detected",
                    sess: row.id,
                    path: row.rootPath,
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
        do {
            try await database.pool.write { db in
                try db.execute(sql: """
                    UPDATE scan_sessions SET status = ?, completed_at = ?
                    WHERE id = ?
                    """, arguments: [
                        cancelled ? "cancelled" : "completed",
                        Date().timeIntervalSinceReferenceDate,
                        session.id
                    ])
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
