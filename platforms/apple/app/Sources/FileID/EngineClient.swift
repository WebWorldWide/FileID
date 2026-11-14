// Spawns + supervises the engine child process. Auto-respawns with
// backoff. State and events are observable on MainActor.
import Foundation
import Security
import AppKit
import FileIDShared

@MainActor
@Observable
public final class EngineClient {
    public enum ConnectionState {
        case starting
        case ready(EngineInfo)
        case crashed(reason: String)
    }

    public private(set) var state: ConnectionState = .starting
    public private(set) var lastProgress: ScanProgress?
    public private(set) var lastError: EngineError? {
        didSet { lastErrorSignal &+= 1 }
    }
    /// Bumps on every `lastError` write, including a repeat of an identical
    /// message. EngineError isn't Equatable, so views key error-recovery
    /// `.onChange` on this monotonic counter rather than `lastError?.message`
    /// — two consecutive identical failures must still re-fire the handler.
    public private(set) var lastErrorSignal: Int = 0
    public private(set) var latestChatResponse: ChatResponse?
    public private(set) var toolResponses: [String: ToolResponse] = [:]
    private var toolResponseOrder: [String] = []
    private var catalogResponseOrder: [String] = []
    public private(set) var catalogResponses: [String: CatalogResponse] = [:]
    public private(set) var catalogJobs: [CatalogJob] = []
    public func searchCatalog(query: String? = nil, vector: [Float]? = nil, seedID: Int64? = nil, limit: Int = 60) async -> CatalogResponse? {
        for _ in 0..<120 {
            guard !Task.isCancelled else { return nil }
            let id = UUID().uuidString
            let request = CatalogRequest(requestID: id, action: "search", query: query, fileID: seedID,
                                         searchMode: seedID == nil ? "hybrid" : "semantic", queryVector: vector,
                                         embeddingModel: vector == nil ? nil : CLIPEmbeddingSpace.modelID, limit: limit, resultScope: "files")
            guard send(.catalogRequest(request: request)),
                  let response = await catalogResponse(for: id, timeout: .seconds(10)) else { return nil }
            if response.status != "indexing" { return response }
            do { try await Task.sleep(for: .seconds(1)) } catch { return nil }
        }
        return nil
    }

    private var catalogWaiters: [String: CheckedContinuation<CatalogResponse?, Never>] = [:]

    /// Sends `request` and waits for its reply; nil when it can't be sent,
    /// on timeout, on cancellation or when the engine stops.
    func catalogReply(to request: CatalogRequest, timeout: Duration) async -> CatalogResponse? {
        guard send(.catalogRequest(request: request)) else { return nil }
        return await catalogResponse(for: request.requestID, timeout: timeout)
    }

    /// Resumes when the engine replies to `id`; nil on timeout, cancellation or engine exit.
    func catalogResponse(for id: String, timeout: Duration) async -> CatalogResponse? {
        let timer = Task { [weak self] in
            try? await Task.sleep(for: timeout)
            guard !Task.isCancelled else { return }
            self?.resumeCatalogWaiter(id, with: nil)
        }
        defer { timer.cancel() }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                if let response = catalogResponses[id] {
                    continuation.resume(returning: response)
                } else if Task.isCancelled {
                    continuation.resume(returning: nil)
                } else {
                    catalogWaiters[id] = continuation
                }
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.resumeCatalogWaiter(id, with: nil) }
        }
    }

    private func resumeCatalogWaiter(_ id: String, with response: CatalogResponse?) {
        catalogWaiters.removeValue(forKey: id)?.resume(returning: response)
    }

    private var bulkResultWaiters: [UUID: (prefix: String, continuation: CheckedContinuation<BulkActionResult?, Never>)] = [:]

    /// The first bulk result after `signal` whose action starts with `prefix`;
    /// nil on timeout or engine exit. A cancelled caller still gets the result:
    /// the files were already moved, and the undo needs the batch it names.
    func bulkActionResult(after signal: Int, prefix: String, timeout: Duration) async -> BulkActionResult? {
        if bulkActionResultSignal != signal, let result = bulkActionResult, result.action.hasPrefix(prefix) {
            return result
        }
        let token = UUID()
        let timer = Task { [weak self] in
            try? await Task.sleep(for: timeout)
            guard !Task.isCancelled else { return }
            self?.bulkResultWaiters.removeValue(forKey: token)?.continuation.resume(returning: nil)
        }
        defer { timer.cancel() }
        return await withCheckedContinuation { continuation in
            bulkResultWaiters[token] = (prefix, continuation)
        }
    }

    private func resumeBulkResultWaiters(with result: BulkActionResult?) {
        for (token, waiter) in bulkResultWaiters where result.map({ $0.action.hasPrefix(waiter.prefix) }) ?? true {
            bulkResultWaiters.removeValue(forKey: token)
            waiter.continuation.resume(returning: result)
        }
    }

    private let queryReplies = EngineQueryReplies()

    /// Nil when the engine is down, fails, restarts or doesn't answer in time.
    public func findMergeSuggestions() async -> [MergeSuggestionPair]? {
        await queryReplies.mergeSuggestions { send(.findMergeSuggestions) }
    }

    /// One face review reply (list, resolve or undo); nil when the engine is down or doesn't answer in time.
    public func faceReview(_ command: (String) -> IPCCommand.Payload) async -> FaceReviewPage? {
        await queryReplies.faceReview { send(command($0)) }
    }

    /// Nil when the engine has no CLIP text encoder or can't answer.
    public func embedTextQuery(_ query: String) async -> [Float]? {
        await queryReplies.textEmbedding { send(.embedTextQuery(query: query, queryID: $0)) }
    }

    /// JPEG bytes with a `maxPixelSize` long side (clamped to 64–2048, never
    /// upscaled), or nil when the engine is down or can't decode the video.
    public func generateVideoThumbnail(path: String, modifiedAt: Double?, maxPixelSize: Int = 192) async -> Data? {
        let size = min(max(maxPixelSize, 64), 2048)
        return await queryReplies.videoThumbnail(path: path, modifiedAt: modifiedAt, maxPixelSize: size) {
            send(.generateVideoThumbnail(path: path, modifiedAt: modifiedAt, maxPixelSize: size))
        }
    }

    func applyCatalogJobsSnapshot(_ jobs: [CatalogJob]) { catalogJobs = jobs }
    public private(set) var lastBatch: BatchSummary?
    public private(set) var lastFaceClustering: FaceClusteringResult?
    public private(set) var faceClusteringInFlight: Bool = false
    /// Engine doesn't echo paused state, so the app mirrors it locally.
    public private(set) var isPaused: Bool = false

    public private(set) var deepAnalyzeProgress: DeepAnalyzeProgress?
    public private(set) var deepAnalyzeLast: DeepAnalyzeFileDone?
    public private(set) var deepAnalyzeComplete: DeepAnalyzeComplete?
    public private(set) var modelDownloadProgress: ModelDownloadProgress?
    public private(set) var benchmarkProgress: BenchmarkProgress?
    public private(set) var libraryRoots: [LibraryRoot] = []
    public private(set) var benchmarkResults: [String: ModelBenchmark] = [:]
    public private(set) var benchmarkMessage: (modelKind: String, text: String)?
    public private(set) var benchmarkModelKind: String?
    private var benchmarkRequestID: String?
    public private(set) var deepAnalyzeInFlight: Bool = false
    /// Streamed by the engine between command-receipt and the first
    /// per-file `deepAnalyzeProgress`, so the UI can show progressive
    /// labelling during the ~10s VLM cold-load window. Cleared when the
    /// first progress event arrives or the run completes.
    public private(set) var deepAnalyzeStarting: DeepAnalyzeStarting?
    /// False when engine reports mlx.metallib is missing — Deep Analyze
    /// would crash on first VLM call. UI should disable + explain.
    public private(set) var deepAnalyzeAvailable: Bool = true
    public private(set) var deepAnalyzeUnavailableReason: String?

    /// Bumped on every engine exit (crash or clean). A dead engine can't
    /// emit the terminal events that clear a view's *own* in-flight UI
    /// (e.g. Deep Analyze's StreamCard + Cancel), so it would freeze until
    /// a tab switch. Views observe this via `.onChange` to reset that
    /// local state. EngineClient's own published flags are already reset
    /// in `handleEngineExit`; this is the signal for state the client
    /// doesn't own.
    public private(set) var engineResetSignal: Int = 0

    public private(set) var queueState: QueueState = QueueState(
        running: nil, pending: [], totalEtaSeconds: nil
    )

    // MARK: - Restructure butler (engine-routed plan + apply)
    //
    // The macOS Restructure tab drives its plan + apply through
    // the engine butler instead of the app-side classifier. These mirror the
    // published-state pattern above; the `*Signal` counters bump on every fresh
    // payload so views can react via `.onChange` even though the DTOs aren't
    // Equatable (same idiom as `engineResetSignal`).
    public private(set) var restructurePlan: RestructurePlan?
    public private(set) var restructureApplyResult: RestructureApplyResult?
    public private(set) var restructurePlanSignal: Int = 0
    public private(set) var restructureApplyResultSignal: Int = 0
    public private(set) var bulkActionResult: BulkActionResult?
    public private(set) var bulkActionResultSignal: Int = 0
    /// True once an applyRestructure has moved files and they haven't been undone
    /// yet — drives the "Undo last run" affordance.
    public private(set) var canUndoRestructure = false
    private var undoRestructureInFlight = false

    private var process: Process?
    private var stdinPipe: Pipe?
    /// Serial queue for stdin command writes. The global CONCURRENT queue used
    /// to let two rapid send() calls write to the engine's stdin fd at once,
    /// which could reorder commands or interleave their bytes mid-line.
    private let stdinWriteQueue = DispatchQueue(label: "com.fileid.engine.stdin")

    // MARK: - App Nap protection
    //
    // The old app-lifetime beginActivity token (removed in AppDelegate) kept
    // the UI process from being App-Napped — throttled timers / coalesced
    // updates — during a long scan or Deep Analyze run. Replaced with
    // operation-scoped tokens so the app isn't held un-nappable while idle.
    // (System idle-sleep during scans is handled separately by the engine's
    // SleepGuard; this is purely App Nap for the UI process.) Each token is
    // begun when its operation starts and ended on EVERY terminal path —
    // completion, cancel, failure, and engine crash/exit — through the guarded
    // helpers below (begin no-ops if already held, end no-ops if already
    // cleared), so the pair can never leak or double-release.
    private var scanActivityToken: NSObjectProtocol?
    private var deepAnalyzeActivityToken: NSObjectProtocol?

    // Up to 3 respawns within respawnWindow; cleared on .ready.
    private static let respawnDelays: [UInt64] = [1, 4, 16]
    private static let respawnWindow: TimeInterval = 60
    private var respawnAttempts: [Date] = []
    // When the engine last reached Ready. The respawn budget is cleared
    // only after it has been continuously Ready for `stabilitySettle` (a real
    // recovery) — not on every Ready, which would let a Ready→crash flap reset
    // the budget forever.
    private var lastReadyAt: Date = .distantPast
    private static let stabilitySettle: TimeInterval = 30
    private var pendingRespawn: Task<Void, Never>?

    /// Set on shutdown() or after a "work complete" signal. Suppresses
    /// the phantom error pill after MLX's known SIGSEGV-at-exit bug.
    private var expectedExit: Bool = false
    public private(set) var lastTerminalEventAt: Date = .distantPast

    /// When non-nil, the next engine exit deletes the SQLite library
    /// before the respawn, and the next `.ready` event auto-starts a
    /// scan against this URL. Drives `wipeAndRescan(rootURL:)`.
    private var pendingWipeAndRescanRoot: URL?

    /// 2 Hz throttle on `deepAnalyzeLast`; otherwise SwiftUI's
    /// AttributeGraph overflows on a fast Deep Analyze run.
    private var lastDeepAnalyzeFileDoneAt: Date = .distantPast

    // Every decoded event is funnelled through ONE serial AsyncStream
    // consumed by a single long-lived @MainActor task, so handleEvent runs in
    // strict receipt order. The previous per-event `Task { @MainActor }` dispatch
    // had no ordering guarantee, so events could be applied out of order — e.g.
    // wedging the Deep Analyze / face-clustering in-flight flag.
    private let eventContinuation: AsyncStream<InboundEvent>.Continuation
    private var eventPump: Task<Void, Never>?

    public convenience init() {
        self.init(launch: .appBundle())
    }

    init(launch: LaunchContext) {
        self.launch = launch
        let (stream, continuation) = AsyncStream.makeStream(of: InboundEvent.self)
        eventContinuation = continuation
        eventPump = Task { @MainActor [weak self] in
            for await inbound in stream {
                self?.handleEvent(inbound.event, videoFrame: inbound.videoFrame)
            }
        }
    }

    /// Where the engine binaries live and how each is vetted before it starts.
    /// The app uses its own bundle; tests point it at a folder of stand-ins.
    struct LaunchContext {
        var directory: URL
        var environment: [String: String]
        var defaults: UserDefaults
        var integrityFailure: (URL) -> String?

        static func appBundle() -> LaunchContext {
            let exec = Bundle.main.executablePath ?? CommandLine.arguments[0]
            return LaunchContext(directory: URL(fileURLWithPath: exec).deletingLastPathComponent(),
                                 environment: ProcessInfo.processInfo.environment,
                                 defaults: .standard,
                                 integrityFailure: EngineClient.engineIntegrityFailure(binary:))
        }

        func binary(for backend: EngineBackend) -> URL {
            directory.appendingPathComponent(backend.binaryName)
        }

        func isBundled(_ backend: EngineBackend) -> Bool {
            FileManager.default.isExecutableFile(atPath: binary(for: backend).path)
        }
    }

    private let launch: LaunchContext

    /// The engine the live process runs; nil while none is running.
    public private(set) var runningBackend: EngineBackend?
    /// Engines that failed to start this session, and the note Settings shows.
    public private(set) var fallback = EngineFallback()
    private var runningReachedReady = false

    public var rustEngineBundled: Bool { launch.isBundled(.rust) }

    /// The engine Settings ▸ Engine shows as chosen (before any fallback).
    public var preferredBackend: EngineBackend {
        EngineBackendPolicy.preferred(environment: launch.environment, defaults: launch.defaults,
                                      rustBundled: rustEngineBundled)
    }

    /// Settings ▸ Engine: remember the choice and restart on it. A new choice
    /// also forgets this session's fallback, so choosing Rust again retries it.
    public func switchBackend(to backend: EngineBackend) {
        launch.defaults.set(backend.rawValue, forKey: EngineBackendPolicy.defaultsKey)
        fallback = EngineFallback()
        start()
    }

    nonisolated static let damagedCopyReason = "This copy of FileID is damaged. Reinstall it."

    public func start() {
        // A live engine here means this is a restart (Settings ▸ Restart
        // Engine calls start() directly). Terminate + reap it FIRST —
        // two live engines = two SQLite writers against one DB =
        // corruption. Single-flight stop-then-start.
        terminateRunningEngine()
        let candidates = EngineBackendPolicy.candidates(environment: launch.environment, defaults: launch.defaults,
                                                        rustBundled: rustEngineBundled, demoted: fallback.demoted)
        var failure = Self.damagedCopyReason
        for backend in candidates {
            let binURL = launch.binary(for: backend)
            guard launch.isBundled(backend) else {
                Self.debug("start: \(backend.rawValue) engine binary not found next to app executable")
                fallback.launchFailed(backend, reason: "it isn't in this copy of FileID")
                continue
            }
            // Refuse to spawn an engine binary that didn't ship with this
            // app. Prevents a malicious process from dropping a payload at
            // FileID.app/Contents/MacOS/FileIDEngine and getting full FS
            // access via IPC. In dev (ad-hoc signing) and notarized
            // builds (Developer ID), both binaries share a signing
            // identity — we require it to match the app's.
            if let reason = launch.integrityFailure(binURL) {
                Self.debug("start: \(reason)")
                failure = Self.damagedCopyReason
                fallback.launchFailed(backend, reason: "its signature doesn't match the app")
                continue
            }
            if let reason = spawn(binary: binURL, backend: backend) {
                failure = reason
                fallback.launchFailed(backend, reason: "it couldn't be launched")
                continue
            }
            return
        }
        state = .crashed(reason: failure)
    }

    /// Stop half of a restart: cancel any pending respawn and
    /// synchronously terminate + reap the running engine, so a restart
    /// never overlaps two live engines against the single-writer SQLite
    /// DB. The bounded reap releases the WAL lock before the next spawn;
    /// the old process's pipe handlers are left armed (so it keeps
    /// draining and can't deadlock on a full pipe during shutdown) — its
    /// late EOF is harmless because `handleEngineExit(for:)` rejects any
    /// process that is no longer `self.process`. Idempotent.
    private func terminateRunningEngine() {
        pendingRespawn?.cancel()
        pendingRespawn = nil
        if let proc = process, proc.isRunning {
            proc.terminate()
            // Bounded reap, not an unbounded waitUntilExit() on the
            // @MainActor: SIGTERM drops the WAL lock in well under a second
            // (the engine installs no handler), but a wedged/uninterruptible
            // engine must never hang the UI. Poll, then escalate to SIGKILL.
            if !Self.waitForExit(proc, timeout: 3.0) {
                kill(proc.processIdentifier, SIGKILL)
                _ = Self.waitForExit(proc, timeout: 2.0)
            }
        }
        process = nil
        stdinPipe = nil
        runningBackend = nil

        // `handleEngineExit(for:)` deliberately rejects this process the moment
        // it stops being `self.process`, so a restart NEVER reaches the reset
        // there — `start()` reassigns `self.process` in `spawn()` before the old
        // pipe's EOF lands. Without this call, Settings ▸ Restart Engine during a
        // job left `deepAnalyzeInFlight` (and the undo/queue/App-Nap state)
        // latched against a jobless engine, disabling Analyze until relaunch.
        resetStateOnlyADeadEngineCouldClear()
        // A restart is an explicit "start over", so drop the frozen scan
        // progress too: the sidebar scan card keys off a non-idle phase and
        // would otherwise keep rendering a dead Pause/Stop pair with no
        // reachable Start. The crash leg deliberately keeps its last progress as
        // forensic context — only this explicit path clears it.
        lastProgress = nil
    }

    /// Bounded wait for a child to exit; true if it exited within `timeout`.
    /// Polls `isRunning` (flipped by Foundation's background reaper, so it
    /// updates even while this runs on the main actor) instead of the
    /// timeout-free `waitUntilExit()`, which can hang indefinitely.
    private static func waitForExit(_ proc: Process, timeout: TimeInterval) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while proc.isRunning {
            if Date() >= deadline { return false }
            usleep(20_000)
        }
        return true
    }

    /// Returns a non-nil failure reason when the engine binary
    /// shouldn't be spawned. Two checks are mandatory:
    ///
    ///   1. The engine path resolves inside the running app bundle's
    ///      `Contents/MacOS/`. This blocks the "drop a payload at
    ///      FileID.app/Contents/MacOS/FileIDEngine" attack at the
    ///      symlink level too — symlinks that escape the bundle fail.
    ///   2. The engine's signing identity (Team ID for Developer ID
    ///      builds, or both being unsigned/ad-hoc for dev builds)
    ///      matches the app's. Each binary gets its own cdhash so a
    ///      strict designated-requirement match against the app
    ///      never works for dev — we compare team identifiers
    ///      instead, which is what realistically catches a swapped
    ///      binary signed by a different developer.
    nonisolated private static func engineIntegrityFailure(binary: URL) -> String? {
        let resolved = binary.resolvingSymlinksInPath().standardizedFileURL
        let bundleMacOS = (Bundle.main.executableURL ?? URL(fileURLWithPath: ""))
            .resolvingSymlinksInPath()
            .deletingLastPathComponent()
            .standardizedFileURL
        guard resolved.path.hasPrefix(bundleMacOS.path + "/") else {
            return "Engine binary outside app bundle: \(resolved.lastPathComponent)"
        }

        let appTeam = appTeamIdentifier()
        let engineTeam = teamIdentifier(forBinaryAt: resolved)

        // Both signed by the same Team ID — Developer ID release path.
        if let a = appTeam, let e = engineTeam, a == e {
            return nil
        }
        // Both ad-hoc / unsigned — dev path (`bash run.sh`). Path
        // containment above is the only realistic guarantee here, and
        // an attacker who can write inside Contents/MacOS/ already has
        // enough access to swap the app itself.
        if appTeam == nil && engineTeam == nil {
            return nil
        }
        return "Engine signing identity does not match app (engine: \(engineTeam ?? "<unsigned>"), app: \(appTeam ?? "<unsigned>"))"
    }

    /// Team Identifier of the running app, or nil if ad-hoc / unsigned.
    nonisolated private static func appTeamIdentifier() -> String? {
        var appCode: SecCode?
        guard SecCodeCopySelf([], &appCode) == errSecSuccess,
              let appCodeUnwrapped = appCode else { return nil }
        var appStatic: SecStaticCode?
        guard SecCodeCopyStaticCode(appCodeUnwrapped, [], &appStatic) == errSecSuccess,
              let appStaticUnwrapped = appStatic else { return nil }
        return teamIdentifier(of: appStaticUnwrapped)
    }

    nonisolated private static func teamIdentifier(forBinaryAt url: URL) -> String? {
        var staticCode: SecStaticCode?
        guard SecStaticCodeCreateWithPath(url as CFURL, [], &staticCode) == errSecSuccess,
              let s = staticCode else { return nil }
        return teamIdentifier(of: s)
    }

    nonisolated private static func teamIdentifier(of code: SecStaticCode) -> String? {
        var info: CFDictionary?
        guard SecCodeCopySigningInformation(code, [], &info) == errSecSuccess,
              let dict = info as? [String: Any] else { return nil }
        return dict[kSecCodeInfoTeamIdentifier as String] as? String
    }

    /// Nil once the engine is running, else why it couldn't be launched.
    private func spawn(binary: URL, backend: EngineBackend) -> String? {
        Self.debug("spawn: starting \(backend.rawValue) engine at \(redactPathForLog(binary.path))")
        let proc = Process()
        proc.executableURL = binary
        // swift-transformers' NetworkMonitor reports offline until its
        // first NWPathMonitor update arrives, racing welcome-sheet
        // "Install all" clicks. Its escape hatch (HubApi.swift:822).
        var env = ProcessInfo.processInfo.environment
        env["CI_DISABLE_NETWORK_MONITOR"] = "1"
        env[ORTPrivacy.disableTelemetryVariable] = "1"
        // Restructure folder-granularity (Settings ▸ Restructure). The engine reads
        // FILEID_RESTRUCTURE_GRANULARITY at plan time; pass the user's saved choice
        // through at spawn so it applies on the next engine start. "normal"/unset is the
        // calibrated default, so only a validated non-default value is forwarded.
        if let g = UserDefaults.standard.string(forKey: AppSettings.restructureGranularityKey),
           g != AppSettings.restructureGranularityDefault,
           AppSettings.restructureGranularityValues.contains(g) {
            env["FILEID_RESTRUCTURE_GRANULARITY"] = g
        }
        if UserDefaults.standard.bool(forKey: AppSettings.fastTextReadingKey) {
            env["FILEID_CAREFUL_READ"] = AppSettings.fastTextReadingEngineValue
        }
        proc.environment = env
        let inPipe = Pipe()
        let outPipe = Pipe()
        let errPipe = Pipe()
        proc.standardInput = inPipe
        proc.standardOutput = outPipe
        proc.standardError = errPipe

        // IPC flows over stderr (see IPCSink for the .app fd-1 rationale).
        // stdout is unused; drain it, and disarm on EOF — otherwise the closed
        // fd stays permanently readable and the handler busy-spins burning CPU
        // after the engine exits.
        outPipe.fileHandleForReading.readabilityHandler = { handle in
            if handle.availableData.isEmpty {
                handle.readabilityHandler = nil
            }
        }
        // `scanned` = count of leading buffer bytes already known newline-free,
        // so the drain resumes the newline search from where it left off instead
        // of re-scanning the whole buffer every readability tick (the old O(n²)
        // when a large frame accumulates across ticks).
        let stderrBuffer = MutexBox((data: Data(), scanned: 0))
        // 64 MiB inbound frame cap — matches the Windows app's inbound cap and the
        // engine's command-read cap. The engine emits whole event lines (IPCSink
        // caps OUTBOUND too and substitutes ipc_frame_too_large), and a butler
        // restructurePlan (~3.5k+ moves) blows past the old 1 MiB guard, which
        // silently dropped the frame and left the UI wedged with no plan and no
        // error. Bumped 32→64 MiB for a whole-library plan. Still
        // bounded so a wedged engine that never emits a newline can't grow this
        // buffer without limit and OOM the app — an oversize frame surfaces a
        // visible error instead of vanishing.
        let maxFrameBytes = 64 * 1024 * 1024
        // Hand decoded events to the serial pump (Sendable continuation
        // captured as a local) instead of spawning an unordered Task per event.
        let continuation = self.eventContinuation
        errPipe.fileHandleForReading.readabilityHandler = { [weak self, weak proc] handle in
            let data = handle.availableData
            if data.isEmpty {
                handle.readabilityHandler = nil
                Task { @MainActor in
                    self?.handleEngineExit(for: proc)
                }
                return
            }
            // Append + drain whole lines under the lock.
            var oversizeBytes: Int? = nil
            let lines: [Data] = stderrBuffer.withLock { state in
                state.data.append(data)
                var out: [Data] = []
                // Lines are sliced by offset and the consumed prefix is dropped
                // once per chunk; removing it per line shifted the whole buffer
                // for every event in a burst.
                let base = state.data.startIndex
                var consumed = 0
                var searchFrom = state.scanned
                while true {
                    let searchStart = base + max(searchFrom, consumed)
                    guard searchStart < state.data.endIndex,
                          let nl = state.data[searchStart...].firstIndex(of: 0x0A) else { break }
                    let lineStart = base + consumed
                    let lineBytes = nl - lineStart
                    if lineBytes > maxFrameBytes {
                        oversizeBytes = max(oversizeBytes ?? 0, lineBytes)
                    } else if lineBytes > 0 {
                        out.append(state.data.subdata(in: lineStart..<nl))
                    }
                    consumed = nl - base + 1
                    searchFrom = consumed
                }
                if consumed > 0 { state.data.removeSubrange(base..<(base + consumed)) }
                state.scanned = state.data.count
                // A partial frame past the cap (no newline yet) means the engine
                // is emitting garbage or a frame larger than the shared limit —
                // drop it and resync to the next newline rather than buffering
                // unbounded. Reported below instead of dropped silently.
                if state.data.count > maxFrameBytes {
                    oversizeBytes = state.data.count
                    state.data.removeAll(keepingCapacity: false)
                    state.scanned = 0
                }
                return out
            }
            if let dropped = oversizeBytes {
                Self.debug("ENGINE: oversize IPC frame (\(dropped) bytes) — discarding")
                Task { @MainActor in
                    self?.lastError = EngineError(
                        kind: "ipc_frame_too_large",
                        message: "That folder plan is too large to show. Try a smaller folder."
                    )
                }
            }
            for line in lines {
                // Only lines that can be JSON frames earn a decode
                // attempt; anything else is library chatter from an engine
                // predating the fd-2 split (or a torn frame) — log a
                // truncated sample so spew can't bloat app.log.
                guard line.first == 0x7B else {
                    Self.debug("ENGINE: \(Self.redactFrameSample(line))")
                    continue
                }
                if let event = try? IPCCoder.decoder.decode(IPCEvent.self, from: line) {
                    // Serial, ordered hand-off to the @MainActor pump. A video
                    // frame's base64 is decoded here, off the main actor.
                    var videoFrame: DecodedVideoFrame?
                    if case .thumbnailGenerated(let reply) = event.payload {
                        videoFrame = EngineQueryReplies.decode(reply)
                    }
                    continuation.yield(InboundEvent(event: event, videoFrame: videoFrame))
                } else {
                    Self.debug("ENGINE: undecodable frame: \(Self.redactFrameSample(line))")
                }
            }
        }

        do {
            try proc.run()
        } catch {
            // NSError descriptions embed the binary path — persist
            // domain+code only.
            let ns = error as NSError
            Self.debug("spawn: proc.run() FAILED: \(ns.domain) (\(ns.code))")
            return "FileID's helper couldn't start. Quit and reopen FileID. If that doesn't help, reinstall FileID."
        }
        Self.debug("spawn: engine pid=\(proc.processIdentifier), readabilityHandler armed")
        self.process = proc
        self.stdinPipe = inPipe
        runningBackend = backend
        runningReachedReady = false
        return nil
    }

    /// Frame samples can carry full user paths in JSON values (e.g. a
    /// torn deepAnalyzeProgress `currentPath`) — scrub home and volume
    /// prefixes before they persist in app.log. External-volume paths
    /// carry no username but the volume + share names identify the
    /// user's NAS layout, so collapse them the same way.
    nonisolated private static func redactFrameSample(_ line: Data) -> String {
        let sample = String(data: line.prefix(512), encoding: .utf8) ?? "<binary>"
        return sample
            .replacingOccurrences(
                of: #"/Users/[^/"]+"#, with: "~", options: .regularExpression
            )
            .replacingOccurrences(
                of: #"/Volumes/[^/"]+"#, with: "~", options: .regularExpression
            )
    }

    /// Debug log at ~/Library/Application Support/FileID/logs/app.log.
    nonisolated public static func debug(_ msg: String) {
        debugLog.withLock { $0.write(msg) }
    }

    nonisolated private static let debugLog = MutexBox(DebugLog())

    /// One formatter and one open handle for app.log. The handle is dropped
    /// whenever the file is gone (Start over, Factory Reset and Settings all
    /// delete it), so a write never lands in an unlinked file.
    private struct DebugLog {
        private let formatter = ISO8601DateFormatter()
        private var handle: FileHandle?
        private var path: String?

        mutating func write(_ msg: String) {
            let url = AppSupportPath.fileID.appendingPathComponent("logs/app.log")
            if handle == nil || path != url.path || !FileManager.default.fileExists(atPath: url.path) {
                try? handle?.close()
                handle = nil
                guard (try? ReadOnlyLocations.requireWritable(url)) != nil else { return }
                try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                          withIntermediateDirectories: true)
                if !FileManager.default.fileExists(atPath: url.path) {
                    _ = FileManager.default.createFile(atPath: url.path, contents: nil)
                }
                handle = try? FileHandle(forWritingTo: url)
                path = url.path
            }
            guard let handle else { return }
            let line = "\(formatter.string(from: Date())) \(msg)\n"
            // Logging must never crash the app, so write errors are dropped.
            _ = try? handle.seekToEnd()
            _ = try? handle.write(contentsOf: Data(line.utf8))
        }
    }

    func handleEvent(_ event: IPCEvent, videoFrame: DecodedVideoFrame? = nil) {
        switch event.payload {
        case .chatResponse(let response):
            latestChatResponse = response
        case .toolResponse(let response):
            if toolResponses[response.requestID] == nil {
                if toolResponseOrder.count >= 64 { toolResponses.removeValue(forKey: toolResponseOrder.removeFirst()) }
                toolResponseOrder.append(response.requestID)
            }
            toolResponses[response.requestID] = response
        case .catalogResponse(let response):
            if catalogResponses[response.requestID] == nil {
                if catalogResponseOrder.count >= 64 {
                    catalogResponses.removeValue(forKey: catalogResponseOrder.removeFirst())
                }
                catalogResponseOrder.append(response.requestID)
            }
            catalogResponses[response.requestID] = response
            resumeCatalogWaiter(response.requestID, with: response)
            if !response.jobs.isEmpty { catalogJobs = response.jobs }
        case .ready(let info):
            state = .ready(info)
            runningReachedReady = true
            if let runningBackend { fallback.reachedReady(runningBackend) }
            if lastError?.kind == "engine_exited" { lastError = nil }
            restoreSavedFolderAccess()
            // Do NOT clear the respawn budget merely on reaching Ready — a
            // Ready→immediate-crash flap would reset it every cycle and never trip
            // the 3-in-60s cap (unbounded ~1s respawn loop). Just record when we
            // became Ready; handleEngineExit clears the budget only if the engine
            // stayed Ready for `stabilitySettle` (a genuine recovery).
            lastReadyAt = Date()
            // If we just came back from a wipe-and-rescan, auto-start
            // the scan against the user's chosen root.
            if let root = pendingWipeAndRescanRoot {
                pendingWipeAndRescanRoot = nil
                startScan(rootURL: root)
            }
        case .progress(let p):
            // Each IPC line is delivered to the main actor as its own
            // unstructured Task, which the runtime may reorder — so a
            // late "discovered: 100" must never overwrite the final
            // "discovered: 4000". Drop any snapshot that would roll a
            // counter (or the phase) backwards within the same session.
            guard p.supersedes(lastProgress) else { break }
            lastProgress = p
            // Release the App-Nap token on any terminal scan phase. Cancel /
            // fail arrive here as a phase change (not as .scanComplete), so
            // this is the only place those two paths surface.
            switch p.phase {
            case .completed, .cancelled, .failed: endScanActivity()
            case .idle, .discovering, .tagging, .postScan: break
            }
        case .phaseChanged:
            break  // phase is encoded in lastProgress.phase
        case .discoveryComplete:
            break
        case .fileDone:
            break
        case .batchSummary(let b):
            lastBatch = b
        case .scanComplete:
            lastTerminalEventAt = Date()
            endScanActivity()
        case .error(let e):
            queryReplies.deliver(e)
            // Engine startup capability warning: not a real error, just a
            // signal that Deep Analyze can't run on this build.
            if e.kind == "deep_analyze_unavailable" {
                deepAnalyzeAvailable = false
                deepAnalyzeUnavailableReason = e.message
                return
            }
            // PAR-111 mirror: a busy bounce means a pass IS running — the
            // opposite of a failure. Falling through would match the
            // hasPrefix("face_cluster")/hasPrefix("deep") arms below and
            // wrongly clear the in-flight flags while the running pass is
            // still going. Log-only; the running pass
            // emits its own terminal event.
            if e.kind == "face_clustering_busy" || e.kind == "deep_analyze_already_running" {
                Self.debug("engine bounce (benign): \(e.kind)")
                return
            }
            // The user's own Cancel, a repeated click and a declined license
            // are not failures; the request they belong to already told them.
            if e.kind == "prewarm_cancelled" || e.kind == "scan_already_queued" || e.kind == "model_license_not_accepted" {
                Self.debug("engine notice (user-initiated): \(e.kind)")
                return
            }
            lastError = e
            if e.kind.hasPrefix("face_cluster") {
                faceClusteringInFlight = false
            }
            if e.kind.hasPrefix("deep") {
                // A deep-analyze error (e.g. unknown model kind → "deep_invalid")
                // means we'll never get .deepAnalyzeComplete, which is the only
                // place that clears these flags — so clear them here or the UI
                // stays stuck "analyzing…" forever.
                deepAnalyzeInFlight = false
                deepAnalyzeProgress = nil
                endDeepAnalyzeActivity()
            }
        case .log:
            break
        case .faceClusteringComplete(let summary):
            lastFaceClustering = summary
            faceClusteringInFlight = false
            lastTerminalEventAt = Date()
        case .deepAnalyzeStarting(let s):
            deepAnalyzeStarting = s
            deepAnalyzeInFlight = true
            beginDeepAnalyzeActivity()
        case .deepAnalyzeProgress(let p):
            deepAnalyzeProgress = p
            deepAnalyzeInFlight = true
            beginDeepAnalyzeActivity()
            // First per-file progress arrived — clear the "Starting…"
            // card so the progress card can take over without overlap.
            deepAnalyzeStarting = nil
        case .deepAnalyzeFileDone(let d):
            // 500 ms throttle — otherwise SwiftUI's AttributeGraph
            // overflows over a fast Deep Analyze run.
            let now = Date()
            if now.timeIntervalSince(lastDeepAnalyzeFileDoneAt) >= 0.5 {
                deepAnalyzeLast = d
                lastDeepAnalyzeFileDoneAt = now
            }
        case .deepAnalyzeComplete(let c):
            deepAnalyzeComplete = c
            deepAnalyzeInFlight = false
            endDeepAnalyzeActivity()
            lastTerminalEventAt = Date()
            deepAnalyzeProgress = nil
            deepAnalyzeStarting = nil
        case .modelDownloadProgress(let p):
            // Clear the bar once the download completes so the model picker
            // doesn't show a stale "Downloading 100%" forever; otherwise track
            // progress. It also clears on engine exit (handleEngineExit).
            modelDownloadProgress = p.fraction >= 1.0 ? nil : p
        case .queueState(let q):
            queueState = q
        // ── Restructure butler replies. handleEvent already
        //    runs on the main actor (see spawn()'s dispatch), so these
        //    assignments are isolation-safe. ──
        case .restructurePlan(let plan):
            restructurePlan = plan
            restructurePlanSignal &+= 1
        case .restructureApplyResult(let result):
            restructureApplyResult = result
            restructureApplyResultSignal &+= 1
            // Toggle the "Undo last run" affordance: an apply that moved files
            // makes the run undoable; the undo's own reply clears it.
            if undoRestructureInFlight {
                undoRestructureInFlight = false
                // A cancelled or partially-failed undo must KEEP the
                // affordance: Restructure.undoLast only clears the on-disk
                // journal when the undo both completed and had zero failures
                // (Pipeline/Restructure.swift), so the remaining files are
                // still relocated and still reversible. Clearing this
                // unconditionally stranded a half-reverted library with no UI
                // path back — permanently, since this flag is only ever set
                // true by a forward apply and is never re-seeded from disk.
                // Mirrors Windows NextCanUndoRestructure and Linux
                // undo_fully_completed.
                canUndoRestructure = result.cancelled || result.failed > 0
            } else {
                canUndoRestructure = result.applied > 0
            }
        case .bulkActionResult(let result):
            bulkActionResult = result
            bulkActionResultSignal &+= 1
            resumeBulkResultWaiters(with: result)
        // ── Remaining Windows-originated reply events. The mac app's
        //    equivalent flows are synchronous (per-tab actions), so these
        //    aren't consumed here yet; they're decoded so a shared/
        //    cross-platform engine doesn't wedge the wire. ──
        case .rootsList(let list):
            guard list.requestID.hasPrefix("roots-") else { break }
            libraryRoots = list.roots
        case .modelCatalog(let catalog):
            for model in catalog.models { if let result = model.benchmark { benchmarkResults[model.id] = result } }
        case .benchmarkProgress(let progress):
            guard progress.requestID == benchmarkRequestID else { break }
            benchmarkProgress = progress
        case .benchmarkResult(let result):
            guard result.requestID == benchmarkRequestID else { break }
            benchmarkRequestID = nil
            benchmarkModelKind = nil
            benchmarkProgress = nil
            if let benchmark = result.benchmark, result.status == "ok" {
                benchmarkResults[result.modelKind] = benchmark
                benchmarkMessage = nil
            } else {
                benchmarkMessage = (result.modelKind, result.message ?? "The benchmark didn't finish.")
            }
        case .clipTextEmbedding(let reply):
            queryReplies.deliver(reply)
        case .mergeSuggestions(let reply):
            queryReplies.deliver(reply)
        case .thumbnailGenerated(let reply):
            queryReplies.deliver(videoFrame ?? EngineQueryReplies.decode(reply))
        case .faceReview(let page):
            queryReplies.deliver(page)
        case .healthCheckResult,
             .hardwareReprobed,
             .libraryWiped:
            break
        }
    }

    /// Clear the UI state whose only other clearer is an engine terminal event.
    ///
    /// A dead engine can never emit those events, so one mid-job crash would
    /// wedge Deep Analyze / People across the respawn (the respawned engine
    /// starts jobless, so nothing else un-sticks them). Both the crash leg and
    /// a deliberate restart run this — see the call in `terminateRunningEngine`.
    private func resetStateOnlyADeadEngineCouldClear() {
        benchmarkRequestID = nil
        benchmarkModelKind = nil
        benchmarkProgress = nil
        deepAnalyzeInFlight = false
        deepAnalyzeStarting = nil
        deepAnalyzeProgress = nil
        faceClusteringInFlight = false
        queryReplies.abandonAll()
        for id in Array(catalogWaiters.keys) { resumeCatalogWaiter(id, with: nil) }
        resumeBulkResultWaiters(with: nil)
        // A dead engine emits no terminal event, so release both App-Nap
        // tokens here — this is the crash/exit leg that keeps the begin/end
        // pairs balanced. Both helpers no-op when their token isn't held.
        endScanActivity()
        endDeepAnalyzeActivity()
        // Same for the undo affordance: a crash mid-undo never emits the terminal
        // restructureApplyResult that clears this, so without the reset the NEXT
        // apply's result is mis-attributed as the (dead) undo's. (app)
        undoRestructureInFlight = false
        // A crash mid-download leaves a stale sub-1.0 progress that defeats the
        // WelcomeSheet VLM "no response" watchdog and shows a phantom Deep-Analyze
        // download bar.
        modelDownloadProgress = nil
        isPaused = false
        queueState = QueueState(running: nil, pending: [], totalEtaSeconds: nil)
        // Keep the counters as context, but a scan the dead engine was running
        // can never report its terminal phase: left live, the sidebar offers
        // only Pause/Stop against a jobless engine and hides Scan again.
        lastProgress = lastProgress?.settledAfterEngineExit()
        // Signal views that own their in-flight UI (e.g. Deep Analyze's
        // StreamCard + inert Cancel) to reset — the engine can no longer
        // emit the terminal event that would clear them.
        engineResetSignal &+= 1
    }

    @MainActor
    private func handleEngineExit(for proc: Process?) {
        // Identity guard (mirrors the Windows sender != _process check):
        // a late EOF from a process we already terminated/replaced — a
        // Restart, or a respawn that raced — must NOT tear down the
        // engine that's live now, or reset its in-flight UI.
        guard let proc, proc === self.process else { return }

        // Nil pipe handlers so any in-flight GCD callback short-circuits.
        (proc.standardError as? Pipe)?.fileHandleForReading.readabilityHandler = nil
        (proc.standardOutput as? Pipe)?.fileHandleForReading.readabilityHandler = nil
        process = nil
        stdinPipe = nil
        let exitedBackend = runningBackend
        let exitedBeforeReady = !runningReachedReady
        runningBackend = nil

        resetStateOnlyADeadEngineCouldClear()

        // Expected exit (shutdown called or work just completed): silent
        // re-spawn, no error pill, no respawn budget burned. Covers
        // MLX's known SIGSEGV-in-static-destructor on clean exit.
        let recentClean = Date().timeIntervalSince(lastTerminalEventAt) < 5.0
        if expectedExit || recentClean {
            Self.debug("exit: clean (expectedExit=\(expectedExit) recentClean=\(recentClean))")
            expectedExit = false
            // Wipe the SQLite library before the respawn — this is
            // the only safe window to delete it; the engine holds
            // the WAL lock while it's running.
            if pendingWipeAndRescanRoot != nil {
                Self.deleteLibraryFiles()
                clearProgress()
            }
            state = .starting
            pendingRespawn?.cancel()
            pendingRespawn = Task { @MainActor [weak self] in
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                self?.start()
            }
            return
        }

        // A Rust engine that can't get going falls back to the Swift engine
        // at once, with a fresh respawn budget.
        if let exitedBackend, exitedBeforeReady, fallback.exitedBeforeReady(exitedBackend) {
            fallBackAfterExit(of: exitedBackend)
            return
        }

        let now = Date()
        // A genuine recovery (engine stayed Ready for ≥ stabilitySettle)
        // clears the budget; a Ready→immediate-crash flap does not, so it keeps
        // ticking toward the 3-in-60s terminal cap instead of resetting forever.
        if lastReadyAt != .distantPast, now.timeIntervalSince(lastReadyAt) >= Self.stabilitySettle {
            respawnAttempts.removeAll()
        }
        lastReadyAt = .distantPast
        respawnAttempts = respawnAttempts.filter {
            now.timeIntervalSince($0) < Self.respawnWindow
        }

        let attemptIdx = respawnAttempts.count
        guard attemptIdx < Self.respawnDelays.count else {
            if let exitedBackend, fallback.respawnsExhausted(exitedBackend) {
                fallBackAfterExit(of: exitedBackend)
                return
            }
            state = .crashed(reason: "FileID's helper keeps stopping. Press Restart, or quit and reopen FileID.")
            Self.debug("respawn: budget exhausted, marking crashed")
            return
        }

        let delay = Self.respawnDelays[attemptIdx]
        respawnAttempts.append(now)
        state = .starting
        lastError = EngineError(
            kind: "engine_exited",
            message: "FileID's helper stopped and is restarting…"
        )
        Self.debug("respawn: scheduling attempt \(attemptIdx + 1)/\(Self.respawnDelays.count) in \(delay)s")

        pendingRespawn?.cancel()
        pendingRespawn = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
            guard let self else { return }
            if case .crashed = self.state { return }
            self.start()
        }
    }

    private func fallBackAfterExit(of backend: EngineBackend) {
        Self.debug("fallback: \(backend.rawValue) engine demoted for this session")
        respawnAttempts.removeAll()
        lastReadyAt = .distantPast
        state = .starting
        if let note = fallback.note { lastError = EngineError(kind: "engine_fallback", message: note) }
        pendingRespawn?.cancel()
        pendingRespawn = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 1_000_000_000)
            guard let self else { return }
            if case .crashed = self.state { return }
            self.start()
        }
    }

    // MARK: - App Nap activity (begin/end must stay balanced)

    private func beginScanActivity() {
        guard scanActivityToken == nil else { return }
        scanActivityToken = AppSleepActivity.begin(reason: "FileID is scanning files")
    }

    private func endScanActivity() {
        guard let token = scanActivityToken else { return }
        AppSleepActivity.end(token)
        scanActivityToken = nil
    }

    private func beginDeepAnalyzeActivity() {
        guard deepAnalyzeActivityToken == nil else { return }
        deepAnalyzeActivityToken = AppSleepActivity.begin(reason: "FileID is running Deep Analyze")
    }

    private func endDeepAnalyzeActivity() {
        guard let token = deepAnalyzeActivityToken else { return }
        AppSleepActivity.end(token)
        deepAnalyzeActivityToken = nil
    }

    // MARK: - Commands

    /// Returns false when the command could not be queued (engine down or
    /// encode failure). A later pipe-write failure terminates the captured
    /// engine generation so its normal exit path clears caller state.
    @discardableResult
    public func send(_ payload: IPCCommand.Payload) -> Bool {
        guard let pipe = stdinPipe else { return false }
        let cmd = IPCCommand(payload: payload)
        let data: Data
        do {
            data = try IPCCoder.encodeLine(cmd)
        } catch {
            FileHandle.standardError.write(Data("EngineClient send encode failed: \(error)\n".utf8))
            return false
        }

        // Off the main thread with a 10 s deadline. FileHandle.write
        // blocks if the engine is dead with a full stdin buffer; on
        // timeout we kill the engine to trigger handleEngineExit's
        // respawn.
        let writeHandle = pipe.fileHandleForWriting
        let procBox = MutexBox<Process?>(self.process)
        let done = MutexBox(false)
        // Serial queue → commands are written in submission order and never
        // interleave. The timeout below stays on a concurrent queue so it fires
        // as a real timer even while a blocked write holds the serial queue.
        stdinWriteQueue.async {
            DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + 10.0) {
                guard !done.withLock({ $0 }) else { return }
                FileHandle.standardError.write(Data("EngineClient send timed out (engine stdin blocked)\n".utf8))
                procBox.withLock { $0?.terminate() }
            }
            do {
                try writeHandle.write(contentsOf: data)
            } catch {
                FileHandle.standardError.write(Data("EngineClient send failed: \(error)\n".utf8))
                procBox.withLock { $0?.terminate() }
            }
            done.withLock { $0 = true }
        }
        return true
    }

    /// Wipe progress + last batch + error (e.g. when the user picks a new folder).
    public func clearProgress() {
        lastProgress = nil
        lastBatch = nil
        lastError = nil
    }

    public func clearLastError() { lastError = nil }

    private func restoreSavedFolderAccess() {
        guard let bookmark = UserDefaults.standard.data(
            forKey: SecurityScopedBookmark.pickedFolderDefaultsKey
        ) else { return }
        Task.detached(priority: .utility) { [weak self] in
            guard let rootURL = SecurityScopedBookmark.retainAccess(for: bookmark) else { return }
            guard let ipcBookmark = try? SecurityScopedBookmark.makeIPCBookmark(for: rootURL) else {
                await self?.setSavedFolderBookmarkError()
                return
            }
            await self?.grantSavedFolderAccess(path: rootURL.path, bookmark: ipcBookmark)
        }
    }

    private func setSavedFolderBookmarkError() {
        lastError = EngineError(
            kind: "bookmark_unavailable",
            message: "FileID can't open that folder any more. Choose the folder again."
        )
    }

    private func grantSavedFolderAccess(path: String, bookmark: Data) {
        _ = send(.grantFolderAccess(rootPath: path, rootBookmark: bookmark))
    }

    public func startScan(rootURL: URL) {
        isPaused = false
        let displayPath = rootURL.path
        // The persisted app-scoped bookmark stays in the app; the engine gets
        // a separate implicit-scope bookmark that can cross the process boundary.
        let client = self
        Task.detached(priority: .userInitiated) {
            var resolvedPath = rootURL.standardizedFileURL.path
            var rootBookmark: Data?
            do {
                rootBookmark = try SecurityScopedBookmark.makeIPCBookmark(for: rootURL)
            } catch {
                #if FILEID_APP_STORE
                await MainActor.run {
                    client.lastError = EngineError(
                        kind: "bookmark_unavailable",
                        message: "FileID couldn't retain access to that folder. Choose the folder again."
                    )
                    client.cancelAutoPilot()
                }
                return
                #else
                resolvedPath = displayPath
                #endif
            }
            await MainActor.run {
                // Begin the App-Nap token only if the command actually left the
                // app; a dropped send (engine down) would otherwise leave the
                // token held with no scan and no terminal event to release it.
                if client.send(.startScan(rootPath: resolvedPath, rootBookmark: rootBookmark,
                                          rootDisplay: displayPath,
                                          rescan: false, excludedPaths: nil)) {
                    client.beginScanActivity()
                } else {
                    client.lastError = EngineError(
                        kind: "engine_exited",
                        message: "The scan couldn't start because FileID's helper isn't running. Wait a moment and press Scan again."
                    )
                    client.cancelAutoPilot()
                }
            }
        }
    }

    /// Nothing chains stages any more; kept so Chat's "stop" action still has
    /// something to call.
    public func cancelAutoPilot() {}

    public func pause()    { isPaused = true;  send(.pauseScan)  }
    public func resume()   { isPaused = false; send(.resumeScan) }
    /// Stops the scan only; a folder move or Describe Files run keeps going.
    public func cancelScan() {
        isPaused = false
        send(.cancelScan)
        cancelAutoPilot()
    }

    /// Stops everything the engine is doing (Chat's "stop all").
    public func cancel() {
        isPaused = false
        send(.cancelScan)
        send(.cancelRestructure)
        send(.deepAnalyzeCancel)
        cancelAutoPilot()
    }
    public func shutdown() {
        // Only latch the expected-exit suppression when the shutdown
        // command actually left the app — otherwise a genuine crash that
        // races the respawn window (stdinPipe nil → send() returns false)
        // would be masked as a clean exit and skip the error pill.
        if send(.shutdown) {
            expectedExit = true
        }
    }

    /// Factory Reset: terminate the engine, wipe the Application Support folder,
    /// purge all UserDefaults, and exit the macOS app immediately.
    public func factoryResetAndQuit() {
        terminateRunningEngine()
        SpotlightIndexer.wipe()
        let fm = FileManager.default
        if (try? ReadOnlyLocations.requireWritable(AppSupportPath.fileID)) != nil {
            try? fm.removeItem(at: AppSupportPath.fileID)
        }
        if let bundleID = Bundle.main.bundleIdentifier {
            UserDefaults.standard.removePersistentDomain(forName: bundleID)
            UserDefaults.standard.synchronize()
        }
        #if os(macOS)
        NSApplication.shared.terminate(nil)
        #endif
    }

    /// Wipes the SQLite library + scan logs and triggers a fresh
    /// scan against `rootURL` once the engine has restarted. Cancels
    /// any in-flight scan first. The engine has to exit before we
    /// can delete the SQLite files (it holds the WAL lock), so the
    /// flow is: shutdown → engine exit → handleEngineExit deletes
    /// the files → engine respawn → on `.ready` event we trigger
    /// the new scan.
    public func wipeAndRescan(rootURL: URL) {
        if let p = lastProgress, p.phase == .discovering || p.phase == .tagging || p.phase == .postScan {
            send(.cancelScan)
        }
        // Snapshot the bookmark + display path NOW — by the time the
        // restarted engine is ready, the security-scoped resource
        // would have to be re-acquired. We rely on the same
        // bookmark-resolve path as `startScan`.
        pendingWipeAndRescanRoot = rootURL
        // Engine already down (respawn backoff or exhausted budget):
        // the shutdown would be silently dropped and no exit would
        // ever run the wipe — but no WAL lock is held either, so wipe

        // directly and respawn.
        guard stdinPipe != nil else {
            Self.deleteLibraryFiles()
            clearProgress()
            state = .starting
            pendingRespawn?.cancel()
            pendingRespawn = Task { @MainActor [weak self] in
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                self?.start()
            }
            return
        }
        if send(.shutdown) {
            expectedExit = true
        }
    }

    /// Deletes the SQLite library + WAL/SHM siblings + scan log.
    /// Safe to call only when the engine isn't running — SQLite
    /// holds the lock otherwise.
    private static func deleteLibraryFiles() {
        let fm = FileManager.default
        let root = AppSupportPath.fileID
        let candidates = [
            "fileid.sqlite",
            "fileid.sqlite-wal",
            "fileid.sqlite-shm",
            "logs/scan.jsonl",
            "logs/app.log"
        ]
        for name in candidates {
            let url = root.appendingPathComponent(name)
            if (try? ReadOnlyLocations.requireWritable(url)) != nil {
                try? fm.removeItem(at: url)
            }
        }
        // The Spotlight items mirror the rows just wiped — without
        // this, captions/tags/paths of wiped files stay queryable in
        // ⌘Space long after the library is gone.
        SpotlightIndexer.wipe()
    }

    // In-flight flags arm only when send() succeeded — a click during
    // the respawn backoff (stdinPipe nil) used to set the flag, drop
    // the command, and wedge the UI with no job running.
    public func runFaceClustering() {
        guard !faceClusteringInFlight, send(.runFaceClustering) else { return }
        faceClusteringInFlight = true
    }

    @discardableResult
    public func markPersonsDifferent(
        sourcePersonID: Int64,
        destinationPersonID: Int64,
        sourceAnchorFaceID: Int64,
        destinationAnchorFaceID: Int64
    ) -> Bool {
        send(.markPersonsDifferent(
            sourcePersonID: sourcePersonID,
            destinationPersonID: destinationPersonID,
            sourceAnchorFaceID: sourceAnchorFaceID,
            destinationAnchorFaceID: destinationAnchorFaceID
        ))
    }

    public func deepAnalyzeFile(fileID: Int64, modelKind: String) {
        guard ModelLicenseGate.ensureAccepted(for: AIModelKind.migrated(rawValue: modelKind)) else { return }
        guard send(.deepAnalyzeFile(fileID: fileID, modelKind: modelKind)) else { return }
        deepAnalyzeInFlight = true
        deepAnalyzeProgress = nil
        deepAnalyzeComplete = nil
        deepAnalyzeStarting = nil
        beginDeepAnalyzeActivity()
    }

    public func deepAnalyzeFolder(prefix: String, modelKind: String) {
        guard ModelLicenseGate.ensureAccepted(for: AIModelKind.migrated(rawValue: modelKind)) else { return }
        guard send(.deepAnalyzeFolder(pathPrefix: prefix, modelKind: modelKind)) else { return }
        deepAnalyzeInFlight = true
        deepAnalyzeProgress = nil
        deepAnalyzeComplete = nil
        deepAnalyzeStarting = nil
        beginDeepAnalyzeActivity()
    }

    public func deepAnalyzeAll(modelKind: String, skipExisting: Bool) {
        guard ModelLicenseGate.ensureAccepted(for: AIModelKind.migrated(rawValue: modelKind)) else { return }
        // Every current call site is a whole-library run (no fileIDs), so the
        // persisted Deep Analyze exclusion list is threaded through here —
        // the one choke point every deepAnalyzeAll send passes through —
        // rather than at each call site. Sent as nil (omitted on the wire)
        // when empty; ignored engine-side whenever fileIDs is present.
        let excluded = DeepAnalyzeSettings.shared.excludedFolders
        guard send(.deepAnalyzeAll(modelKind: modelKind, skipExisting: skipExisting, tagsOnly: false, proposeRenames: true, fileIDs: nil, excludedFolders: excluded.isEmpty ? nil : excluded)) else { return }
        deepAnalyzeInFlight = true
        deepAnalyzeProgress = nil
        deepAnalyzeComplete = nil
        deepAnalyzeStarting = nil
        beginDeepAnalyzeActivity()
    }

    /// Describe an explicit selection of at most 100 files (the schema bound).
    @discardableResult
    public func deepAnalyzeSelection(fileIDs: [Int64], modelKind: String) -> Bool {
        guard !fileIDs.isEmpty, fileIDs.count <= 100,
              ModelLicenseGate.ensureAccepted(for: AIModelKind.migrated(rawValue: modelKind)) else { return false }
        guard send(.deepAnalyzeAll(modelKind: modelKind, skipExisting: false, tagsOnly: false, proposeRenames: true, fileIDs: fileIDs, excludedFolders: nil)) else { return false }
        deepAnalyzeInFlight = true
        deepAnalyzeProgress = nil
        deepAnalyzeComplete = nil
        deepAnalyzeStarting = nil
        beginDeepAnalyzeActivity()
        return true
    }

    public func deepAnalyzeCancel() {
        send(.deepAnalyzeCancel)
    }

    // MARK: - Restructure butler commands

    /// Ask the engine to compute a restructure plan for `libraryRoot`. The
    /// reply lands on `restructurePlan` and bumps `restructurePlanSignal`.
    /// Returns false when the command never left the app (engine starting /
    /// down) so the caller can stop its "computing…" spinner.
    @discardableResult
    public func planRestructure(libraryRoot: String) -> Bool {
        send(.planRestructure(libraryRoot: libraryRoot, supportsPagedPlans: true))
    }

    /// Apply the selected `moves` through the engine butler. macOS performs
    /// real on-disk moves only — there is no symlink-preview apply mode, so
    /// the engine now rejects `useSymlinks: true` with an error instead of
    /// silently performing a real move. Callers
    /// should not pass `true`; the parameter exists for wire parity with the
    /// Windows engine. The reply lands on `restructureApplyResult` and bumps
    /// `restructureApplyResultSignal`.
    @discardableResult
    public func applyRestructure(libraryRoot: String, moves: [RestructureMove],
                                 useSymlinks: Bool = false,
                                 planID: String? = nil) -> Bool {
        send(.applyRestructure(
            libraryRoot: libraryRoot, moves: moves,
            useSymlinks: useSymlinks, planID: planID))
    }

    /// Reverse the most recent applyRestructure — the engine replays its on-disk
    /// undo journal, moving every relocated file back. The reply lands on
    /// `restructureApplyResult` (applied = files moved back) and clears
    /// `canUndoRestructure`. (RESTRUCTURE.md §6 reversibility)
    @discardableResult
    public func undoRestructure(libraryRoot: String) -> Bool {
        // Arm the in-flight flag ONLY on a successful send. A failed send (engine
        // respawning) would otherwise latch the flag and mis-attribute the NEXT
        // apply's result as the undo's — leaving canUndoRestructure false while the
        // on-disk journal is still undoable. (app)
        let sent = send(.undoRestructure(libraryRoot: libraryRoot))
        if sent { undoRestructureInFlight = true }
        return sent
    }

    /// Cooperatively cancel the active restructure plan/apply/undo — never a
    /// library scan (that's `cancel()` → `.cancelScan`; the schema requires
    /// `.cancelRestructure` stay isolated from it). The engine finishes the
    /// move currently in flight (each is already durable before the next
    /// cancel-poll) and replies with a terminal `restructureApplyResult`
    /// whose `cancelled` is true.
    public func cancelRestructure() {
        send(.cancelRestructure)
    }

    /// Pre-fetch a VLM's weights without running inference. Used by the
    /// welcome-sheet onboarding flow so first-launch downloads happen
    /// up front instead of stalling the first Deep Analyze run. The
    /// engine emits `modelDownloadProgress` events identical to the
    /// in-Deep-Analyze flow; bind to `engine.modelDownloadProgress`
    /// for live progress.
    @discardableResult
    public func prewarmModel(_ modelKind: String) -> Bool {
        guard ModelLicenseGate.ensureAccepted(for: AIModelKind.migrated(rawValue: modelKind)) else { return false }
        return send(.prewarmModel(modelKind: modelKind))
    }

    /// Measure an installed model on this Mac: cold load, prefill and decode
    /// speed, seconds per image, peak memory, JSON reliability.
    @discardableResult
    public func benchmarkModel(_ kind: AIModelKind) -> Bool {
        guard benchmarkRequestID == nil else { return false }
        let requestID = "bench-" + UUID().uuidString.prefix(8).lowercased()
        guard send(.benchmarkModel(requestID: requestID, modelKind: kind.rawValue, tune: false)) else { return false }
        benchmarkRequestID = requestID
        benchmarkModelKind = kind.rawValue
        benchmarkMessage = nil
        benchmarkProgress = BenchmarkProgress(requestID: requestID, modelKind: kind.rawValue, phase: "load", completed: 0, total: 1)
        return true
    }

    public func cancelBenchmark() {
        send(.cancelBenchmark)
    }

    /// Asks the engine for the scanned locations; the answer lands in `libraryRoots`.
    public func refreshLibraryRoots() {
        send(.listRoots(requestID: "roots-" + UUID().uuidString.prefix(8).lowercased()))
    }

    /// Pulls stored benchmark results for this Mac.
    public func refreshModelCatalog() {
        send(.listModels(requestID: "catalog-" + UUID().uuidString.prefix(8).lowercased(), role: nil))
    }

    /// Cancel a running prewarm. Lands at the next Task.checkCancellation
    /// inside swift-transformers' Hub fetcher (typically <1 s). `nil` cancels
    /// every in-flight prewarm (the only mode the welcome sheet uses today).
    public func cancelPrewarm() {
        send(.cancelPrewarm(modelKind: nil))
    }

    /// Drop a stale `modelDownloadProgress` left behind by a cancelled prewarm.
    /// The engine only auto-clears the bar at fraction 1.0 (or on exit); a user
    /// Cancel stops mid-download, so the last fraction lingers with the same
    /// modelKind and would fool WelcomeSheet's retry watchdogs into believing a
    /// fresh download is already progressing — arming neither the "no response"
    /// nor the "stalled" timer. Pass a modelKind to clear only that model's
    /// residue; nil clears whatever is showing.
    public func clearModelDownloadProgress(forModelKind modelKind: String? = nil) {
        if let modelKind, modelDownloadProgress?.modelKind != modelKind { return }
        modelDownloadProgress = nil
    }
}

extension ScanProgress {
    func settledAfterEngineExit() -> ScanProgress {
        switch phase {
        case .completed, .cancelled, .failed, .idle: return self
        case .discovering, .tagging, .postScan:
            return ScanProgress(sessionID: sessionID, phase: .failed, total: total, discovered: discovered,
                                processed: processed, failed: failed, filesPerSecond: 0, etaSeconds: nil,
                                residentMB: residentMB, availableMB: availableMB)
        }
    }

    /// True when `self` is a fresher scan-progress snapshot than `prior`
    /// and should replace it. Snapshots reach the main actor as
    /// independent unstructured Tasks the runtime may reorder; a stale
    /// one must never roll a counter backwards — e.g. a late
    /// `discovered: 100` overwriting the final `discovered: 4000`.
    /// Within one session the counters are monotonic and the phase only
    /// advances; a different session always supersedes.
    func supersedes(_ prior: ScanProgress?) -> Bool {
        guard let prior, prior.sessionID == sessionID else { return true }
        func isTerminal(_ phase: ScanPhase) -> Bool {
            switch phase {
            case .completed, .cancelled, .failed: return true
            case .idle, .discovering, .tagging, .postScan: return false
            }
        }
        // Terminal snapshots always win; once terminal, nothing rolls back.
        if isTerminal(phase) { return true }
        if isTerminal(prior.phase) { return false }
        if discovered < prior.discovered { return false }
        if processed < prior.processed { return false }
        // total latches 0 → positive at discovery's end; never drop it.
        if prior.total > 0 && total == 0 { return false }
        return true
    }
}

/// Lock-protected box: concurrent closures capture a reference instead
/// of a `var`, sidestepping Swift 6 SendableClosureCaptures errors.
final class MutexBox<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: T
    init(_ initial: T) { self.value = initial }
    func withLock<R>(_ body: (inout T) throws -> R) rethrows -> R {
        lock.lock()
        defer { lock.unlock() }
        return try body(&value)
    }
}
