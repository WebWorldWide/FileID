// Spawns and supervises the engine child process, auto-respawning with backoff; state and events are observable on MainActor.
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
    /// Bumps on every `lastError` write, including an identical repeat. EngineError isn't Equatable, so views key error-recovery `.onChange` on this counter; two consecutive identical failures must still re-fire.
    public private(set) var lastErrorSignal: Int = 0
    public private(set) var latestChatResponse: ChatResponse?
    public private(set) var toolResponses: [String: ToolResponse] = [:]
    private var toolResponseOrder: [String] = []
    private var catalogResponseOrder: [String] = []
    public private(set) var catalogResponses: [String: CatalogResponse] = [:]
    public private(set) var catalogJobs: [CatalogJob] = []
    func searchCatalog(query: String? = nil, vector: [Float]? = nil, seedID: Int64? = nil, limit: Int = LibrarySearchOutcome.resultLimit) async -> CatalogResponse? {
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

    func catalogReply(to request: CatalogRequest, timeout: Duration) async -> CatalogResponse? {
        guard send(.catalogRequest(request: request)) else { return nil }
        return await catalogResponse(for: request.requestID, timeout: timeout)
    }

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

    private var bulkResultWaiters: [UUID: (matcher: BulkResultMatcher, continuation: CheckedContinuation<BulkActionResult?, Never>)] = [:]

    /// The first bulk result after `signal` whose action starts with `prefix`; nil on timeout or engine exit. A cancelled caller still gets it: the files were already moved and undo needs the batch it names.
    func bulkActionResult(after signal: Int, prefix: String, commandID: String? = nil, timeout: Duration) async -> BulkActionResult? {
        let matcher = BulkResultMatcher(actionPrefix: prefix, sentID: commandID)
        if bulkActionResultSignal != signal, let result = bulkActionResult, matcher.accepts(result) {
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
            bulkResultWaiters[token] = (matcher, continuation)
        }
    }

    private func resumeBulkResultWaiters(with result: BulkActionResult?) {
        for (token, waiter) in bulkResultWaiters where result.map({ waiter.matcher.accepts($0) }) ?? true {
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

    /// JPEG bytes with a `maxPixelSize` long side (clamped to 64–2048, never upscaled), or nil when the engine is down or can't decode the video.
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
    /// "Finding faces… 120 of 848" while the engine reads the faces of a pass, which it reports as the running job's title.
    public var faceStageProgress: String? {
        guard let job = queueState.running, job.category == .faceCluster, job.title.hasPrefix("Finding faces") else { return nil }
        return job.title
    }
    /// Engine doesn't echo paused state, so the app mirrors it locally.
    public private(set) var isPaused: Bool = false

    public private(set) var deepAnalyzeProgress: DeepAnalyzeProgress?
    public private(set) var deepAnalyzeLast: DeepAnalyzeFileDone?
    public private(set) var deepAnalyzeComplete: DeepAnalyzeComplete?
    public private(set) var modelDownloadProgress: ModelDownloadProgress?
    public private(set) var benchmarkProgress: BenchmarkProgress?
    public private(set) var libraryRoots: [LibraryRoot] = []
    /// Which paths sit on read-only locations, from the engine's root list plus the Adlon name guard.
    var readOnlyGate: ReadOnlyGate { ReadOnlyGate(libraryRoots) }
    public private(set) var benchmarkResults: [String: ModelBenchmark] = [:]
    /// The engine's own verdict on each vision model for this Mac and the memory free now, from the last `listModels` for the vision role.
    public private(set) var vlmAdvice: VLMAdvice?
    public private(set) var benchmarkMessage: (modelKind: String, text: String)?
    public private(set) var benchmarkModelKind: String?
    private var benchmarkRequestID: String?
    public private(set) var deepAnalyzeInFlight: Bool = false
    /// Streamed between command receipt and the first `deepAnalyzeProgress` so the UI can show labelling during the ~10 s VLM cold load; cleared on the first progress event or run completion.
    public private(set) var deepAnalyzeStarting: DeepAnalyzeStarting?
    /// False when the engine reports mlx.metallib missing (Deep Analyze would crash on first VLM call); the UI disables and explains.
    public private(set) var deepAnalyzeAvailable: Bool = true
    public private(set) var deepAnalyzeUnavailableReason: String?
    /// The scan fell back to the CPU for picture search and will take longer than usual; cleared when the next scan starts.
    public private(set) var scanRunsOnCPU = false

    /// Bumped on every engine exit. A dead engine can't emit the terminal events that clear a view's own in-flight UI (e.g. Deep Analyze's StreamCard + Cancel), so views reset that state via `.onChange`. The client's own flags are reset in `handleEngineExit`.
    public private(set) var engineResetSignal: Int = 0

    public private(set) var queueState: QueueState = QueueState(
        running: nil, pending: [], totalEtaSeconds: nil
    )

    // MARK: - Restructure butler (engine-routed plan + apply)
    // The `*Signal` counters bump on every fresh payload so views can react via `.onChange` even though the DTOs aren't Equatable (same idiom as `engineResetSignal`).
    public private(set) var restructurePlan: RestructurePlan?
    public private(set) var restructureApplyResult: RestructureApplyResult?
    public private(set) var restructurePlanSignal: Int = 0
    public private(set) var restructureApplyResultSignal: Int = 0
    public private(set) var bulkActionResult: BulkActionResult?
    public private(set) var bulkActionResultSignal: Int = 0
    public private(set) var lastSentCommandID: String?
    /// True once an applyRestructure has moved files and they haven't been undone; drives "Undo last run".
    public private(set) var canUndoRestructure = false
    /// The folder whose last Organize is waiting to be undone after a relaunch (from the engine's journal on disk); nil when the undo came from this session's own apply.
    public private(set) var restructureUndoRoot: String?
    private var undoRestructureInFlight = false

    private var process: Process?
    private var stdinPipe: Pipe?
    /// Serial queue for stdin writes: the former global concurrent queue let two rapid send() calls write to the stdin fd at once, reordering commands or interleaving bytes mid-line.
    private let stdinWriteQueue = DispatchQueue(label: "com.fileid.engine.stdin")

    // MARK: - App Nap protection
    // Operation-scoped tokens (not an app-lifetime one) keep the UI process from being App-Napped during a long scan or Deep Analyze run without holding it un-nappable while idle. System idle-sleep is the engine's SleepGuard. Each token is begun when its operation starts and ended on EVERY terminal path (completion, cancel, failure, engine exit) through guarded helpers (begin no-ops if held, end no-ops if cleared), so the pair can't leak or double-release.
    private var scanActivityToken: NSObjectProtocol?
    private var deepAnalyzeActivityToken: NSObjectProtocol?

    // Up to 3 respawns within respawnWindow; cleared on .ready.
    private static let respawnDelays: [UInt64] = [1, 4, 16]
    private static let respawnWindow: TimeInterval = 60
    private var respawnAttempts: [Date] = []
    // When the engine last reached Ready. The respawn budget is cleared only after `stabilitySettle` of continuous Ready (a real recovery), not on every Ready, which would let a Ready→crash flap reset it forever.
    private var lastReadyAt: Date = .distantPast
    private static let stabilitySettle: TimeInterval = 30
    private var pendingRespawn: Task<Void, Never>?

    /// Set on shutdown() or after a "work complete" signal; suppresses the phantom error pill after MLX's known SIGSEGV-at-exit bug.
    private var expectedExit: Bool = false
    public private(set) var lastTerminalEventAt: Date = .distantPast

    /// When non-nil, the next engine exit deletes the SQLite library before the respawn and the next `.ready` auto-starts a scan of this URL (`wipeAndRescan(rootURL:)`).
    private var pendingWipeAndRescanRoot: URL?
    /// A folder to scan the moment the scan being stopped has ended; the engine refuses a start while the old scan is still draining.
    private(set) var pendingScanRoot: URL?
    private var scanBusyRetriesLeft = 0

    /// 2 Hz throttle on `deepAnalyzeLast`; otherwise SwiftUI's
    /// AttributeGraph overflows on a fast Deep Analyze run.
    private var lastDeepAnalyzeFileDoneAt: Date = .distantPast

    // Every decoded event goes through ONE serial AsyncStream consumed by a single @MainActor task, so handleEvent runs in strict receipt order; per-event `Task { @MainActor }` dispatch had no ordering guarantee and could wedge the Deep Analyze / face-clustering in-flight flags.
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

    /// Where the engine binary lives and how it is vetted before it starts; tests point it at a folder holding a stand-in.
    struct LaunchContext {
        var directory: URL
        var environment: [String: String]
        var integrityFailure: (URL) -> String?

        static func appBundle() -> LaunchContext {
            let exec = Bundle.main.executablePath ?? CommandLine.arguments[0]
            return LaunchContext(directory: URL(fileURLWithPath: exec).deletingLastPathComponent(),
                                 environment: ProcessInfo.processInfo.environment,
                                 integrityFailure: EngineClient.engineIntegrityFailure(binary:))
        }

        var binary: URL { directory.appendingPathComponent(EngineBinary.name) }

        var isBundled: Bool {
            FileManager.default.isExecutableFile(atPath: binary.path)
        }
    }

    private let launch: LaunchContext

    public private(set) var startFailure: EngineStartFailure?
    private var runningReachedReady = false
    private var startupExits = EngineStartupExits()

    public func restart() {
        pendingRespawn?.cancel()
        respawnAttempts.removeAll()
        startupExits = EngineStartupExits()
        start()
    }

    public func start() {
        // A live engine here means this is a restart. Terminate + reap it FIRST: two live engines = two SQLite writers against one DB = corruption.
        terminateRunningEngine()
        startFailure = nil
        if case .crashed = state { state = .starting }
        guard launch.isBundled else {
            Self.debug("start: engine binary not found next to app executable")
            fail(EngineStartFailure(.missing))
            return
        }
        // Refuse to spawn an engine binary that didn't ship with this app, so a malicious process can't drop a payload at FileID.app/Contents/MacOS/fileidd and get full FS access via IPC. Dev (ad-hoc) and notarized (Developer ID) builds both share the app's signing identity, and we require it to match.
        if let reason = launch.integrityFailure(launch.binary) {
            Self.debug("start: \(reason)")
            fail(EngineStartFailure(.signatureMismatch))
            return
        }
        if let failure = spawn(binary: launch.binary) {
            fail(failure)
        }
    }

    private func fail(_ failure: EngineStartFailure) {
        Self.debug("engine unavailable: \(failure.cause)")
        startFailure = failure
        if lastError?.kind == "engine_exited" { lastError = nil }
        state = .crashed(reason: failure.summary)
    }

    /// Stop half of a restart: cancel any pending respawn and synchronously terminate + reap the engine so two engines never overlap on the single-writer SQLite DB. The old process's pipe handlers stay armed (so it keeps draining and can't deadlock on a full pipe); its late EOF is harmless because `handleEngineExit(for:)` rejects any process that is no longer `self.process`. Idempotent.
    private func terminateRunningEngine() {
        pendingRespawn?.cancel()
        pendingRespawn = nil
        if let proc = process, proc.isRunning {
            proc.terminate()
            // Bounded reap, not an unbounded waitUntilExit() on the @MainActor: SIGTERM drops the WAL lock in well under a second, but a wedged engine must never hang the UI. Poll, then escalate to SIGKILL.
            if !Self.waitForExit(proc, timeout: 3.0) {
                kill(proc.processIdentifier, SIGKILL)
                _ = Self.waitForExit(proc, timeout: 2.0)
            }
        }
        process = nil
        stdinPipe = nil

        // `handleEngineExit(for:)` rejects this process once it stops being `self.process`, so a restart never reaches the reset there (`start()` reassigns `self.process` before the old pipe's EOF lands). Without this call, Restart Engine during a job latched `deepAnalyzeInFlight` (and undo/queue/App-Nap state) against a jobless engine, disabling Analyze until relaunch.
        resetStateOnlyADeadEngineCouldClear()
        // A restart is an explicit "start over", so drop the frozen scan progress too; otherwise the sidebar scan card keeps a dead Pause/Stop pair with no Start. The crash leg keeps its last progress as forensic context.
        lastProgress = nil
    }

    /// Bounded wait for a child to exit; polls `isRunning` (flipped by Foundation's background reaper, even while this runs on the main actor) instead of the hang-prone `waitUntilExit()`.
    private static func waitForExit(_ proc: Process, timeout: TimeInterval) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while proc.isRunning {
            if Date() >= deadline { return false }
            usleep(20_000)
        }
        return true
    }

    /// Returns a failure reason when the engine binary shouldn't be spawned. Two mandatory checks:
    ///  1. The engine path resolves inside the app bundle's `Contents/MacOS/` (blocks the drop-a-payload attack, including symlinks that escape the bundle).
    ///  2. The engine's signing identity matches the app's: Team ID for Developer ID builds, or both unsigned/ad-hoc for dev. Each binary has its own cdhash so a strict designated-requirement match never works for dev; comparing team identifiers is what catches a binary signed by a different developer.
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

        // Both signed by the same Team ID: Developer ID release path.
        if let a = appTeam, let e = engineTeam, a == e {
            return nil
        }
        // Both ad-hoc / unsigned: dev path (`bash run.sh`). Path containment above is the only realistic guarantee, and an attacker who can write inside Contents/MacOS/ can already swap the app.
        if appTeam == nil && engineTeam == nil {
            return nil
        }
        return "Engine signing identity does not match app (engine: \(engineTeam ?? "<unsigned>"), app: \(appTeam ?? "<unsigned>"))"
    }

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

    private func spawn(binary: URL) -> EngineStartFailure? {
        Self.debug("spawn: starting engine at \(redactPathForLog(binary.path))")
        let proc = Process()
        proc.executableURL = binary
        // swift-transformers' NetworkMonitor reports offline until its first NWPathMonitor update, racing welcome-sheet "Install all" clicks; this is its escape hatch (HubApi.swift:822).
        var env = ProcessInfo.processInfo.environment
        env["CI_DISABLE_NETWORK_MONITOR"] = "1"
        // Restructure folder-granularity: the engine reads FILEID_RESTRUCTURE_GRANULARITY at plan time, so the saved choice is passed at spawn. "normal"/unset is the calibrated default; only a validated non-default value is forwarded.
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
        Self.refuseSigpipe(on: inPipe)
        let outPipe = Pipe()
        let errPipe = Pipe()
        proc.standardInput = inPipe
        proc.standardOutput = outPipe
        proc.standardError = errPipe

        // IPC flows over stderr (see IPCSink for the .app fd-1 rationale). stdout is unused: drain it and disarm on EOF, otherwise the closed fd stays readable and the handler busy-spins after the engine exits.
        outPipe.fileHandleForReading.readabilityHandler = { handle in
            if handle.availableData.isEmpty {
                handle.readabilityHandler = nil
            }
        }
        // `scanned` = leading buffer bytes already known newline-free, so the drain resumes the newline search instead of re-scanning the buffer every tick (O(n²) for a large frame).
        let stderrBuffer = MutexBox((data: Data(), scanned: 0))
        // 64 MiB inbound frame cap, matching the Windows app and the engine's command-read cap. A whole-library restructurePlan blows past a 1 MiB guard, which silently dropped the frame and wedged the UI. Still bounded so an engine that never emits a newline can't OOM the app; an oversize frame surfaces a visible error.
        let maxFrameBytes = 64 * 1024 * 1024
        // Hand decoded events to the serial pump (Sendable continuation captured as a local) rather than an unordered Task per event.
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
            var oversizeBytes: Int? = nil
            let lines: [Data] = stderrBuffer.withLock { state in
                state.data.append(data)
                var out: [Data] = []
                // Lines are sliced by offset and the consumed prefix dropped once per chunk; removing it per line shifted the whole buffer for every event in a burst.
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
                // A partial frame past the cap (no newline yet) means garbage or an oversize frame: drop it and resync to the next newline rather than buffer unbounded; reported below, not silently.
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
                // Only lines that can be JSON frames earn a decode attempt; anything else is chatter from an engine predating the fd-2 split (or a torn frame), logged as a truncated sample so it can't bloat app.log.
                guard line.first == 0x7B else {
                    Self.debug("ENGINE: \(Self.redactFrameSample(line))")
                    continue
                }
                if let event = try? IPCCoder.decoder.decode(IPCEvent.self, from: line) {
                    // Serial, ordered hand-off to the @MainActor pump; a video frame's base64 is decoded here, off the main actor.
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
            // NSError descriptions embed the binary path; persist domain+code only.
            let ns = error as NSError
            Self.debug("spawn: proc.run() FAILED: \(ns.domain) (\(ns.code))")
            return EngineStartFailure(.couldNotLaunch)
        }
        Self.debug("spawn: engine pid=\(proc.processIdentifier), readabilityHandler armed")
        self.process = proc
        self.stdinPipe = inPipe
        runningReachedReady = false
        return nil
    }

    /// Error kinds that are a notice, not a failure: the user's own Cancel, a scan that fell back to the CPU, a repeated Scan click (Swift sends `scan_already_queued`, Rust `scan_already_running`), a rescan with nothing new, a declined license.
    nonisolated static func isQuietNotice(_ kind: String) -> Bool {
        switch kind {
        case "prewarm_cancelled", "scan_already_queued", "scan_already_running", "rescan_no_changes",
             "model_license_not_accepted", "scan_slower_path": return true
        default: return false
        }
    }

    /// A deep-analyze error ends the run unless it is a per-file notice: the engine reports a file that changed since its scan and carries on with the rest.
    nonisolated static func deepErrorEndsRun(_ kind: String) -> Bool {
        kind.hasPrefix("deep") && kind != "deep_source_changed"
    }

    /// A write to an engine that has already exited must fail with EPIPE, not kill the app with SIGPIPE.
    nonisolated static func refuseSigpipe(on pipe: Pipe) {
        _ = fcntl(pipe.fileHandleForWriting.fileDescriptor, F_SETNOSIGPIPE, 1)
    }

    /// Frame samples can carry full user paths in JSON values (e.g. a torn deepAnalyzeProgress `currentPath`): scrub home and volume prefixes before they persist in app.log. External-volume paths carry no username but volume + share names identify the user's NAS layout, so they collapse too.
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

    nonisolated public static func debug(_ msg: String) {
        guard !runningTests else { return }
        debugLog.withLock { $0.write(msg) }
    }

    /// A test run must not append to the person's real app.log.
    nonisolated private static let runningTests = NSClassFromString("XCTestCase") != nil

    nonisolated private static let debugLog = MutexBox(DebugLog())

    /// One formatter and one open handle for app.log, dropped whenever the file is gone (Start over, Factory Reset and Settings delete it) so a write never lands in an unlinked file.
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
            startupExits.reachedReady()
            if lastError?.kind == "engine_exited" { lastError = nil }
            restoreSavedFolderAccess()
            if !canUndoRestructure, let root = RestructureUndoJournal.pendingRoot() {
                canUndoRestructure = true
                restructureUndoRoot = root
            }
            // Don't clear the respawn budget merely on reaching Ready: a Ready→immediate-crash flap would reset it each cycle and never trip the 3-in-60s cap. Record when we became Ready; handleEngineExit clears the budget only after `stabilitySettle`.
            lastReadyAt = Date()
            refreshVLMAdvice()
            // Back from a wipe-and-rescan: auto-start the scan against the user's chosen root.
            if let root = pendingWipeAndRescanRoot {
                pendingWipeAndRescanRoot = nil
                startScan(rootURL: root)
            }
        case .progress(let p):
            // Events reach the main actor as unstructured Tasks the runtime may reorder, so a late "discovered: 100" must never overwrite the final "discovered: 4000": drop any snapshot that rolls a counter (or the phase) backwards within the same session.
            guard p.supersedes(lastProgress) else { break }
            lastProgress = p
            // Release the App-Nap token on any terminal scan phase; cancel/fail arrive here as a phase change, not as .scanComplete.
            switch p.phase {
            case .completed, .cancelled, .failed:
                endScanActivity()
                startPendingScan()
            case .discovering, .tagging, .postScan: pendingScanRetryRoot = nil
            case .idle: break
            }
        case .phaseChanged(let phase):
            // The Swift engine ends a scan with a terminal progress snapshot; the shared engine announces it with this event alone, so the snapshot keeps its post-scan phase unless the event settles it.
            if let settled = lastProgress?.settled(by: phase) {
                lastProgress = settled
            }
            // A scan the engine rejects or cancels before its first progress snapshot has nothing to settle, but its App-Nap token must still go.
            switch phase {
            case .completed, .cancelled, .failed:
                endScanActivity()
                startPendingScan()
            case .discovering: pendingScanRetryRoot = nil
            case .idle, .tagging, .postScan: break
            }
        case .discoveryComplete:
            break
        case .fileDone:
            break
        case .batchSummary(let b):
            lastBatch = b
        case .scanComplete:
            lastTerminalEventAt = Date()
            endScanActivity()
            startPendingScan()
        case .error(let e):
            queryReplies.deliver(e)
            // Startup capability warning, not a real error: Deep Analyze can't run on this build.
            if e.kind == "deep_analyze_unavailable" {
                deepAnalyzeAvailable = false
                deepAnalyzeUnavailableReason = e.message
                return
            }
            // A busy bounce means a pass IS running, the opposite of a failure. Falling through would match the hasPrefix("face_cluster")/hasPrefix("deep") arms below and wrongly clear the in-flight flags. Log-only; the running pass emits its own terminal event.
            if e.kind == "face_clustering_busy" || e.kind == "deep_analyze_already_running" {
                Self.debug("engine bounce (benign): \(e.kind)")
                return
            }
            // A grouping pass another scan took over runs again after that scan; the in-flight flag is the only thing to settle.
            if e.kind == "face_clustering_cancelled" {
                faceClusteringInFlight = false
                return
            }
            // The user's own Cancel, a repeated click and a declined license aren't failures; the request already told them.
            if Self.isQuietNotice(e.kind) {
                Self.debug("engine notice: \(e.kind)")
                if e.kind == "scan_already_running" { retryScanStartWhileOldScanDrains() }
                if e.kind == "scan_slower_path" { scanRunsOnCPU = true }
                return
            }
            lastError = e
            if e.kind.hasPrefix("face_cluster") {
                faceClusteringInFlight = false
            }
            if Self.deepErrorEndsRun(e.kind) {
                // A deep-analyze error (e.g. unknown model kind → "deep_invalid") means .deepAnalyzeComplete, the only clearer of these flags, never arrives: clear them here or the UI stays stuck "analyzing…".
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
            // A describe run's model load reports progress below 1.0 and never a final 1.0, so the bar would stay "Downloading 99%" once the model works.
            clearModelDownloadProgress(forModelKind: p.modelKind)
            beginDeepAnalyzeActivity()
            // First per-file progress arrived: clear the "Starting…" card so the progress card takes over.
            deepAnalyzeStarting = nil
        case .deepAnalyzeFileDone(let d):
            // 500 ms throttle, else SwiftUI's AttributeGraph overflows over a fast Deep Analyze run.
            let now = Date()
            if now.timeIntervalSince(lastDeepAnalyzeFileDoneAt) >= 0.5 {
                deepAnalyzeLast = d
                lastDeepAnalyzeFileDoneAt = now
            }
        case .deepAnalyzeComplete(let c):
            deepAnalyzeComplete = c
            clearModelDownloadProgress(forModelKind: c.modelKind)
            deepAnalyzeInFlight = false
            endDeepAnalyzeActivity()
            lastTerminalEventAt = Date()
            deepAnalyzeProgress = nil
            deepAnalyzeStarting = nil
        case .modelDownloadProgress(let p):
            // Clear the bar when the download completes so the picker doesn't show a stale "Downloading 100%"; also cleared on engine exit (handleEngineExit).
            modelDownloadProgress = p.fraction >= 1.0 ? nil : p
        case .queueState(let q):
            queueState = q
            if q.running?.category == .faceCluster { faceClusteringInFlight = true }
        // Restructure butler replies (handleEvent runs on the main actor, so these assignments are isolation-safe).
        case .restructurePlan(let plan):
            restructurePlan = plan
            restructurePlanSignal &+= 1
        case .restructureApplyResult(let result):
            restructureApplyResult = result
            restructureApplyResultSignal &+= 1
            // An apply that moved files makes the run undoable; the undo's own reply clears it.
            if undoRestructureInFlight {
                undoRestructureInFlight = false
                // A cancelled or partially-failed undo must KEEP the affordance: Restructure.undoLast clears the on-disk journal only when the undo completed with zero failures, so the remaining files are still reversible. Clearing unconditionally stranded a half-reverted library with no UI path back, since this flag is only set by a forward apply and never re-seeded from disk. Mirrors Windows NextCanUndoRestructure and Linux undo_fully_completed.
                canUndoRestructure = result.cancelled || result.failed > 0
            } else {
                canUndoRestructure = result.applied > 0
            }
            restructureUndoRoot = nil
        case .bulkActionResult(let result):
            bulkActionResult = result
            bulkActionResultSignal &+= 1
            resumeBulkResultWaiters(with: result)
        // Remaining Windows-originated reply events: the mac app's equivalent flows are synchronous, so they aren't consumed here, but they're decoded so a shared engine doesn't wedge the wire.
        case .rootsList(let list):
            guard list.requestID.hasPrefix("roots-") else { break }
            libraryRoots = list.roots
        case .modelCatalog(let catalog):
            for model in catalog.models { if let result = model.benchmark { benchmarkResults[model.id] = result } }
            if catalog.requestID.hasPrefix(Self.vlmAdviceRequestPrefix) { vlmAdvice = VLMAdvice(catalog) }
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
                benchmarkMessage = (result.modelKind, result.message ?? "The benchmark didn’t finish.")
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

    /// Clears UI state whose only other clearer is an engine terminal event. A dead engine can't emit those, so a mid-job crash would wedge Deep Analyze / People across the respawn. Both the crash leg and a deliberate restart run this (see `terminateRunningEngine`).
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
        // A dead engine emits no terminal event, so release both App-Nap tokens here (both helpers no-op when not held).
        endScanActivity()
        pendingScanRoot = nil
        pendingScanRetryRoot = nil
        endDeepAnalyzeActivity()
        // Likewise the undo affordance: a crash mid-undo never emits the terminal restructureApplyResult, so without the reset the NEXT apply's result is mis-attributed as the dead undo's.
        undoRestructureInFlight = false
        // A crash mid-download leaves a stale sub-1.0 progress that defeats the WelcomeSheet VLM "no response" watchdog and shows a phantom Deep-Analyze download bar.
        modelDownloadProgress = nil
        isPaused = false
        queueState = QueueState(running: nil, pending: [], totalEtaSeconds: nil)
        // Keep the counters as context, but a scan the dead engine was running can never report its terminal phase: left live, the sidebar offers only Pause/Stop against a jobless engine and hides Scan.
        lastProgress = lastProgress?.settledAfterEngineExit()
        // Signal views that own in-flight UI (e.g. Deep Analyze's StreamCard + inert Cancel) to reset; the engine can no longer emit the terminal event.
        engineResetSignal &+= 1
    }

    @MainActor
    private func handleEngineExit(for proc: Process?) {
        // Identity guard (mirrors the Windows sender != _process check): a late EOF from a process we already terminated/replaced (a Restart, or a raced respawn) must NOT tear down the live engine or reset its in-flight UI.
        guard let proc, proc === self.process else { return }

        // Nil pipe handlers so any in-flight GCD callback short-circuits.
        (proc.standardError as? Pipe)?.fileHandleForReading.readabilityHandler = nil
        (proc.standardOutput as? Pipe)?.fileHandleForReading.readabilityHandler = nil
        process = nil
        stdinPipe = nil
        let exitedBeforeReady = !runningReachedReady

        resetStateOnlyADeadEngineCouldClear()

        // Expected exit (shutdown called or work just completed): silent respawn, no error pill, no respawn budget burned. Covers MLX's known SIGSEGV-in-static-destructor on clean exit.
        let recentClean = Date().timeIntervalSince(lastTerminalEventAt) < 5.0
        if expectedExit || recentClean {
            Self.debug("exit: clean (expectedExit=\(expectedExit) recentClean=\(recentClean))")
            expectedExit = false
            // Wipe the SQLite library before the respawn: the only safe window, since the running engine holds the WAL lock.
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

        // An engine that stops twice before it is ready is not coming up: say so now rather than after the whole respawn budget.
        if exitedBeforeReady, startupExits.exitedBeforeReady() {
            Self.debug("exit: stopped before ready \(EngineStartupExits.limit) times")
            fail(EngineStartFailure(.exitsAtStart))
            return
        }

        let now = Date()
        // A genuine recovery (Ready for ≥ stabilitySettle) clears the budget; a Ready→immediate-crash flap does not, so it keeps ticking toward the 3-in-60s terminal cap.
        if lastReadyAt != .distantPast, now.timeIntervalSince(lastReadyAt) >= Self.stabilitySettle {
            respawnAttempts.removeAll()
        }
        lastReadyAt = .distantPast
        respawnAttempts = respawnAttempts.filter {
            now.timeIntervalSince($0) < Self.respawnWindow
        }

        let attemptIdx = respawnAttempts.count
        guard attemptIdx < Self.respawnDelays.count else {
            Self.debug("respawn: budget exhausted, marking crashed")
            fail(EngineStartFailure(.keepsStopping))
            return
        }

        let delay = Self.respawnDelays[attemptIdx]
        respawnAttempts.append(now)
        state = .starting
        lastError = EngineError(
            kind: "engine_exited",
            message: "FileID’s helper stopped and is restarting…"
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

    // MARK: - App Nap activity (begin/end must stay balanced)

    func setPendingScanRootForTesting(_ url: URL?) { pendingScanRoot = url }

    var scanActivityHeld: Bool { scanActivityToken != nil }

    func beginScanActivity() {
        guard scanActivityToken == nil else { return }
        scanActivityToken = AppSleepActivity.begin(reason: "FileID is scanning files")
    }

    func endScanActivity() {
        guard let token = scanActivityToken else { return }
        AppSleepActivity.end(token)
        scanActivityToken = nil
    }

    private func beginDeepAnalyzeActivity() {
        guard deepAnalyzeActivityToken == nil else { return }
        deepAnalyzeActivityToken = AppSleepActivity.begin(reason: "FileID is describing files")
    }

    private func endDeepAnalyzeActivity() {
        guard let token = deepAnalyzeActivityToken else { return }
        AppSleepActivity.end(token)
        deepAnalyzeActivityToken = nil
    }

    // MARK: - Commands

    /// Returns false when the command could not be queued (engine down or encode failure). A later pipe-write failure terminates the captured engine generation so its normal exit path clears caller state.
    @discardableResult
    public func send(_ payload: IPCCommand.Payload) -> Bool {
        guard let pipe = stdinPipe else { return false }
        let cmd = IPCCommand(payload: payload)
        lastSentCommandID = cmd.id
        let data: Data
        do {
            data = try IPCCoder.encodeLine(cmd)
        } catch {
            FileHandle.standardError.write(Data("EngineClient send encode failed: \(error)\n".utf8))
            return false
        }

        // Off the main thread with a 10 s deadline: FileHandle.write blocks if the engine is dead with a full stdin buffer, and on timeout we kill the engine to trigger handleEngineExit's respawn.
        let writeHandle = pipe.fileHandleForWriting
        let procBox = MutexBox<Process?>(self.process)
        let done = MutexBox(false)
        // Serial queue: commands are written in submission order and never interleave. The timeout below stays on a concurrent queue so it fires as a real timer even while a blocked write holds the serial queue.
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

    /// Stops the scan that is running and starts `rootURL` once the engine reports the old one ended; with no scan running it starts at once.
    public func replaceScan(with rootURL: URL) {
        let running = lastProgress.map { [ScanPhase.discovering, .tagging, .postScan].contains($0.phase) } ?? false
        guard running else {
            startScan(rootURL: rootURL)
            return
        }
        pendingScanRoot = rootURL
        scanBusyRetriesLeft = 8
        if !send(.cancelScan) {
            pendingScanRoot = nil
            startScan(rootURL: rootURL)
            return
        }
        isPaused = false
    }

    private func startPendingScan() {
        guard let root = pendingScanRoot else { return }
        pendingScanRoot = nil
        pendingScanRetryRoot = root
        // The ended scan's remaining terminal events (phase change, scanComplete) follow its first one within moments; a start sent now would have its App-Nap token released by them.
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard let self, self.pendingScanRetryRoot == root else { return }
            self.clearProgress()
            self.startScan(rootURL: root)
        }
    }

    private var pendingScanRetryRoot: URL?

    /// The engine can report the old scan ended a moment before it frees its slot, so a start sent right then bounces; try again a few times rather than dropping the user's folder.
    private func retryScanStartWhileOldScanDrains() {
        guard let root = pendingScanRetryRoot else { return }
        if let phase = lastProgress?.phase, [.discovering, .tagging, .postScan].contains(phase) {
            pendingScanRetryRoot = nil
            return
        }
        guard scanBusyRetriesLeft > 0 else {
            pendingScanRetryRoot = nil
            endScanActivity()
            lastError = EngineError(kind: "scan_not_started",
                                    message: "The scan couldn’t start because the last one was still stopping. Press Scan again.")
            return
        }
        scanBusyRetriesLeft -= 1
        endScanActivity()
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 400_000_000)
            guard let self, self.pendingScanRetryRoot == root else { return }
            self.startScan(rootURL: root)
        }
    }

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
            message: "FileID can’t open that folder any more. Choose the folder again."
        )
    }

    private func grantSavedFolderAccess(path: String, bookmark: Data) {
        _ = send(.grantFolderAccess(rootPath: path, rootBookmark: bookmark))
    }

    public func startScan(rootURL: URL) {
        isPaused = false
        // A Describe result belongs to the library as it was; once a new scan starts it must not outrank the scan's own state in the sidebar.
        deepAnalyzeComplete = nil
        scanRunsOnCPU = false
        let displayPath = rootURL.path
        // The persisted app-scoped bookmark stays in the app; the engine gets a separate implicit-scope bookmark that can cross the process boundary.
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
                        message: "FileID couldn’t retain access to that folder. Choose the folder again."
                    )
                }
                return
                #else
                resolvedPath = displayPath
                #endif
            }
            await MainActor.run {
                // Begin the App-Nap token only if the command actually left the app; a dropped send would leave the token held with no scan and no terminal event to release it.
                if client.send(.startScan(rootPath: resolvedPath, rootBookmark: rootBookmark,
                                          rootDisplay: displayPath,
                                          rescan: false, excludedPaths: nil)) {
                    client.beginScanActivity()
                } else {
                    client.lastError = EngineError(
                        kind: "engine_exited",
                        message: "The scan couldn’t start because FileID’s helper isn’t running. Wait a moment and press Scan again."
                    )
                }
            }
        }
    }

    public func pause()    { isPaused = true;  send(.pauseScan)  }
    public func resume()   { isPaused = false; send(.resumeScan) }
    /// Stops the scan only; a folder move or Describe Files run keeps going.
    public func cancelScan() {
        isPaused = false
        send(.cancelScan)
    }

    /// Stops everything the engine is doing (Chat's "stop all").
    public func cancel() {
        isPaused = false
        send(.cancelScan)
        send(.cancelRestructure)
        send(.deepAnalyzeCancel)
    }
    public func shutdown() {
        // Latch the expected-exit suppression only when the shutdown command actually left the app; otherwise a genuine crash racing the respawn window would be masked as a clean exit and skip the error pill.
        if send(.shutdown) {
            expectedExit = true
        }
    }

    /// Factory Reset: terminate the engine, wipe the Application Support folder except the downloaded description models, purge all UserDefaults, and quit.
    public func factoryResetAndQuit() {
        terminateRunningEngine()
        SpotlightIndexer.wipe()
        let fm = FileManager.default
        if (try? ReadOnlyLocations.requireWritable(AppSupportPath.fileID)) != nil {
            FactoryReset.erase(root: AppSupportPath.fileID, keeping: [ModelCachePaths.huggingFaceRoot], fileManager: fm)
        }
        if let bundleID = Bundle.main.bundleIdentifier {
            UserDefaults.standard.removePersistentDomain(forName: bundleID)
            UserDefaults.standard.synchronize()
        }
        #if os(macOS)
        NSApplication.shared.terminate(nil)
        #endif
    }

    /// Wipes the SQLite library + scan logs and triggers a fresh scan of `rootURL` once the engine has restarted. The engine must exit before the files can be deleted (it holds the WAL lock), so: shutdown → exit → handleEngineExit deletes the files → respawn → on `.ready` the new scan starts. Cancels any in-flight scan first.
    public func wipeAndRescan(rootURL: URL) {
        if let p = lastProgress, p.phase == .discovering || p.phase == .tagging || p.phase == .postScan {
            send(.cancelScan)
        }
        // Snapshot the bookmark + display path NOW: by the time the restarted engine is ready the security-scoped resource would have to be re-acquired, via the same bookmark-resolve path as `startScan`.
        pendingWipeAndRescanRoot = rootURL
        // Engine already down (respawn backoff or exhausted budget): the shutdown would be silently dropped and no exit would run the wipe, but no WAL lock is held either, so wipe directly and respawn.

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

    /// Deletes the SQLite library + WAL/SHM siblings + the engine and app logs. Only safe when the engine isn't running (SQLite holds the lock).
    private static func deleteLibraryFiles() {
        let fm = FileManager.default
        let root = AppSupportPath.fileID
        let candidates = [
            "fileid.sqlite",
            "fileid.sqlite-wal",
            "fileid.sqlite-shm"
        ] + EngineLogFiles.removableNames(in: root.appendingPathComponent("logs", isDirectory: true)).map { "logs/" + $0 }
        for name in candidates {
            let url = root.appendingPathComponent(name)
            if (try? ReadOnlyLocations.requireWritable(url)) != nil {
                try? fm.removeItem(at: url)
            }
        }
        // The Spotlight items mirror the rows just wiped; without this, captions/tags/paths of wiped files stay queryable in ⌘Space.
        SpotlightIndexer.wipe()
    }

    // In-flight flags arm only when send() succeeded: a click during respawn backoff (stdinPipe nil) used to set the flag, drop the command, and wedge the UI with no job running.
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
        // Every current call site is a whole-library run, so the persisted Deep Analyze exclusion list is threaded through this choke point, sent as nil (omitted) when empty and ignored engine-side whenever fileIDs is present.
        let excluded = DeepAnalyzeSettings.shared.excludedFolders
        guard send(.deepAnalyzeAll(modelKind: modelKind, skipExisting: skipExisting, tagsOnly: false, proposeRenames: true, fileIDs: nil, excludedFolders: excluded.isEmpty ? nil : excluded)) else { return }
        deepAnalyzeInFlight = true
        deepAnalyzeProgress = nil
        deepAnalyzeComplete = nil
        deepAnalyzeStarting = nil
        beginDeepAnalyzeActivity()
    }

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

    /// Asks the engine to compute a restructure plan for `libraryRoot`; the reply lands on `restructurePlan` and bumps `restructurePlanSignal`. False when the command never left the app so the caller can stop its "computing…" spinner.
    @discardableResult
    public func planRestructure(libraryRoot: String) -> Bool {
        send(.planRestructure(libraryRoot: libraryRoot, supportsPagedPlans: true))
    }

    /// Applies the selected `moves` through the engine butler. macOS does real on-disk moves only: the engine rejects `useSymlinks: true` with an error rather than silently moving; the parameter exists for wire parity with Windows. The reply lands on `restructureApplyResult` and bumps `restructureApplyResultSignal`.
    @discardableResult
    public func applyRestructure(libraryRoot: String, moves: [RestructureMove],
                                 useSymlinks: Bool = false,
                                 planID: String? = nil) -> Bool {
        send(.applyRestructure(
            libraryRoot: libraryRoot, moves: moves,
            useSymlinks: useSymlinks, planID: planID))
    }

    /// Reverses the most recent applyRestructure by replaying the engine's on-disk undo journal. The reply lands on `restructureApplyResult` (applied = files moved back) and clears `canUndoRestructure`.
    @discardableResult
    public func undoRestructure(libraryRoot: String) -> Bool {
        // Arm the in-flight flag ONLY on a successful send: a failed send (engine respawning) would latch it and mis-attribute the NEXT apply's result as the undo's, leaving canUndoRestructure false while the journal is still undoable.
        let sent = send(.undoRestructure(libraryRoot: libraryRoot))
        if sent { undoRestructureInFlight = true }
        return sent
    }

    /// Cooperatively cancels the active restructure plan/apply/undo, never a library scan (`cancel()` → `.cancelScan`; the schema requires `.cancelRestructure` stay isolated). The engine finishes the move in flight (each durable before the next cancel-poll) and replies with a terminal `restructureApplyResult` with `cancelled` true.
    public func cancelRestructure() {
        send(.cancelRestructure)
    }

    /// Pre-fetches a VLM's weights without inference, so first-launch downloads happen up front (welcome sheet) instead of stalling the first Deep Analyze run. Progress arrives as `modelDownloadProgress` events.
    @discardableResult
    public func prewarmModel(_ modelKind: String) -> Bool {
        guard ModelLicenseGate.ensureAccepted(for: AIModelKind.migrated(rawValue: modelKind)) else { return false }
        return send(.prewarmModel(modelKind: modelKind))
    }

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

    public func refreshLibraryRoots() {
        send(.listRoots(requestID: "roots-" + UUID().uuidString.prefix(8).lowercased()))
    }

    public func refreshModelCatalog() {
        send(.listModels(requestID: "catalog-" + UUID().uuidString.prefix(8).lowercased(), role: nil))
    }

    static let vlmAdviceRequestPrefix = "advice-"

    /// Asks the engine which vision models this Mac can run, and which loads with the memory free right now.
    public func refreshVLMAdvice() {
        send(.listModels(requestID: Self.vlmAdviceRequestPrefix + UUID().uuidString.prefix(8).lowercased(), role: "vision"))
    }

    /// Cancels a running prewarm at the next Task.checkCancellation inside swift-transformers' Hub fetcher (typically <1 s). `nil` cancels every in-flight prewarm.
    public func cancelPrewarm() {
        send(.cancelPrewarm(modelKind: nil))
    }

    #if DEBUG
    func selfShotSetModelProgress(_ progress: ModelDownloadProgress) { modelDownloadProgress = progress }
    #endif

    /// Drops a stale `modelDownloadProgress` left by a cancelled prewarm. The engine auto-clears the bar only at 1.0 (or on exit), so a user Cancel leaves the last fraction with the same modelKind and would fool WelcomeSheet's retry watchdogs into thinking a fresh download is progressing, arming neither the "no response" nor "stalled" timer. A modelKind clears only that model's residue; nil clears whatever is showing.
    public func clearModelDownloadProgress(forModelKind modelKind: String? = nil) {
        if let modelKind, modelDownloadProgress?.modelKind != modelKind { return }
        modelDownloadProgress = nil
    }
}

extension ScanProgress {
    func settled(by phase: ScanPhase) -> ScanProgress? {
        switch phase {
        case .completed, .cancelled, .failed: break
        case .idle, .discovering, .tagging, .postScan: return nil
        }
        switch self.phase {
        case .discovering, .tagging, .postScan:
            return ScanProgress(sessionID: sessionID, phase: phase, total: total, discovered: discovered,
                                processed: processed, failed: failed, filesPerSecond: 0, etaSeconds: nil,
                                residentMB: residentMB, availableMB: availableMB)
        case .completed, .cancelled, .failed, .idle: return nil
        }
    }

    func settledAfterEngineExit() -> ScanProgress {
        switch phase {
        case .completed, .cancelled, .failed, .idle: return self
        case .discovering, .tagging, .postScan:
            return ScanProgress(sessionID: sessionID, phase: .failed, total: total, discovered: discovered,
                                processed: processed, failed: failed, filesPerSecond: 0, etaSeconds: nil,
                                residentMB: residentMB, availableMB: availableMB)
        }
    }

    /// True when `self` is fresher than `prior` and should replace it. Snapshots arrive as unstructured Tasks the runtime may reorder, and a stale one must never roll a counter backwards. Within a session counters are monotonic and the phase only advances; a different session always supersedes.
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
