// In-app downloader for MobileCLIP-S2 (image encoder + text encoder +
// BPE vocab). Files come straight from Apple/OpenAI HF repos —
// per-file streaming with up to 3 concurrent fetches.
import Foundation
import AppKit
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
    /// Flips once the text encoder's ORT session finishes its
    /// multi-second build — Library observes it to drop the
    /// keyword-only hint and re-run the active search.
    public private(set) var textEncoderReady = false
    private var task: Task<Void, Never>?

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
        let files = Self.requiredFiles
        var totalSize: Int64 = 0
        for f in files {
            guard FileManager.default.fileExists(atPath: f.path) else {
                status = .missing(reason: "Missing: \((f.lastPathComponent))")
                return
            }
            totalSize += directorySize(f)
        }
        status = .installed(sizeBytes: totalSize)
    }

    // MARK: - Install paths

    public func install() {
        guard task == nil else { return }
        task = Task { [weak self] in
            await self?.runHubFetch()
            self?.task = nil
        }
    }

    /// Air-gapped fallback. Expects the same layout the hub fetch
    /// produces — mobileclip_image/… and clip_text/… at the top.
    public func installFromLocalZip(_ zipURL: URL) {
        guard task == nil else { return }
        task = Task { [weak self] in
            await self?.runExtract(zipAt: zipURL, deleteZipAfter: false)
            self?.task = nil
        }
    }

    public func cancel() {
        task?.cancel()
    }

    public func uninstall() {
        cancel()
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
        let approxBytes: Int64 = 250 * 1024 * 1024
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
            for item in stagedPlan {
                try FileManager.default.createDirectory(
                    at: item.finalDest.deletingLastPathComponent(),
                    withIntermediateDirectories: true)
                if FileManager.default.fileExists(atPath: item.finalDest.path) {
                    _ = try FileManager.default.replaceItemAt(item.finalDest, withItemAt: item.staged)
                } else {
                    try FileManager.default.moveItem(at: item.staged, to: item.finalDest)
                }
            }
        } catch {
            status = .installFailed("Couldn't promote staged files: \(error.localizedDescription)")
            return
        }

        for f in Self.requiredFiles {
            if !FileManager.default.fileExists(atPath: f.path) {
                status = .installFailed("Missing after install: \(f.lastPathComponent).")
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

        // Zip-bomb safety margin: extract is ~250 MB, require 1 GB free.
        let minFreeBytes: Int64 = 1_073_741_824
        if let fsAttrs = try? FileManager.default.attributesOfFileSystem(forPath: modelsRoot.path),
           let free = (fsAttrs[.systemFreeSize] as? NSNumber)?.int64Value,
           free < minFreeBytes {
            let freeMB = free / 1_048_576
            status = .installFailed("Need at least 1 GB free to extract; only \(freeMB) MB available.")
            return
        }

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/unzip")
        proc.arguments = ["-o", "-q", zipURL.path, "-d", modelsRoot.path]
        let stderr = Pipe()
        proc.standardError = stderr
        proc.standardOutput = Pipe()

        do {
            try proc.run()
            // 5 min watchdog — degenerate archive shouldn't hang.
            let resumed = MutexBox(false)
            await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
                proc.terminationHandler = { _ in
                    if resumed.withLock({ if $0 { return false } else { $0 = true; return true } }) {
                        cont.resume()
                    }
                }
                Task.detached {
                    try? await Task.sleep(nanoseconds: 300 * 1_000_000_000)
                    if proc.isRunning { proc.terminate() }
                    if resumed.withLock({ if $0 { return false } else { $0 = true; return true } }) {
                        cont.resume()
                    }
                }
            }
        } catch {
            status = .installFailed("Couldn't run unzip: \(error.localizedDescription)")
            return
        }

        guard proc.terminationStatus == 0 else {
            let errData: Data = ((try? stderr.fileHandleForReading.readToEnd()) ?? nil) ?? Data()
            let errStr = String(data: errData, encoding: .utf8) ?? ""
            status = .installFailed("Extract failed (\(proc.terminationStatus)): \(errStr.trimmingCharacters(in: .whitespacesAndNewlines))")
            return
        }

        for f in Self.requiredFiles {
            if !FileManager.default.fileExists(atPath: f.path) {
                status = .installFailed("Zip didn't contain \(f.lastPathComponent).")
                return
            }
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
