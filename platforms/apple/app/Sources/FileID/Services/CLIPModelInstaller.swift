// In-app downloader for MobileCLIP-S2 (image encoder + text encoder +
// BPE vocab). Files come straight from Apple/OpenAI HF repos —
// per-file streaming with up to 3 concurrent fetches.
import Foundation
import AppKit
import CryptoKit
import Darwin
import FileIDShared

@MainActor
@Observable
public final class CLIPModelInstaller {

    public static let shared = CLIPModelInstaller()

    public enum Status: Equatable {
        case unknown
        case missing(reason: String)
        case installed(sizeBytes: Int64)
        case downloading(fraction: Double, message: String,
                         bytesPerSecond: Double, etaSeconds: Double)
        case extracting
        case installFailed(String)
    }

    public private(set) var status: Status = .unknown
    /// On-disk presence of each required file, refreshed only on real disk
    /// transitions (appear / install / uninstall) via refreshStatus — never on
    /// download ticks. The Settings card reads this instead of calling
    /// FileManager.fileExists in its row builder, so an active download no longer
    /// fires a stat() syscall on the main thread on every progress tick. (R7)
    public private(set) var presentFilePaths: Set<String> = []
    /// Flips once the text encoder's ORT session finishes its
    /// multi-second build — Library observes it to drop the
    /// keyword-only hint and re-run the active search.
    public private(set) var textEncoderReady = false
    private var task: Task<Void, Never>?
    /// True only while a hub fetch is actively running. Progress ticks
    /// arrive as queued MainActor tasks and can be scheduled AFTER the
    /// catch arm wrote a terminal status — gate `publishFromTracker` on
    /// liveness so a stale tick can't resurrect a phantom "Downloading…"
    /// footer with a dead Cancel button. Mirrors ArcFaceModelInstaller.active.
    private var installing = false
    private var uninstalling = false

    private init() {}

    // MARK: - Required files

    /// Files the installer must produce on disk for both the image
    /// embedder (engine) and the text encoder (app) to be usable.
    public static var requiredFiles: [URL] {
        let models = modelsRoot
        return [
            models.appendingPathComponent("mobileclip_image/clip_vitb32_image.onnx"),
            models.appendingPathComponent("clip_text/clip_text.onnx"),
            models.appendingPathComponent("clip_text/vocab.json"),
            models.appendingPathComponent("clip_text/merges.txt"),
        ]
    }

    public static var modelsRoot: URL { AppSupportPath.models }

    public static let approxDownloadBytes: Int64 = {
        let ids = Set(["clip_vitb32_image", "clip_vitb32_text", "clip_bpe_vocab", "clip_bpe_merges"])
        return ModelManifest.artifacts
            .filter { ids.contains($0.id) }
            .reduce(0) { $0 + $1.approxBytes }
    }()

    private static var fetchPlan: [(remote: URL, dest: URL, sha256: String?)] {
        let m = modelsRoot
        let txtDir = m.appendingPathComponent("clip_text")
        // OpenCLIP ViT-B/32 (MIT) ONNX — commercial-clean replacement for the
        // research-only Apple MobileCLIP-S2 CoreML packages. Same 512-d space
        // as the Windows engine; BPE vocab/merges still from OpenAI's repo.
        func xenova(_ rel: String) -> URL? {
            URL(string: "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/\(rel)")
        }
        func openaiBpe(_ name: String) -> URL? {
            URL(string: "https://huggingface.co/openai/clip-vit-base-patch32/resolve/main/\(name)")
        }
        // compactMap drops any entry whose URL doesn't parse — currently never
        // possible (static literals), but it means a future typo can't crash
        // the installer.
        let pairs: [(URL?, URL)] = [
            (xenova("onnx/vision_model.onnx"),
             m.appendingPathComponent("mobileclip_image/clip_vitb32_image.onnx")),
            (xenova("onnx/text_model.onnx"),
             txtDir.appendingPathComponent("clip_text.onnx")),
            (openaiBpe("vocab.json"), txtDir.appendingPathComponent("vocab.json")),
            (openaiBpe("merges.txt"), txtDir.appendingPathComponent("merges.txt")),
        ]
        return pairs.compactMap { remote, dest in
            remote.map { (remote: $0, dest: dest,
                          sha256: ModelManifest.sha256(forURL: $0)) }
        }
    }

    // MARK: - Status

    public func markTextEncoderReady() {
        textEncoderReady = true
    }

    public func refreshStatus() {
        recomputePresentFiles()
        var totalSize: Int64 = 0
        var firstMissing: String?
        for f in Self.requiredFiles {
            if presentFilePaths.contains(f.path) {
                totalSize += directorySize(f)
            } else if firstMissing == nil {
                firstMissing = f.lastPathComponent
            }
        }
        if let missing = firstMissing {
            status = .missing(reason: "Missing: \(missing)")
        } else {
            status = .installed(sizeBytes: totalSize)
        }
    }

    /// Refresh on-disk presence WITHOUT touching `status`. The partial-install
    /// failure paths (promote / verify / extract) leave some required files in
    /// the live tree but must keep their `.installFailed` status — calling
    /// refreshStatus() there would overwrite it with .missing/.installed, so they
    /// call this instead to keep the per-file Settings rows accurate. (R7 delta)
    private func recomputePresentFiles() {
        var present: Set<String> = []
        for f in Self.requiredFiles where FileManager.default.fileExists(atPath: f.path) {
            present.insert(f.path)
        }
        presentFilePaths = present
    }

    // MARK: - Install paths

    public func install() {
        guard task == nil, !uninstalling else { return }
        task = Task { [weak self] in
            await AppSleepActivity.run(reason: "Install CLIP models") {
                await self?.runHubFetch()
            }
            self?.task = nil
        }
    }

    /// Air-gapped fallback. Expects the same layout the hub fetch
    /// produces — mobileclip_image/… and clip_text/… at the top.
    public func installFromLocalZip(_ zipURL: URL) {
        guard task == nil, !uninstalling else { return }
        task = Task { [weak self] in
            await AppSleepActivity.run(reason: "Install local CLIP models") {
                await self?.runExtract(zipAt: zipURL, deleteZipAfter: false)
            }
            self?.task = nil
        }
    }

    public func cancel() {
        task?.cancel()
    }

    public func uninstall() async {
        guard !uninstalling else { return }
        uninstalling = true
        defer { uninstalling = false }
        let activeTask = task
        activeTask?.cancel()
        await activeTask?.value
        task = nil
        let dirs = [
            Self.modelsRoot.appendingPathComponent("mobileclip_image", isDirectory: true),
            Self.modelsRoot.appendingPathComponent("clip_text", isDirectory: true),
        ]
        for d in dirs {
            try? FileManager.default.removeItem(at: d)
        }
        refreshStatus()
    }

    // MARK: - Implementation

    /// Concurrent per-file fetch from HF (3 streams). Each file's
    /// tick lands in the shared tracker; the global Status.downloading
    /// reads sum-of-writtens / sum-of-totals + summed bandwidth.
    /// Files stage into a sibling dir and atomic-promote on full
    /// success — a partial install never poisons the production tree.
    private func runHubFetch() async {
        installing = true
        defer { installing = false }
        Self.sweepOrphanedStagingRoots()
        let approxBytes = Self.approxDownloadBytes
        if let free = freeDiskBytes(), free < approxBytes * 2 {
            status = .installFailed("Not enough free space (need ~\(approxBytes * 2 / 1_048_576) MB).")
            return
        }

        let modelsRoot = Self.modelsRoot
        let stagingRoot = modelsRoot
            .appendingPathComponent(".clip-staging-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: stagingRoot, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: stagingRoot) }

        let plan = Self.fetchPlan
        let stagedPlan: [(remote: URL, staged: URL, finalDest: URL, sha256: String?)] = plan.map { item in
            let rel = item.dest.path.dropFirst(modelsRoot.path.count + 1)
            let staged = stagingRoot.appendingPathComponent(String(rel))
            return (item.remote, staged, item.dest, item.sha256)
        }

        status = .downloading(fraction: 0, message: "Connecting…",
                              bytesPerSecond: 0, etaSeconds: 0)
        let tracker = ProgressTracker(fileCount: plan.count)

        do {
            try await runParallelDownloads(
                plan: stagedPlan.map { (remote: $0.remote, dest: $0.staged, sha256: $0.sha256) },
                // Each file uses up to 8-way ranged GETs internally,
                // so 2 concurrent files = ~16 TCP connections to HF.
                // More than that and Cloudflare starts rate-limiting.
                tracker: tracker, maxConcurrency: 2
            )
        } catch is CancellationError {
            status = .missing(reason: "Cancelled.")
            return
        } catch let StreamingDownloadError.http(code) {
            status = .installFailed("Server returned HTTP \(code). Couldn't reach the model server.")
            return
        } catch StreamingDownloadError.checksumMismatch(let expected, let actual) {
            status = .installFailed("Integrity check failed: a downloaded file's SHA-256 (\(actual.prefix(12))…) doesn't match the pinned manifest hash (\(expected.prefix(12))…). The file was discarded — try again; repeated failures may mean the download was tampered with.")
            return
        } catch {
            status = .installFailed("Download failed: \(error.localizedDescription)")
            return
        }

        status = .extracting
        do {
            try promoteStaged(stagedPlan.map { (staged: $0.staged, finalDest: $0.finalDest) })
        } catch {
            status = .installFailed("Couldn't promote staged files: \(error.localizedDescription)")
            recomputePresentFiles()
            return
        }

        for f in Self.requiredFiles {
            if !FileManager.default.fileExists(atPath: f.path) {
                status = .installFailed("Missing after install: \(f.lastPathComponent).")
                recomputePresentFiles()
                return
            }
        }

        // Eager text-encoder load so search activates without restart.
        Task.detached(priority: .utility) {
            if CLIPTextEncoder.shared.load() {
                await Self.shared.markTextEncoderReady()
            }
        }
        refreshStatus()
    }

    /// `.clip-staging-<UUID>` roots from a process killed mid-install
    /// (the cleanup `defer` above dies with the process) strand ~250 MB
    /// each and eat into the free-space preflight. Wholesale removal is
    /// safe: only this installer creates them, one install task runs at
    /// a time (`guard task == nil`), and none can be in flight here.
    private static func sweepOrphanedStagingRoots() {
        let fm = FileManager.default
        guard let entries = try? fm.contentsOfDirectory(
            at: modelsRoot, includingPropertiesForKeys: nil) else { return }
        for entry in entries where entry.lastPathComponent.hasPrefix(".clip-staging-") {
            try? fm.removeItem(at: entry)
        }
    }

    /// Body lives outside the TaskGroup closure so the Swift 6 region
    /// isolation checker sees Sendable parameters instead of capture
    /// inference on the closure's implicit set.
    private func runParallelDownloads(
        plan: [(remote: URL, dest: URL, sha256: String?)],
        tracker: ProgressTracker,
        maxConcurrency: Int
    ) async throws {
        let count = plan.count
        let remotes: [URL] = plan.map(\.remote)
        let dests:   [URL] = plan.map(\.dest)
        let hashes:  [String?] = plan.map(\.sha256)
        try await withThrowingTaskGroup(of: Void.self) { group in
            var inFlight = 0
            var i = 0
            while i < count {
                if inFlight >= maxConcurrency {
                    try await group.next()
                    inFlight -= 1
                }
                let idx = i
                let remote = remotes[idx]
                let dest = dests[idx]
                let sha256 = hashes[idx]
                group.addTask {
                    try await Self.runOneFile(index: idx, remote: remote,
                                              dest: dest, sha256: sha256,
                                              tracker: tracker)
                }
                inFlight += 1
                i += 1
            }
            try await group.waitForAll()
        }
    }

    private static func runOneFile(
        index: Int, remote: URL, dest: URL, sha256: String?, tracker: ProgressTracker
    ) async throws {
        try Task.checkCancellation()
        try FileManager.default.createDirectory(
            at: dest.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        // parallelStreamingDownload HEADs first, decides parts based
        // on Content-Length, and falls back to single-stream when the
        // host doesn't expose ranges or the file is small. weight.bin
        // (~80 MB) gets 8-way; Manifest.json (~few KB) stays single.
        try await parallelStreamingDownload(remote: remote, dest: dest, parts: 12,
                                            expectedSHA256: sha256) { tick in
            tracker.update(index: index, tick: tick)
            Task { @MainActor in
                Self.shared.publishFromTracker(tracker)
            }
        }
        tracker.markComplete(index: index)
        await MainActor.run { Self.shared.publishFromTracker(tracker) }
    }

    @MainActor
    private func publishFromTracker(_ tracker: ProgressTracker) {
        guard installing else { return }
        let snap = tracker.snapshot()
        let total = snap.totalBytes
        let written = snap.writtenBytes
        let frac = total > 0 ? min(1.0, Double(written) / Double(total)) : 0
        let mb = Double(written) / 1_048_576.0
        let totalMB = Double(total) / 1_048_576.0
        let activeCount = snap.activeFiles
        let activeLabel = activeCount > 1 ? " (\(activeCount) files in parallel)" : ""
        let msg = total > 0
            ? String(format: "Downloading… %.0f / %.0f MB%@", mb, totalMB, activeLabel)
            : String(format: "Downloading… %.0f MB%@", mb, activeLabel)
        status = .downloading(
            fraction: frac, message: msg,
            bytesPerSecond: snap.combinedBytesPerSec,
            etaSeconds: snap.combinedETASec
        )
    }

    /// Lock-protected aggregator. NSLock + `@unchecked Sendable` rather
    /// than `@MainActor` because Swift 6's region isolation checker
    /// can't see through the `addTask { @MainActor in … }` closure.
    private final class ProgressTracker: @unchecked Sendable {
        struct FileState { var written: Int64; var total: Int64; var bps: Double; var done: Bool }
        struct Snapshot {
            let writtenBytes: Int64
            let totalBytes: Int64
            let combinedBytesPerSec: Double
            let combinedETASec: Double
            let activeFiles: Int
        }
        private let lock = NSLock()
        private var states: [FileState]

        init(fileCount: Int) {
            self.states = Array(repeating: FileState(written: 0, total: 0, bps: 0, done: false),
                                count: fileCount)
        }

        func update(index: Int, tick: DownloadTick) {
            lock.lock(); defer { lock.unlock() }
            guard states.indices.contains(index) else { return }
            states[index].written = tick.written
            if tick.total > 0 { states[index].total = tick.total }
            states[index].bps = tick.bytesPerSecond
        }

        func markComplete(index: Int) {
            lock.lock(); defer { lock.unlock() }
            guard states.indices.contains(index) else { return }
            states[index].done = true
            if states[index].total > 0 {
                states[index].written = states[index].total
            }
            states[index].bps = 0
        }

        func snapshot() -> Snapshot {
            lock.lock(); defer { lock.unlock() }
            var written: Int64 = 0
            var total: Int64 = 0
            var bps: Double = 0
            var active = 0
            for s in states {
                written += s.written
                total += s.total
                if !s.done && s.bps > 0 { bps += s.bps; active += 1 }
                if !s.done && s.bps == 0 && s.written > 0 { active += 1 }
            }
            let remaining = max(0, total - written)
            let eta = bps > 0 ? Double(remaining) / bps : 0
            return Snapshot(writtenBytes: written, totalBytes: total,
                            combinedBytesPerSec: bps, combinedETASec: eta,
                            activeFiles: active)
        }
    }

    private func runExtract(zipAt zipURL: URL, deleteZipAfter: Bool) async {
        status = .extracting
        let modelsRoot = Self.modelsRoot
        try? FileManager.default.createDirectory(at: modelsRoot, withIntermediateDirectories: true)

        var isDir: ObjCBool = false
        guard FileManager.default.fileExists(atPath: zipURL.path, isDirectory: &isDir),
              !isDir.boolValue else {
            status = .installFailed("Zip file not found at \(zipURL.lastPathComponent).")
            return
        }
        guard zipURL.pathExtension.lowercased() == "zip" else {
            status = .installFailed("Selected file isn't a .zip — choose a clip-models.zip archive.")
            return
        }
        if let attrs = try? FileManager.default.attributesOfItem(atPath: zipURL.path),
           let type = attrs[.type] as? FileAttributeType, type != .typeRegular {
            status = .installFailed("Zip path isn't a regular file (symlink or special file).")
            return
        }

        let minFreeBytes: Int64 = 1_073_741_824
        if let fsAttrs = try? FileManager.default.attributesOfFileSystem(forPath: modelsRoot.path),
           let free = (fsAttrs[.systemFreeSize] as? NSNumber)?.int64Value,
           free < minFreeBytes {
            status = .installFailed("Need at least 1 GB free to extract; only \(free / 1_048_576) MB available.")
            return
        }

        let stagingRoot = modelsRoot.appendingPathComponent(
            ".clip-staging-\(UUID().uuidString)", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: stagingRoot, withIntermediateDirectories: false)
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: stagingRoot.path)
        } catch {
            status = .installFailed("Couldn't create a private extraction area: \(error.localizedDescription)")
            return
        }
        defer { try? FileManager.default.removeItem(at: stagingRoot) }

        do {
            try Task.checkCancellation()
            let expandedBytes = try await preflightArchive(zipURL, workRoot: stagingRoot)
            let requiredFree = expandedBytes * 2 + 256 * 1_024 * 1_024
            if let fsAttrs = try? FileManager.default.attributesOfFileSystem(forPath: modelsRoot.path),
               let free = (fsAttrs[.systemFreeSize] as? NSNumber)?.uint64Value,
               free < requiredFree {
                throw LocalArchiveError(
                    "archive needs at least \(requiredFree / 1_048_576) MB free for extraction and rollback")
            }
            let result = try await runUnzip(
                ["-q", zipURL.path, "-d", stagingRoot.path],
                workRoot: stagingRoot,
                timeoutSeconds: 300,
                label: "extract")
            guard result.status == 0 else {
                throw LocalArchiveError("unzip failed (\(result.status)): \(result.error)")
            }
            try Task.checkCancellation()

            let plan = Self.fetchPlan.map { item -> (staged: URL, finalDest: URL, sha256: String?) in
                let relative = item.dest.path.dropFirst(modelsRoot.path.count + 1)
                return (
                    stagingRoot.appendingPathComponent(String(relative)),
                    item.dest,
                    item.sha256
                )
            }
            guard plan.count == Self.requiredFiles.count else {
                throw LocalArchiveError("the canonical CLIP manifest is incomplete")
            }
            for item in plan {
                try Task.checkCancellation()
                let values = try item.staged.resourceValues(forKeys: [
                    .isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey,
                ])
                guard values.isRegularFile == true, values.isSymbolicLink != true else {
                    throw LocalArchiveError("\(item.staged.lastPathComponent) isn't a regular file")
                }
                guard let expected = item.sha256 else {
                    throw LocalArchiveError("\(item.staged.lastPathComponent) has no canonical SHA-256 pin")
                }
                let actual = try sha256File(item.staged)
                guard actual == expected.lowercased() else {
                    throw LocalArchiveError(
                        "\(item.staged.lastPathComponent) failed integrity verification")
                }
            }
            try rejectSpecialFiles(in: stagingRoot)
            try Task.checkCancellation()
            try promoteStaged(plan.map { (staged: $0.staged, finalDest: $0.finalDest) })
        } catch is CancellationError {
            status = .missing(reason: "Cancelled.")
            recomputePresentFiles()
            return
        } catch {
            status = .installFailed("Local model archive rejected: \(error.localizedDescription)")
            recomputePresentFiles()
            return
        }

        if deleteZipAfter {
            try? FileManager.default.removeItem(at: zipURL)
        }
        Task.detached(priority: .utility) {
            if CLIPTextEncoder.shared.load() {
                await Self.shared.markTextEncoderReady()
            }
        }
        refreshStatus()
    }

    func preflightArchive(_ zipURL: URL, workRoot: URL) async throws -> UInt64 {
        let namesResult = try await runUnzip(
            ["-Z1", zipURL.path], workRoot: workRoot,
            timeoutSeconds: 30, label: "names")
        guard namesResult.status == 0 else {
            throw LocalArchiveError("couldn't list archive entries: \(namesResult.error)")
        }
        let names = namesResult.output.split(whereSeparator: \.isNewline).map(String.init)
        guard names.count <= 10_000 else {
            throw LocalArchiveError("archive has more than 10,000 entries")
        }
        for name in names where !LocalArchiveSafety.entryNameIsSafe(name) {
            throw LocalArchiveError("archive contains an unsafe entry name")
        }

        let verbose = try await runUnzip(
            ["-Z", "-v", zipURL.path], workRoot: workRoot,
            timeoutSeconds: 30, label: "metadata")
        guard verbose.status == 0 else {
            throw LocalArchiveError("couldn't inspect archive metadata: \(verbose.error)")
        }
        var total: UInt64 = 0
        for rawLine in verbose.output.split(whereSeparator: \.isNewline) {
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("uncompressed size:") {
                let fields = line.split(whereSeparator: \.isWhitespace)
                guard fields.count >= 3, let size = UInt64(fields[2]), size <= 1_073_741_824 else {
                    throw LocalArchiveError("archive contains an oversized entry")
                }
                total = total.addingReportingOverflow(size).overflow ? UInt64.max : total + size
                if total > 2_147_483_648 {
                    throw LocalArchiveError("archive expands past the 2 GB safety cap")
                }
            }
            if line.hasPrefix("file security status:"),
               let value = line.split(separator: ":", maxSplits: 1).last,
               !LocalArchiveSafety.fileSecurityStatusIsUnencrypted(String(value)) {
                throw LocalArchiveError("encrypted archives aren't supported")
            }
            if line.hasPrefix("Unix file attributes"),
               let permissions = line.split(separator: ":", maxSplits: 1).last?
                    .trimmingCharacters(in: .whitespaces),
               !LocalArchiveSafety.unixEntryTypeIsSafe(permissions) {
                throw LocalArchiveError("archive contains a symlink or special entry")
            }
        }
        return total
    }

    private func runUnzip(
        _ arguments: [String],
        workRoot: URL,
        timeoutSeconds: UInt64,
        label: String
    ) async throws -> (status: Int32, output: String, error: String) {
        let outputURL = workRoot.appendingPathComponent(".\(label)-stdout")
        let errorURL = workRoot.appendingPathComponent(".\(label)-stderr")
        FileManager.default.createFile(atPath: outputURL.path, contents: nil)
        FileManager.default.createFile(atPath: errorURL.path, contents: nil)
        let outputHandle = try FileHandle(forWritingTo: outputURL)
        let errorHandle = try FileHandle(forWritingTo: errorURL)
        defer {
            try? outputHandle.close()
            try? errorHandle.close()
            try? FileManager.default.removeItem(at: outputURL)
            try? FileManager.default.removeItem(at: errorURL)
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/unzip")
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = outputHandle
        process.standardError = errorHandle
        let timedOut = MutexBox(false)
        let cancelled = MutexBox(false)
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                process.terminationHandler = { _ in continuation.resume() }
                do {
                    try process.run()
                } catch {
                    continuation.resume(throwing: error)
                    return
                }
                if Task.isCancelled || cancelled.withLock({ $0 }) {
                    cancelled.withLock { $0 = true }
                    process.terminate()
                }
                Task.detached {
                    try? await Task.sleep(nanoseconds: timeoutSeconds * 1_000_000_000)
                    if process.isRunning {
                        timedOut.withLock { $0 = true }
                        process.terminate()
                        try? await Task.sleep(nanoseconds: 2_000_000_000)
                        if process.isRunning {
                            Darwin.kill(process.processIdentifier, SIGKILL)
                        }
                    }
                }
            }
        } onCancel: {
            cancelled.withLock { $0 = true }
            if process.isRunning {
                process.terminate()
                Task.detached {
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                    if process.isRunning {
                        Darwin.kill(process.processIdentifier, SIGKILL)
                    }
                }
            }
        }
        try outputHandle.synchronize()
        try errorHandle.synchronize()
        if cancelled.withLock({ $0 }) { throw CancellationError() }
        if timedOut.withLock({ $0 }) {
            throw LocalArchiveError("unzip \(label) timed out")
        }
        let output = try readSmallText(outputURL, cap: 4 * 1_024 * 1_024)
        let error = try readSmallText(errorURL, cap: 256 * 1_024)
        return (process.terminationStatus, output, error)
    }

    private func readSmallText(_ url: URL, cap: Int) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        let data = try handle.read(upToCount: cap + 1) ?? Data()
        guard data.count <= cap else { throw LocalArchiveError("archive listing was too large") }
        guard let text = String(data: data, encoding: .utf8) else {
            throw LocalArchiveError("archive listing wasn't UTF-8")
        }
        return text
    }

    private func sha256File(_ url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let data = try handle.read(upToCount: 1_024 * 1_024), !data.isEmpty {
            hasher.update(data: data)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    private func rejectSpecialFiles(in root: URL) throws {
        guard let enumerator = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: [.isDirectoryKey, .isRegularFileKey, .isSymbolicLinkKey],
            options: []) else {
            throw LocalArchiveError("couldn't inspect extracted files")
        }
        var count = 0
        for case let url as URL in enumerator {
            count += 1
            guard count <= 10_000 else { throw LocalArchiveError("too many extracted entries") }
            let values = try url.resourceValues(forKeys: [
                .isDirectoryKey, .isRegularFileKey, .isSymbolicLinkKey,
            ])
            guard values.isSymbolicLink != true,
                  values.isDirectory == true || values.isRegularFile == true else {
                throw LocalArchiveError("archive extracted a special filesystem object")
            }
        }
    }

    private func promoteStaged(_ plan: [(staged: URL, finalDest: URL)]) throws {
        try Self.promoteStaged(plan, modelsRoot: Self.modelsRoot)
    }

    static func promoteStaged(
        _ plan: [(staged: URL, finalDest: URL)],
        modelsRoot: URL,
        moveItem: (URL, URL) throws -> Void = { source, destination in
            try FileManager.default.moveItem(at: source, to: destination)
        },
        removeItem: (URL) throws -> Void = { url in
            try FileManager.default.removeItem(at: url)
        }
    ) throws {
        let backupRoot = modelsRoot.appendingPathComponent(
            ".clip-backup-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: backupRoot, withIntermediateDirectories: false)
        var backedUp: [(backup: URL, finalDest: URL)] = []
        var promoted: [(staged: URL, finalDest: URL)] = []
        do {
            for item in plan {
                try prepareSafeDestination(item.finalDest, modelsRoot: modelsRoot)
                if FileManager.default.fileExists(atPath: item.finalDest.path) {
                    let relative = item.finalDest.path.dropFirst(modelsRoot.path.count + 1)
                    let backup = backupRoot.appendingPathComponent(String(relative))
                    try FileManager.default.createDirectory(
                        at: backup.deletingLastPathComponent(), withIntermediateDirectories: true)
                    try moveItem(item.finalDest, backup)
                    backedUp.append((backup, item.finalDest))
                }
            }
            for item in plan {
                try prepareSafeDestination(item.finalDest, modelsRoot: modelsRoot)
                try moveItem(item.staged, item.finalDest)
                promoted.append(item)
            }
            try removeItem(backupRoot)
        } catch {
            var rollbackErrors: [String] = []
            for item in promoted.reversed() {
                do { try moveItem(item.finalDest, item.staged) }
                catch { rollbackErrors.append(error.localizedDescription) }
            }
            for item in backedUp.reversed() {
                do { try moveItem(item.backup, item.finalDest) }
                catch { rollbackErrors.append(error.localizedDescription) }
            }
            if rollbackErrors.isEmpty {
                try? FileManager.default.removeItem(at: backupRoot)
                throw error
            }
            throw LocalArchiveError(
                "promotion failed and rollback was incomplete; originals remain at \(backupRoot.path): \(rollbackErrors.joined(separator: "; "))")
        }
    }

    private static func prepareSafeDestination(_ destination: URL, modelsRoot: URL) throws {
        let fileManager = FileManager.default
        let root = modelsRoot.standardizedFileURL
        let destination = destination.standardizedFileURL
        let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        guard destination.path.hasPrefix(prefix) else {
            throw LocalArchiveError("model destination escaped the models directory")
        }
        let rootAttributes = try fileManager.attributesOfItem(atPath: root.path)
        guard rootAttributes[.type] as? FileAttributeType == .typeDirectory else {
            throw LocalArchiveError("models directory is a symlink or special object")
        }

        let relative = destination.path.dropFirst(prefix.count)
        let components = relative.split(separator: "/").map(String.init)
        var current = root
        for component in components.dropLast() {
            guard component != "." && component != ".." else {
                throw LocalArchiveError("invalid model destination component")
            }
            current.appendPathComponent(component, isDirectory: true)
            if fileManager.fileExists(atPath: current.path) {
                let attributes = try fileManager.attributesOfItem(atPath: current.path)
                guard attributes[.type] as? FileAttributeType == .typeDirectory else {
                    throw LocalArchiveError("model destination parent is a symlink or special object")
                }
            } else {
                try fileManager.createDirectory(at: current, withIntermediateDirectories: false)
            }
        }
        if fileManager.fileExists(atPath: destination.path) {
            let attributes = try fileManager.attributesOfItem(atPath: destination.path)
            guard attributes[.type] as? FileAttributeType == .typeRegular else {
                throw LocalArchiveError("model destination is a symlink or special object")
            }
        }
    }

    private struct LocalArchiveError: LocalizedError {
        let message: String
        init(_ message: String) { self.message = message }
        var errorDescription: String? { message }
    }

    // MARK: - Utilities

    private func freeDiskBytes() -> Int64? {
        let url = Self.modelsRoot
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        guard let values = try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]),
              let avail = values.volumeAvailableCapacityForImportantUsage else { return nil }
        return avail
    }

    private func directorySize(_ url: URL) -> Int64 {
        var total: Int64 = 0
        let fm = FileManager.default
        var isDir: ObjCBool = false
        guard fm.fileExists(atPath: url.path, isDirectory: &isDir) else { return 0 }
        if !isDir.boolValue {
            if let v = try? url.resourceValues(forKeys: [.fileSizeKey]),
               let s = v.fileSize { return Int64(s) }
            return 0
        }
        if let en = fm.enumerator(at: url, includingPropertiesForKeys: [.fileSizeKey]) {
            for case let f as URL in en {
                if let v = try? f.resourceValues(forKeys: [.fileSizeKey]),
                   let s = v.fileSize { total += Int64(s) }
            }
        }
        return total
    }
}
