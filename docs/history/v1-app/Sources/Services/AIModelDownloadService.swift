import Foundation
import Observation
import MLX
import MLXLMCommon
import MLXVLM

// MARK: - AIModelDownloadService

// Single-lane model downloader. Concurrent HTTP streams + MLX Metal init on
// @MainActor previously crashed, so downloads serialize through one queue.
// I/O runs on a detached utility Task; progress hops back to main.

@MainActor
@Observable
final class AIModelDownloadService {
    static let shared = AIModelDownloadService()

    struct Progress: Sendable {
        var model: AIModelKind
        var bytesDownloaded: Int64 = 0
        var bytesExpected: Int64 = 0
        var currentFile: String = ""
        var fileIndex: Int = 0
        var fileTotal: Int = 0
        var fractionComplete: Double {
            guard bytesExpected > 0 else { return 0 }
            return min(1.0, Double(bytesDownloaded) / Double(bytesExpected))
        }
    }

    enum ModelStatus: Sendable {
        case notInstalled
        case queued
        case downloading(Progress)
        case installed
        case failed(String)
    }

    private(set) var status: [AIModelKind: ModelStatus] = [:]

    var isAnyActive: Bool {
        status.values.contains {
            if case .downloading = $0 { return true }
            if case .queued      = $0 { return true }
            return false
        }
    }

    // Flipped by AppViewModel to pause the lane during a scan; downloads
    // resume automatically when it flips back.
    var scanInProgress: Bool = false {
        didSet {
            if !scanInProgress { startNextIfIdle() }
        }
    }

    private var queue: [AIModelKind] = []
    private var currentTask: Task<Void, Never>?
    private var currentKind: AIModelKind?

    init() {
        refreshStatus()
    }

    func refreshStatus() {
        for kind in AIModelKind.allCases {
            if kind.descriptor.isInstalled {
                status[kind] = .installed
            } else if status[kind] == nil {
                status[kind] = .notInstalled
            }
        }
    }

    // MARK: - Public API

    func isDownloading(_ kind: AIModelKind) -> Bool {
        if case .downloading = status[kind] { return true }
        return false
    }

    func download(_ kind: AIModelKind) {
        switch status[kind] {
        case .downloading, .queued, .installed: return
        default: break
        }

        let descriptor = kind.descriptor
        if let free = freeDiskBytes(), free < descriptor.approxBytes * 2 {
            status[kind] = .failed("Not enough free disk space (need ~\(ByteCountFormatter.string(fromByteCount: descriptor.approxBytes * 2, countStyle: .file))).")
            return
        }

        queue.append(kind)
        let canStartNow = currentKind == nil && !scanInProgress
        status[kind] = canStartNow
            ? .downloading(Progress(
                model: kind,
                bytesExpected: descriptor.approxBytes,
                fileTotal: descriptor.relativePaths.count
            ))
            : .queued

        startNextIfIdle()
    }

    func cancel(_ kind: AIModelKind) {
        if currentKind == kind {
            currentTask?.cancel()
        } else {
            queue.removeAll { $0 == kind }
            status[kind] = .notInstalled
        }
    }

    func delete(_ kind: AIModelKind) {
        cancel(kind)
        AIModelRegistry.remove(kind)
        status[kind] = .notInstalled
    }

    // MARK: - Queue runner

    private func startNextIfIdle() {
        guard !scanInProgress else { return }
        guard currentKind == nil, let next = queue.first else { return }
        currentKind = next

        let descriptor = next.descriptor
        if case .queued = status[next] {
            status[next] = .downloading(Progress(
                model: next,
                bytesExpected: descriptor.approxBytes,
                fileTotal: descriptor.relativePaths.count
            ))
        }

        currentTask = Task { [weak self] in
            await self?.runDownload(kind: next, descriptor: descriptor)
        }
    }

    private func runDownload(kind: AIModelKind, descriptor: AIModelDescriptor) async {
        defer {
            queue.removeAll { $0 == kind }
            currentKind = nil
            currentTask = nil
            startNextIfIdle()
        }

        if kind == .qwen2VL2B {
            // Legacy: explicit relativePaths so we get accurate per-file
            // progress. MLX's HF downloader crashes on @MainActor — route
            // through our detached HTTP path writing into MLX's cache dir.
            let mlxCacheDir = DeepAnalyzeService.modelCacheDirectory(for: .qwen2VL2B)
            do {
                try await performDetachedDownload(
                    descriptor: descriptor,
                    overrideDestDir: mlxCacheDir
                )
                status[.qwen2VL2B] = .installed
            } catch is CancellationError {
                status[.qwen2VL2B] = .notInstalled
                if let dir = mlxCacheDir { try? FileManager.default.removeItem(at: dir) }
            } catch {
                status[.qwen2VL2B] = .failed(error.localizedDescription)
                if let dir = mlxCacheDir { try? FileManager.default.removeItem(at: dir) }
            }
            return
        }

        // New VLMs: relativePaths is empty (file list varies and some are
        // sharded). Hand off to MLX's downloader via VLMModelFactory and
        // immediately drop the loaded container — we just want the bytes on
        // disk, not the model in GPU memory. Same path that ensureLoaded()
        // uses on first analyze, run here in advance.
        if kind.isVLM, descriptor.relativePaths.isEmpty {
            do {
                try await downloadVLMViaMLX(kind: kind)
                status[kind] = .installed
            } catch is CancellationError {
                status[kind] = .notInstalled
                AIModelRegistry.remove(kind)
            } catch {
                status[kind] = .failed(error.localizedDescription)
                AIModelRegistry.remove(kind)
            }
            return
        }

        do {
            try await performDetachedDownload(descriptor: descriptor)
            status[kind] = .installed
        } catch is CancellationError {
            status[kind] = .notInstalled
            try? FileManager.default.removeItem(at: descriptor.localDir)
        } catch {
            status[kind] = .failed(error.localizedDescription)
            try? FileManager.default.removeItem(at: descriptor.localDir)
        }
    }

    // MLX-managed download path: invoke VLMModelFactory.loadContainer from a
    // detached Task (off MainActor — MLX's HF downloader requires that), wire
    // the fractionCompleted callback into our Progress struct, then immediately
    // discard the loaded ModelContainer + clear MLX's GPU cache so we don't
    // hold weights in memory just because the user clicked Download.
    private func downloadVLMViaMLX(kind: AIModelKind) async throws {
        let descriptor = kind.descriptor
        let id = descriptor.id
        let displayName = descriptor.displayName
        let approxBytes = descriptor.approxBytes

        let reporter: @Sendable (ProgressPatch) -> Void = { [weak self] patch in
            Task { @MainActor [weak self] in
                self?.applyPatch(for: id, patch: patch)
            }
        }
        reporter(.expected(approxBytes))
        reporter(.fileHeader(index: 1, total: 1, name: displayName))

        try await Task.detached(priority: .utility) {
            guard let config = DeepAnalyzeService.vlmConfig(for: kind) else {
                throw NSError(domain: "AIModelDownload", code: 2, userInfo: [
                    NSLocalizedDescriptionKey: "Unsupported VLM kind: \(kind.rawValue)"
                ])
            }

            // Generous GPU cap during the brief load-then-discard. Set to the
            // model's runtime budget so MLX doesn't spill mid-download.
            await MainActor.run {
                MLX.GPU.set(cacheLimit: DeepAnalyzeService.gpuCacheBudgetMB(for: kind) * 1024 * 1024)
            }

            // loadContainer downloads any missing files into the MLX hub
            // cache and returns a live ModelContainer. We drop the reference
            // immediately — the bytes-on-disk are what we wanted.
            _ = try await VLMModelFactory.shared.loadContainer(
                configuration: config
            ) { progress in
                let frac = progress.fractionCompleted
                let bytes = Int64(frac * Double(approxBytes))
                reporter(.downloaded(bytes))
            }

            // Drop GPU weights immediately — the load above briefly resident
            // them just so the safetensors got copied in. ensureLoaded will
            // reload on demand for actual analysis.
            await MainActor.run {
                MLX.GPU.set(cacheLimit: 0)
                MLX.GPU.clearCache()
            }
        }.value

        reporter(.downloaded(approxBytes))
    }

    // MARK: - Off-main download

    private func performDetachedDownload(
        descriptor: AIModelDescriptor,
        overrideDestDir: URL? = nil
    ) async throws {
        let id = descriptor.id
        let destDir = overrideDestDir ?? descriptor.localDir

        let reporter: @Sendable (ProgressPatch) -> Void = { [weak self] patch in
            Task { @MainActor [weak self] in
                self?.applyPatch(for: id, patch: patch)
            }
        }

        try await Task.detached(priority: .utility) {
            try FileManager.default.createDirectory(
                at: destDir, withIntermediateDirectories: true
            )

            // HEAD each file first so the progress bar reflects real byte totals.
            var totalBytes: Int64 = 0
            for path in descriptor.relativePaths {
                let size = (try? await Self.headContentLength(descriptor.remoteURL(for: path))) ?? 0
                totalBytes += size
            }
            if totalBytes > 0 {
                reporter(.expected(totalBytes))
            }

            var running: Int64 = 0
            for (i, path) in descriptor.relativePaths.enumerated() {
                try Task.checkCancellation()
                let url = descriptor.remoteURL(for: path)
                let dst = destDir.appendingPathComponent(path)
                try FileManager.default.createDirectory(
                    at: dst.deletingLastPathComponent(),
                    withIntermediateDirectories: true
                )

                reporter(.fileHeader(
                    index: i + 1,
                    total: descriptor.relativePaths.count,
                    name: (path as NSString).lastPathComponent
                ))

                let (asyncBytes, response) = try await URLSession.shared.bytes(from: url)
                guard let http = response as? HTTPURLResponse,
                      (200..<300).contains(http.statusCode) else {
                    throw NSError(domain: "AIModelDownload", code: 1, userInfo: [
                        NSLocalizedDescriptionKey: "Server returned \((response as? HTTPURLResponse)?.statusCode ?? 0) for \(path)"
                    ])
                }

                let tmp = dst.appendingPathExtension("partial")
                FileManager.default.createFile(atPath: tmp.path, contents: nil)
                let handle = try FileHandle(forWritingTo: tmp)

                var chunk = Data()
                chunk.reserveCapacity(64 * 1024)
                var sinceUIUpdate = 0
                for try await byte in asyncBytes {
                    try Task.checkCancellation()
                    chunk.append(byte)
                    if chunk.count >= 64 * 1024 {
                        try handle.write(contentsOf: chunk)
                        running += Int64(chunk.count)
                        sinceUIUpdate += chunk.count
                        chunk.removeAll(keepingCapacity: true)
                        if sinceUIUpdate >= 256 * 1024 {
                            reporter(.downloaded(running))
                            sinceUIUpdate = 0
                        }
                    }
                }
                if !chunk.isEmpty {
                    try handle.write(contentsOf: chunk)
                    running += Int64(chunk.count)
                }
                try handle.close()

                // replaceItemAt avoids the TOCTOU of fileExists+remove+move, but returns
                // nil when there's no original to back up — fall back to moveItem.
                if (try? FileManager.default.replaceItemAt(dst, withItemAt: tmp)) == nil {
                    try FileManager.default.moveItem(at: tmp, to: dst)
                }

                reporter(.downloaded(running))
            }
        }.value
    }

    private enum ProgressPatch: Sendable {
        case expected(Int64)
        case fileHeader(index: Int, total: Int, name: String)
        case downloaded(Int64)
    }

    private func applyPatch(for kind: AIModelKind, patch: ProgressPatch) {
        updateProgress(for: kind) { p in
            switch patch {
            case .expected(let n):              p.bytesExpected = n
            case .fileHeader(let i, let t, let n):
                p.fileIndex = i; p.fileTotal = t; p.currentFile = n
            case .downloaded(let n):            p.bytesDownloaded = n
            }
        }
    }

    private nonisolated static func headContentLength(_ url: URL) async throws -> Int64 {
        var req = URLRequest(url: url)
        req.httpMethod = "HEAD"
        let (_, resp) = try await URLSession.shared.data(for: req)
        guard let http = resp as? HTTPURLResponse,
              let lenStr = http.value(forHTTPHeaderField: "Content-Length"),
              let len = Int64(lenStr) else {
            return 0
        }
        return len
    }

    private func updateProgress(for kind: AIModelKind, mutate: (inout Progress) -> Void) {
        guard case .downloading(var p) = status[kind] else { return }
        mutate(&p)
        status[kind] = .downloading(p)
    }

    private func freeDiskBytes() -> Int64? {
        let url = AIModelRegistry.baseDirectory
        guard let values = try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]),
              let avail = values.volumeAvailableCapacityForImportantUsage else { return nil }
        return avail
    }
}
