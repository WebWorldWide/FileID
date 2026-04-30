// Spawns + supervises the engine child process. Auto-respawns with
// backoff. State and events are observable on MainActor.
import Foundation
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
    public private(set) var lastError: EngineError?
    public private(set) var lastBatch: BatchSummary?
    public private(set) var lastFaceClustering: FaceClusteringResult?
    public private(set) var faceClusteringInFlight: Bool = false
    /// Engine doesn't echo paused state, so the app mirrors it locally.
    public private(set) var isPaused: Bool = false

    public private(set) var deepAnalyzeProgress: DeepAnalyzeProgress?
    public private(set) var deepAnalyzeLast: DeepAnalyzeFileDone?
    public private(set) var deepAnalyzeComplete: DeepAnalyzeComplete?
    public private(set) var modelDownloadProgress: ModelDownloadProgress?
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

    // MARK: - Auto-pilot ("Organize Everything")
    //
    // When the user clicks "Organize Everything" instead of plain
    // "Start Scan", the engine chains all four stages automatically:
    //   1. Scan (already runs)
    //   2. Face clustering (already auto-triggered after scan)
    //   3. Deep Analyze on every image  ← NEW chain link
    //   4. UI flips to Restructure tab with auto-loaded proposals  ← NEW
    //
    // The flag persists across stage transitions; each event handler
    // checks it and kicks the next stage. Cleared on autoPilotCancel
    // or after the final stage finishes.
    public private(set) var autoPilotActive: Bool = false
    public private(set) var autoPilotStage: AutoPilotStage = .idle

    public enum AutoPilotStage: Sendable, Equatable {
        case idle
        case scanning
        case grouping       // face clustering
        case captioning     // deep analyze
        case proposing      // restructure proposals (handled by UI)
        case ready          // user can review + apply
    }

    public private(set) var queueState: QueueState = QueueState(
        running: nil, pending: [], totalEtaSeconds: nil
    )

    private var process: Process?
    private var stdinPipe: Pipe?

    // Up to 3 respawns within respawnWindow; cleared on .ready.
    private static let respawnDelays: [UInt64] = [1, 4, 16]
    private static let respawnWindow: TimeInterval = 60
    private var respawnAttempts: [Date] = []
    private var pendingRespawn: Task<Void, Never>?

    /// Set on shutdown() or after a "work complete" signal. Suppresses
    /// the phantom error pill after MLX's known SIGSEGV-at-exit bug.
    private var expectedExit: Bool = false
    private var lastTerminalEventAt: Date = .distantPast

    /// 2 Hz throttle on `deepAnalyzeLast`; otherwise SwiftUI's
    /// AttributeGraph overflows on a fast Deep Analyze run.
    private var lastDeepAnalyzeFileDoneAt: Date = .distantPast

    public init() {}

    public static func locateEngineBinary() -> URL? {
        let exec = Bundle.main.executablePath ?? CommandLine.arguments[0]
        let execURL = URL(fileURLWithPath: exec)
        let candidate = execURL.deletingLastPathComponent().appendingPathComponent("fileidd")
        if FileManager.default.isExecutableFile(atPath: candidate.path) {
            return candidate
        }
        let altCandidate = execURL.deletingLastPathComponent().appendingPathComponent("FileIDEngine")
        if FileManager.default.isExecutableFile(atPath: altCandidate.path) {
            return altCandidate
        }
        return nil
    }

    public func start() {
        guard let binURL = Self.locateEngineBinary() else {
            state = .crashed(reason: "Engine binary not found next to app executable")
            return
        }
        spawn(binary: binURL)
    }

    private func spawn(binary: URL) {
        Self.debug("spawn: starting engine at \(binary.path)")
        let proc = Process()
        proc.executableURL = binary
        let inPipe = Pipe()
        let outPipe = Pipe()
        let errPipe = Pipe()
        proc.standardInput = inPipe
        proc.standardOutput = outPipe
        proc.standardError = errPipe

        // IPC flows over stderr (see IPCSink for the .app fd-1 rationale).
        outPipe.fileHandleForReading.readabilityHandler = { handle in
            _ = handle.availableData
        }
        let stderrBuffer = MutexBox(Data())
        errPipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            if data.isEmpty {
                handle.readabilityHandler = nil
                Task { @MainActor [weak self] in
                    self?.handleEngineExit()
                }
                return
            }
            // Append + drain whole lines under the lock.
            let lines: [Data] = stderrBuffer.withLock { buf in
                buf.append(data)
                var out: [Data] = []
                while let nl = buf.firstIndex(of: 0x0A) {
                    let line = buf.subdata(in: buf.startIndex..<nl)
                    buf.removeSubrange(buf.startIndex...nl)
                    if !line.isEmpty { out.append(line) }
                }
                return out
            }
            for line in lines {
                if let event = try? IPCCoder.decoder.decode(IPCEvent.self, from: line) {
                    Task { @MainActor [weak self] in
                        self?.handleEvent(event)
                    }
                } else {
                    Self.debug("ENGINE: \(String(data: line, encoding: .utf8) ?? "<binary>")")
                }
            }
        }

        do {
            try proc.run()
        } catch {
            Self.debug("spawn: proc.run() FAILED: \(error)")
            state = .crashed(reason: "Failed to launch engine: \(error)")
            return
        }
        Self.debug("spawn: engine pid=\(proc.processIdentifier), readabilityHandler armed")
        self.process = proc
        self.stdinPipe = inPipe
    }

    /// Debug log at ~/Library/Application Support/FileID/logs/app.log.
    nonisolated public static func debug(_ msg: String) {
        let url = AppSupportPath.fileID.appendingPathComponent("logs/app.log")
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                  withIntermediateDirectories: true)
        let stamp = ISO8601DateFormatter().string(from: Date())
        let line = "\(stamp) \(msg)\n"
        let payload = Data(line.utf8)
        if let h = try? FileHandle(forWritingTo: url) {
            // Discarding errors here is intentional — debug logging
            // must not crash the app. `_ = try?` silences the unused-
            // result warning while keeping the no-throw guarantee.
            _ = try? h.seekToEnd()
            _ = try? h.write(contentsOf: payload)
            _ = try? h.close()
        } else {
            _ = try? payload.write(to: url)
        }
    }

    nonisolated public static func pumpDebug(_ msg: String) async { debug(msg) }

    private func handleEvent(_ event: IPCEvent) {
        switch event.payload {
        case .ready(let info):
            state = .ready(info)
            respawnAttempts.removeAll()   // reset budget on a clean handshake
        case .progress(let p):
            lastProgress = p
            // Auto-pilot: cancel + failed phases must release the
            // assistant view, otherwise the user is stuck looking at
            // "Finding people…" or similar with no way forward. The
            // explicit Cancel button on the assistant view also calls
            // cancelAutoPilot(), but a phase change from any other
            // source (e.g. engine-level cancel) needs to land here.
            if autoPilotActive, p.phase == .cancelled || p.phase == .failed {
                autoPilotActive = false
                autoPilotStage = .idle
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
            // Auto-pilot: scan ➜ face clustering already auto-fires from
            // the engine itself, so just update the visible stage.
            // BUT: if there are no faces in the scanned library, the
            // engine won't fire clustering at all and we'd hang on
            // .grouping. Watchdog kicks the next stage after 6s if no
            // clustering activity is seen.
            if autoPilotActive {
                autoPilotStage = .grouping
                let stamp = lastTerminalEventAt
                Task { @MainActor [weak self] in
                    try? await Task.sleep(nanoseconds: 6_000_000_000)
                    guard let self else { return }
                    // Still in auto-pilot, still on grouping, and no
                    // clustering even started (no inflight + no result):
                    // skip ahead.
                    if self.autoPilotActive,
                       self.autoPilotStage == .grouping,
                       !self.faceClusteringInFlight,
                       self.lastFaceClustering == nil,
                       self.lastTerminalEventAt == stamp {
                        if self.deepAnalyzeAvailable {
                            self.autoPilotStage = .captioning
                            let activeKind = UserDefaults.standard.string(forKey: "deepAnalyzeActiveModel")
                                ?? AIModelKind.qwen2VL3B.rawValue
                            self.deepAnalyzeAll(modelKind: activeKind, skipExisting: true)
                        } else {
                            self.autoPilotStage = .ready
                        }
                    }
                }
            }
        case .error(let e):
            // Engine startup capability warning: not a real error, just a
            // signal that Deep Analyze can't run on this build.
            if e.kind == "deep_analyze_unavailable" {
                deepAnalyzeAvailable = false
                deepAnalyzeUnavailableReason = e.message
                return
            }
            lastError = e
            if e.kind.hasPrefix("face_cluster") {
                faceClusteringInFlight = false
                // Auto-pilot: a clustering error means we won't get
                // .faceClusteringComplete. Skip captioning and flip to
                // ready so the user can still see what was scanned.
                if autoPilotActive {
                    autoPilotStage = .ready
                }
            }
            if e.kind.hasPrefix("deep") && autoPilotActive {
                // Deep Analyze failure during auto-pilot — same idea.
                autoPilotStage = .ready
            }
        case .log:
            break
        case .faceClusteringComplete(let summary):
            lastFaceClustering = summary
            faceClusteringInFlight = false
            lastTerminalEventAt = Date()
            // Auto-pilot used to chain straight into Deep Analyze here.
            // That's gone now — Deep Analyze waits until the user has
            // named at least one person. Auto-pilot just flips to ready
            // and the user takes over.
            if autoPilotActive {
                autoPilotStage = .ready
            }
        case .deepAnalyzeStarting(let s):
            deepAnalyzeStarting = s
            deepAnalyzeInFlight = true
        case .deepAnalyzeProgress(let p):
            deepAnalyzeProgress = p
            deepAnalyzeInFlight = true
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
            lastTerminalEventAt = Date()
            deepAnalyzeProgress = nil
            deepAnalyzeStarting = nil
            // Auto-pilot: captioning ➜ proposing ➜ ready, with a tiny
            // delay so SwiftUI animates the stage transition. Re-checks
            // autoPilotActive after the sleep so a Cancel between the
            // two transitions doesn't snap the user back into the
            // assistant view post-cancel.
            if autoPilotActive {
                autoPilotStage = .proposing
                Task { @MainActor [weak self] in
                    try? await Task.sleep(nanoseconds: 600_000_000)
                    guard let self else { return }
                    if self.autoPilotActive {
                        self.autoPilotStage = .ready
                    }
                }
            }
        case .modelDownloadProgress(let p):
            modelDownloadProgress = p
        case .queueState(let q):
            queueState = q
        }
    }

    @MainActor
    private func markCrashed(reason: String) {
        state = .crashed(reason: reason)
    }

    @MainActor
    private func handleEngineExit() {
        // Nil pipe handlers so any in-flight GCD callback short-circuits.
        if let proc = process {
            (proc.standardError as? Pipe)?.fileHandleForReading.readabilityHandler = nil
            (proc.standardOutput as? Pipe)?.fileHandleForReading.readabilityHandler = nil
        }
        process = nil
        stdinPipe = nil

        // Expected exit (shutdown called or work just completed): silent
        // re-spawn, no error pill, no respawn budget burned. Covers
        // MLX's known SIGSEGV-in-static-destructor on clean exit.
        let recentClean = Date().timeIntervalSince(lastTerminalEventAt) < 5.0
        if expectedExit || recentClean {
            Self.debug("exit: clean (expectedExit=\(expectedExit) recentClean=\(recentClean))")
            expectedExit = false
            state = .starting
            pendingRespawn?.cancel()
            pendingRespawn = Task { @MainActor [weak self] in
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                self?.start()
            }
            return
        }

        let now = Date()
        respawnAttempts = respawnAttempts.filter {
            now.timeIntervalSince($0) < Self.respawnWindow
        }

        let attemptIdx = respawnAttempts.count
        guard attemptIdx < Self.respawnDelays.count else {
            state = .crashed(reason: "Engine exited \(Self.respawnDelays.count)× within \(Int(Self.respawnWindow))s; auto-respawn budget exhausted. Relaunch the app to retry.")
            Self.debug("respawn: budget exhausted, marking crashed")
            return
        }

        let delay = Self.respawnDelays[attemptIdx]
        respawnAttempts.append(now)
        state = .starting
        lastError = EngineError(
            kind: "engine_exited",
            message: "Engine exited unexpectedly. Auto-respawn attempt \(attemptIdx + 1)/\(Self.respawnDelays.count) in \(delay)s…"
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

    // MARK: - Commands

    public func send(_ payload: IPCCommand.Payload) {
        guard let pipe = stdinPipe else { return }
        let cmd = IPCCommand(payload: payload)
        do {
            let data = try IPCCoder.encodeLine(cmd)
            try pipe.fileHandleForWriting.write(contentsOf: data)
        } catch {
            FileHandle.standardError.write(Data("EngineClient send failed: \(error)\n".utf8))
        }
    }

    /// Wipe progress + last batch + error (e.g. when the user picks a new folder).
    public func clearProgress() {
        lastProgress = nil
        lastBatch = nil
        lastError = nil
    }

    public func clearLastError() { lastError = nil }

    /// V3: Start Scan is the single CTA and auto-chains by default.
    /// Sets `autoPilotActive` so faceClusteringComplete kicks off Deep
    /// Analyze automatically. Bookmark serialization is moved off the
    /// main thread — the caller doesn't await it; the engine receives
    /// the startScan command as soon as the bookmark resolves.
    public func startScan(rootURL: URL) {
        autoPilotActive = true
        autoPilotStage = .scanning
        isPaused = false
        let path = rootURL.path
        Task.detached(priority: .userInitiated) { [weak self] in
            do {
                let bookmark = try rootURL.bookmarkData(
                    options: [],
                    includingResourceValuesForKeys: nil,
                    relativeTo: nil
                )
                await MainActor.run {
                    self?.send(.startScan(rootBookmark: bookmark, rootPathDisplay: path))
                }
            } catch {
                await MainActor.run {
                    self?.lastError = EngineError(
                        kind: "bookmark_create_failed",
                        message: "\(error)", path: path
                    )
                    self?.autoPilotActive = false
                    self?.autoPilotStage = .idle
                }
            }
        }
    }

    /// Cancel any in-flight stage chain. The current stage's data
    /// stays intact; subsequent stage-complete events won't kick off
    /// the next stage. The Sidebar's Cancel button calls this in
    /// addition to engine.cancel().
    public func cancelAutoPilot() {
        autoPilotActive = false
        autoPilotStage = .idle
    }

    public func pause()    { isPaused = true;  send(.pauseScan)  }
    public func resume()   { isPaused = false; send(.resumeScan) }
    public func cancel()   { isPaused = false; send(.cancelScan) }
    public func shutdown() {
        expectedExit = true
        send(.shutdown)
    }

    public func runFaceClustering() {
        guard !faceClusteringInFlight else { return }
        faceClusteringInFlight = true
        send(.runFaceClustering)
    }

    public func deepAnalyzeFile(fileID: Int64, modelKind: String) {
        deepAnalyzeInFlight = true
        deepAnalyzeProgress = nil
        deepAnalyzeComplete = nil
        deepAnalyzeStarting = nil
        send(.deepAnalyzeFile(fileID: fileID, modelKind: modelKind))
    }

    public func deepAnalyzeFolder(prefix: String, modelKind: String) {
        deepAnalyzeInFlight = true
        deepAnalyzeProgress = nil
        deepAnalyzeComplete = nil
        deepAnalyzeStarting = nil
        send(.deepAnalyzeFolder(pathPrefix: prefix, modelKind: modelKind))
    }

    public func deepAnalyzeAll(modelKind: String, skipExisting: Bool) {
        deepAnalyzeInFlight = true
        deepAnalyzeProgress = nil
        deepAnalyzeComplete = nil
        deepAnalyzeStarting = nil
        send(.deepAnalyzeAll(modelKind: modelKind, skipExisting: skipExisting))
    }

    public func deepAnalyzeCancel() {
        send(.deepAnalyzeCancel)
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
