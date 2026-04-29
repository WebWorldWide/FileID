import SwiftUI
import AppKit
import SwiftData

@MainActor
class AppViewModel: ObservableObject {

    // MARK: - Published State
    @Published var isProcessing      = false
    @Published var isPaused          = false {
        didSet { atomicLock.lock(); _pausedAtomic = isPaused; atomicLock.unlock() }
    }
    @Published var isCancelled       = false {
        didSet { atomicLock.lock(); _cancelledAtomic = isCancelled; atomicLock.unlock() }
    }
    // True while `startProcessing` is clearing the previous scan's SwiftData
    // state. Gates the tab ZStack so `modelContext.delete` notifications fire
    // into nothing; MainWindowView shows a "Clearing previous scan…" splash.
    @Published var isWiping          = false
    @Published var processedCount    = 0
    @Published var totalCount        = 0
    @Published var logs: [String]    = []
    @Published var currentStatus     = "Ready"
    @Published var activeTab         = "Library"
    @Published var previewFile: FileRecord?
    @Published var previewList: [FileRecord] = []
    @Published var selectedPersonDetail: PersonRecord?

    func openPreview(_ file: FileRecord, in list: [FileRecord]) {
        previewList = list
        previewFile = file
    }

    func closePreview() {
        previewFile = nil
        previewList = []
    }

    func openPersonDetail(_ person: PersonRecord) {
        selectedPersonDetail = person
    }

    func closePersonDetail() {
        selectedPersonDetail = nil
    }

    @Published var searchText        = ""
    @Published var applyFilenameRename = true
    @Published var applyEXIFWrite      = true
    @Published var processingStartTime: Date?
    @Published var fileTree: [FileTreeNode] = []
    @Published var etaString           = ""
    @Published var elapsedString       = ""
    // Plain var (not @Published): every batch save bumps this, so publishing
    // would fan out to every SwiftUI view that observes AppViewModel and cause
    // a full rebuild cascade (~80 ms cadence at peak). Views instead key off
    // uiRefreshTick, the 1 s trailing-edge debounce below.
    var scanBatchCount = 0
    @Published var uiRefreshTick: Int  = 0
    private var uiRefreshTask: Task<Void, Never>?
    @Published var sortByAesthetic     = false
    @Published var discoveredCount     = 0
    @Published var scanPhase: ScanPhase = .idle
    @Published var clusteringCompletedAt: Date?
    @Published var clusteringFacesDone: Int = 0
    @Published var clusteringFacesTotal: Int = 0
    // Reset per-phase by enterPhase() so a slow clustering pass can't poison the tagging ETA.
    @Published var phaseStartTime: Date?
    @Published var namingDone:  Int = 0
    @Published var namingTotal: Int = 0
    @Published var scoringDone:  Int = 0
    @Published var scoringTotal: Int = 0

    func enterPhase(_ next: ScanPhase) {
        scanPhase      = next
        phaseStartTime = Date()
        switch next {
        case .naming:     namingDone  = 0; namingTotal  = 0
        case .scoring:    scoringDone = 0; scoringTotal = 0
        case .clustering: clusteringFacesDone = 0; clusteringFacesTotal = 0
        default: break
        }
    }

    private var activityToken: NSObjectProtocol?

    // Hot-path writers are nonisolated and lock-guarded; an 80 ms @MainActor
    // timer drains the counters into @Published state so the UI ticks steadily
    // regardless of scan throughput.
    private let atomicLock = NSLock()
    nonisolated(unsafe) private var _processedAtomic: Int = 0
    nonisolated(unsafe) private var _discoveredAtomic: Int = 0
    nonisolated(unsafe) private var _treeQueue: [URL] = []
    // Mirror of @Published isCancelled / isPaused readable from any thread
    // without an actor hop. The discovery / tagging loops poll these every
    // N files instead of awaiting MainActor on each iteration — the per-file
    // MainActor hop was the dominant cost in Discovery on big libraries
    // (58K files × ~5 ms per hop = 5 minutes of pure scheduling).
    nonisolated(unsafe) private var _cancelledAtomic: Bool = false
    nonisolated(unsafe) private var _pausedAtomic: Bool = false
    nonisolated var isCancelledAtomic: Bool {
        atomicLock.lock(); defer { atomicLock.unlock() }
        return _cancelledAtomic
    }
    nonisolated var isPausedAtomic: Bool {
        atomicLock.lock(); defer { atomicLock.unlock() }
        return _pausedAtomic
    }
    private var drainTask: Task<Void, Never>?

    nonisolated func bumpDiscoveredAtomic() {
        atomicLock.lock()
        _discoveredAtomic &+= 1
        atomicLock.unlock()
    }

    /// Bulk version — Discovery batches 1 024 files per `nextBatch` call;
    /// taking the lock once per batch instead of once per file removes 1 023
    /// pointless lock acquisitions from the hot path.
    nonisolated func bumpDiscoveredAtomic(by n: Int) {
        guard n > 0 else { return }
        atomicLock.lock()
        _discoveredAtomic &+= n
        atomicLock.unlock()
    }

    // One call, one lock — keeps the processed count and the tree-progress
    // queue from drifting apart between drain ticks.
    nonisolated func recordFileCompleted(fileURL: URL) {
        atomicLock.lock()
        _processedAtomic &+= 1
        if _treeQueue.count >= 50_000 {
            _treeQueue.removeFirst(_treeQueue.count - 49_999)
        }
        _treeQueue.append(fileURL)
        atomicLock.unlock()
    }

    private var drainTickCounter = 0

    // Drain the atomic counters + tree-progress queue onto the main actor.
    // Every 6th tick (~500 ms) also rebuilds the tree view and refreshes ETA,
    // keeping the "N / M" counter and the hierarchy pane on one clock.
    private func drainAtomicState() {
        atomicLock.lock()
        let processed  = _processedAtomic
        let discovered = _discoveredAtomic
        let queue      = _treeQueue
        _treeQueue.removeAll(keepingCapacity: true)
        atomicLock.unlock()

        if processed != processedCount { processedCount = processed }
        if discovered != discoveredCount { discoveredCount = discovered }
        // Discovery and tagging interleave, so the denominator must follow
        // discovered for the whole scan — a phase-gated update would freeze
        // totalCount at the first tagged file.
        if discovered > totalCount { totalCount = discovered }

        for url in queue { recordTreeProgress(fileURL: url, done: true) }

        drainTickCounter &+= 1
        if drainTickCounter >= 6 {
            drainTickCounter = 0
            // Tree rebuild is suppressed during scan — see finishNamingPhase
            // for the one-shot rebuild at .ready. Why: SwiftUI's AttributeGraph
            // overflows after thousands of rebuilds of an OutlineGroup inside
            // a List+Section (TransitionBox) on large libraries, crashing
            // with AG::data::table::grow_region precondition. ETA and counter
            // still tick on the 500 ms cadence.
            if !isProcessing { rebuildTreeFromAccumulator() }
            updateETA()
        }

        if let pending = pendingScanFolderURL,
           !AIModelDownloadService.shared.isAnyActive {
            pendingScanFolderURL = nil
            log("Downloads complete — starting queued scan.")
            startProcessing(folderURL: pending)
        }
    }

    private func startDrainTimer() {
        drainTask?.cancel()
        drainTickCounter = 0
        drainTask = Task { [weak self] in
            while !Task.isCancelled {
                if let self { await MainActor.run { self.drainAtomicState() } }
                try? await Task.sleep(nanoseconds: 80_000_000)
            }
        }
    }

    private func stopDrainTimer() {
        drainTask?.cancel()
        drainTask = nil
    }

    func beginScanActivity() {
        activityToken = ProcessInfo.processInfo.beginActivity(
            options: [.userInitiated, .idleSystemSleepDisabled],
            reason: "FileID scan in progress"
        )
    }

    func endScanActivity() {
        if let t = activityToken { ProcessInfo.processInfo.endActivity(t); activityToken = nil }
    }

    enum ScanPhase: String {
        case idle        = "Idle"
        case discovering = "Discovering"
        case tagging     = "Tagging"
        case clustering  = "Clustering"
        case naming      = "Naming"
        case scoring     = "Scoring"
        case ready       = "Ready"
    }

    var currentFolderURL: URL?
    var modelContainer: ModelContainer?
    var dataStore: FileIDDataStore?

    // Stashed scan URL deferred until any in-flight model download finishes.
    @Published var pendingScanFolderURL: URL?

    @Published var downloaderPulse: Int = 0

    func configureStores(container: ModelContainer) {
        self.modelContainer = container
        if dataStore == nil {
            dataStore = FileIDDataStore(modelContainer: container)
        }
        Task {
            await FaceClusteringService.setUp(modelContainer: container)
        }
    }

    // MARK: - Tree Node

    struct FileTreeNode: Identifiable {
        let id: String
        let name: String
        var children: [FileTreeNode]?
        var done: Int  = 0
        var total: Int = 0
    }

    private var treeUpdateTask: Task<Void, Never>?

    // Key = folder path relative to currentFolderURL. Mutated by MediaProcessor
    // so rebuilds are O(folders), not O(files).
    //
    // SAFETY CAP: treeAccumulator is the source for an OutlineGroup-inside-
    // List render — see DECISIONS.md "no live tree rebuilds during scan."
    // The 6-component path cap below limits depth, but a library with
    // millions of unique folders (a deduplication archive, an art-history
    // database) could still explode the key count. Hard cap at 10K keys —
    // beyond that we drop new keys (existing folders keep updating). The
    // sidebar tree is for navigation, not exhaustive enumeration.
    private static let treeAccumulatorMaxKeys = 10_000
    private var treeAccumulator: [String: (done: Int, total: Int)] = [:]
    private var treeAccumulatorCapHit = false

    func recordTreeProgress(fileURL: URL, done: Bool) {
        guard let root = currentFolderURL else { return }
        let relDir = fileURL.deletingLastPathComponent().path
            .replacingOccurrences(of: root.path, with: "")
        // Cap at 6 path components so deeply-nested libraries (e.g. 15-level
        // TrueNAS chains) don't produce one accumulator key per unique path.
        // 6 levels is ample for sidebar navigation.
        let parts = relDir.split(separator: "/").map(String.init).prefix(6).map { $0 }

        var prefix = ""
        let keys = [""] + parts.map { p -> String in prefix += "/\(p)"; return prefix }
        let allowNewKeys = treeAccumulator.count < Self.treeAccumulatorMaxKeys
        for key in keys {
            if treeAccumulator[key] == nil && !allowNewKeys {
                if !treeAccumulatorCapHit {
                    treeAccumulatorCapHit = true
                    NSLog("FileID treeAccumulator: hit %d-key cap; deeper folders will not appear in the sidebar tree.",
                          Self.treeAccumulatorMaxKeys)
                }
                continue
            }
            var entry = treeAccumulator[key] ?? (done: 0, total: 0)
            entry.total += 1
            if done { entry.done += 1 }
            treeAccumulator[key] = entry
        }
    }

    private func rebuildTreeFromAccumulator() {
        guard let root = currentFolderURL else { return }

        class Node {
            var name: String; var key: String
            var children: [String: Node] = [:]
            var done = 0; var total = 0
            init(_ name: String, _ key: String) { self.name = name; self.key = key }
        }

        let rootNode = Node(root.lastPathComponent, "")
        for (key, counts) in treeAccumulator {
            let parts = key.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
                           .split(separator: "/").map(String.init)
            guard !parts.isEmpty else { continue }
            var cur = rootNode
            var curKey = ""
            for part in parts {
                curKey += "/\(part)"
                if cur.children[part] == nil { cur.children[part] = Node(part, curKey) }
                guard let next = cur.children[part] else { break }
                cur = next
            }
            cur.done  = counts.done
            cur.total = counts.total
        }
        // Roll up totals to root
        if let rootEntry = treeAccumulator[""] {
            rootNode.done  = rootEntry.done
            rootNode.total = rootEntry.total
        }

        func convert(_ n: Node) -> FileTreeNode {
            FileTreeNode(
                id: "root\(n.key)", name: n.name,
                children: n.children.isEmpty ? nil
                    : n.children.values.map(convert).sorted { $0.name < $1.name },
                done: n.done, total: n.total
            )
        }
        fileTree = [convert(rootNode)]
    }

    // MARK: - Pause / Resume

    func pause() {
        guard isProcessing, !isPaused else { return }
        isPaused      = true
        currentStatus = "Paused"
        log("Processing paused.")
    }

    func resume() {
        guard isPaused else { return }
        isPaused      = false
        currentStatus = "Resuming…"
        log("Processing resumed.")
    }

    func cancelProcessing() {
        guard isProcessing, !isCancelled else { return }
        isCancelled   = true
        isPaused      = false
        currentStatus = "Cancelling…"
        log("Cancellation requested.")
    }

    // MARK: - Export Report

    func exportReport() {
        guard let store = dataStore else { return }
        let logsSnapshot    = logs
        let folderSnapshot  = currentFolderURL?.path ?? "Unknown"
        Task {
            let snap = await store.reportSnapshot(categoryFor: fileIDCategory(for:))

            let catRows = snap.categories
                .map { "| \($0.0) | \($0.1) |" }
                .joined(separator: "\n")

            let md = """
            # FileID Scan Report
            **Folder:** `\(folderSnapshot)`
            **Date:** \(Date().formatted())

            ## Summary
            | Metric | Value |
            |--------|-------|
            | Total Files | \(snap.fileCount) |
            | People Identified | \(snap.peopleCount) |
            | Duplicate Groups | \(snap.duplicateGroupCount) |
            | Files Trashed | \(snap.trashedCount) |
            | Total Size | \(String(format: "%.1f", snap.totalMB)) MB |
            | Space Reclaimed | \(String(format: "%.1f", snap.reclaimMB)) MB |

            ## Categories
            | Category | Count |
            |----------|-------|
            \(catRows)

            ## Processing Log
            \(logsSnapshot.joined(separator: "\n"))
            """

            // Task inherits @MainActor — NSSavePanel runs on main thread directly.
            let panel = NSSavePanel()
            panel.nameFieldStringValue = "FileID_Report_\(Date().formatted(date: .numeric, time: .omitted).replacingOccurrences(of: "/", with: "-")).md"
            panel.allowedContentTypes  = [.plainText]
            if panel.runModal() == .OK, let url = panel.url {
                try? md.write(to: url, atomically: true, encoding: .utf8)
                NSWorkspace.shared.activateFileViewerSelecting([url])
            }
        }
    }

    // MARK: - Scan Start

    func selectFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles          = false
        panel.canChooseDirectories    = true
        panel.allowsMultipleSelection = false
        panel.message = "Select a folder of photos and videos to scan"
        if panel.runModal() == .OK, let url = panel.url {
            _ = url.startAccessingSecurityScopedResource()
            startProcessing(folderURL: url)
        }
    }

    func startProcessing(folderURL: URL) {
        guard modelContainer != nil else { return }

        // MLX + Vision fight each other for threads, so a scan waits for any
        // active download and auto-starts from the drain tick.
        if AIModelDownloadService.shared.isAnyActive {
            pendingScanFolderURL = folderURL
            isProcessing  = false
            currentStatus = "Waiting for model downloads to finish…"
            log("Scan queued — waiting for model downloads.")
            startDrainTimer()
            return
        }

        pendingScanFolderURL = nil
        AIModelDownloadService.shared.scanInProgress = true
        log("Scan started: \(folderURL.path)")
        isProcessing        = true
        isCancelled         = false
        currentFolderURL    = folderURL
        enterPhase(.discovering)
        currentStatus       = "Discovering files…"
        logs                = []
        totalCount          = 0
        processedCount      = 0
        discoveredCount     = 0
        fileTree            = []
        treeAccumulator     = [:]
        treeAccumulatorCapHit = false
        processingStartTime = Date()
        etaString           = ""
        scanBatchCount      = 0

        let capturedFolderURL = folderURL
        atomicLock.lock()
        _processedAtomic = 0
        _discoveredAtomic = 0
        _treeQueue.removeAll(keepingCapacity: true)
        atomicLock.unlock()
        startDrainTimer()
        startTreeUpdateLoop()

        guard let store = dataStore else { return }
        Task {
            // Every Start is fresh — no resume-after-cancel. The wipe runs
            // with tabs torn down so SwiftData delete notifications fire into
            // no live @Query observers.
            await MainActor.run { self.isWiping = true }
            await store.wipeForNewScan(folderPath: capturedFolderURL.path)
            FacePrintCache.removeAllAsync()
            await MainActor.run { self.isWiping = false }
            self.runScan(folderURL: capturedFolderURL)
        }
    }

    private func runScan(folderURL: URL) {
        guard let store = dataStore else { return }
        Task {
            beginScanActivity()
            let processor = MediaProcessor(viewModel: self, store: store)
            await processor.startDirectoryScan(url: folderURL)

            FolderWatcherService.shared.startWatching(url: folderURL) { newFileURL in
                Task { await processor.processSingleNewFile(url: newFileURL) }
            }

            await finishNamingPhase()
            endScanActivity()
        }
    }

    // MARK: - Naming / Review

    func finishNamingPhase() async {
        guard let store = dataStore else { return }
        enterPhase(.naming)
        currentStatus = "Naming files…"
        await MediaProcessor(viewModel: self, store: store).preparePreviewNames()

        enterPhase(.scoring)
        currentStatus = "Scoring junk…"
        await JunkScorer.scoreAll(store: store) { [weak self] done, total in
            Task { @MainActor in
                guard let self else { return }
                self.scoringDone  = done
                self.scoringTotal = total
            }
        }

        await store.markScanSessionComplete()

        clusteringCompletedAt = Date()

        enterPhase(.ready)
        phaseStartTime = nil
        currentStatus = "Ready for Review"
        isProcessing  = false
        // Land on the tab literally labelled "Ready for Review" so the end
        // of a scan is visually unambiguous — Library at rest made users
        // think the app had let go of the drive.
        activeTab = "Review"
        stopTreeUpdate()
        drainAtomicState()
        stopDrainTimer()
        // One-shot tree rebuild after scan completes. During the scan the
        // rebuild is suppressed to keep SwiftUI's AttributeGraph from
        // overflowing; here we paint the final snapshot once and it stays
        // static until the next scan.
        rebuildTreeFromAccumulator()
        scanBatchCount += 1

        AIModelDownloadService.shared.scanInProgress = false
    }

    func executeRenaming() async {
        guard let store = dataStore, let url = currentFolderURL else { return }
        currentStatus = "Applying changes…"
        await MediaProcessor(viewModel: self, store: store).applyIdentityNames(folderURL: url)
        currentStatus = "Complete"
    }

    // Runs separately from `isProcessing` (which is gated on scan). The pass
    // holds its own Task so the user can cancel it from Settings.
    @Published var deepAnalyzeRunning = false
    private var deepAnalyzeTask: Task<Void, Never>?

    func runDeepAnalyzeNow() {
        guard !isProcessing, !deepAnalyzeRunning, let store = dataStore else { return }
        deepAnalyzeRunning = true
        currentStatus = "Deep Analyze…"
        deepAnalyzeTask = Task { [weak self] in
            guard let self else { return }
            let processor = MediaProcessor(viewModel: self, store: store)
            await processor.runDeepAnalyzePassIfEnabled()
            await MainActor.run {
                self.deepAnalyzeRunning = false
                self.deepAnalyzeTask = nil
                self.currentStatus = Task.isCancelled
                    ? "Deep Analyze cancelled"
                    : "Deep Analyze complete"
            }
        }
    }

    func cancelDeepAnalyze() {
        deepAnalyzeTask?.cancel()
    }

    func approveChanges() async {
        FolderWatcherService.shared.stopWatching()
        currentFolderURL?.stopAccessingSecurityScopedResource()
        isProcessing  = false
        currentStatus = "Complete"
    }

    // MARK: - QA Load Test

    func runQALoadTest() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowedContentTypes  = [.image]
        panel.message = "QA: Select one photo to clone 10,000×"
        guard panel.runModal() == .OK, let src = panel.url else { return }
        Task {
            currentStatus = "Generating 10k QA clones…"
            let tmp = FileManager.default.temporaryDirectory
                .appendingPathComponent("QA_\(UUID().uuidString)")
            try? FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
            for i in 1...10_000 {
                let dst = tmp.appendingPathComponent("QA_\(i).\(src.pathExtension)")
                try? FileManager.default.copyItem(at: src, to: dst)
            }
            startProcessing(folderURL: tmp)
        }
    }

    // MARK: - Logging

    @discardableResult
    func log(_ message: String) -> String {
        let entry = "[\(Date().formatted(date: .omitted, time: .standard))] \(message)"
        logs.append(entry)
        if logs.count > 500 { logs.removeFirst(logs.count - 500) }
        return entry
    }

    // MARK: - Time Remaining

    // Rolling throughput window — cumulative rate stays optimistic when
    // memory pressure halves throughput mid-scan. These samples feed a 60 s
    // moving-average rate for the current phase.
    private struct ETASample { let t: Date; let done: Int }
    private var etaSamples: [ETASample] = []
    private var etaSamplesPhase: ScanPhase = .idle

    func updateETA() {
        if isProcessing, let begin = processingStartTime {
            let e = Int(Date().timeIntervalSince(begin))
            let h = e / 3600, m = (e % 3600) / 60, s = e % 60
            if h > 0      { elapsedString = "\(h)h \(m)m elapsed" }
            else if m > 0 { elapsedString = "\(m)m \(s)s elapsed" }
            else          { elapsedString = "\(s)s elapsed" }
        } else {
            elapsedString = ""
        }

        guard isProcessing, let start = phaseStartTime else {
            etaString = ""
            return
        }

        let done:  Int
        let total: Int
        switch scanPhase {
        case .discovering:
            etaString = "Discovering…"
            return
        case .tagging:    done = processedCount;       total = totalCount
        case .clustering: done = clusteringFacesDone;  total = clusteringFacesTotal
        case .naming:     done = namingDone;           total = namingTotal
        case .scoring:    done = scoringDone;          total = scoringTotal
        case .idle, .ready:
            etaString = ""
            return
        }

        guard done > 5, total > done + 5 else { etaString = ""; return }

        // Reset samples when the phase changes so the previous phase's rate
        // doesn't poison this phase's ETA.
        if etaSamplesPhase != scanPhase {
            etaSamplesPhase = scanPhase
            etaSamples.removeAll()
        }
        let now = Date()
        etaSamples.append(ETASample(t: now, done: done))
        let windowStart = now.addingTimeInterval(-60)
        etaSamples.removeAll { $0.t < windowStart }

        var rate = Double(done) / max(now.timeIntervalSince(start), 0.001)
        if let first = etaSamples.first, first.t < now,
           etaSamples.count >= 2 {
            let dt = now.timeIntervalSince(first.t)
            let dd = done - first.done
            if dt >= 5, dd > 0 { rate = Double(dd) / dt }
        }

        let secs = Int(Double(total - done) / max(rate, 0.1))
        guard secs > 5 else { etaString = "Almost done…"; return }
        let h = secs / 3600, m = (secs % 3600) / 60, s = secs % 60
        if h > 0      { etaString = "\(h)h \(m)m left" }
        else if m > 0 { etaString = "\(m)m \(s)s left" }
        else          { etaString = "\(s)s left" }
    }

    // MARK: - UI Refresh Loop

    // Tree rebuild + ETA are driven by drainAtomicState. This entry point
    // only owns the trailing-edge debounce for heavy filters below.
    func startTreeUpdateLoop() {
        startUIRefreshLoop()
    }

    // Trailing-edge debounce: emit uiRefreshTick at most once per second, but
    // always emit the *last* scanBatchCount seen so heavy filters catch up
    // once the scan settles.
    private func startUIRefreshLoop() {
        uiRefreshTask?.cancel()
        uiRefreshTask = Task { [weak self] in
            var lastSeen = -1
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                guard let self else { return }
                let current = await MainActor.run { self.scanBatchCount }
                if current != lastSeen {
                    lastSeen = current
                    await MainActor.run { self.uiRefreshTick &+= 1 }
                }
            }
        }
    }

    func stopUIRefreshLoop() {
        uiRefreshTask?.cancel()
        uiRefreshTask = nil
    }

    func stopTreeUpdate() {
        treeUpdateTask?.cancel()
        treeUpdateTask = nil
        rebuildTreeFromAccumulator()
        stopUIRefreshLoop()
        uiRefreshTick &+= 1  // final flush so listening views refresh post-scan
    }
}
