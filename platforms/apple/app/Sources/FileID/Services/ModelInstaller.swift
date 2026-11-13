// User-initiated downloads of the single-directory ONNX models (RAM++, BGE,
// SFace, Face Mesh) from their Hugging Face repos. Weights are never redistributed in-app,
// and every file is SHA-256 verified against ModelManifest.
import Foundation
import FileIDShared

@MainActor
protocol ModelInstalling: AnyObject {
    var status: ModelInstaller.Status { get }
    var presentFilePaths: Set<String> { get }
    func refreshStatus()
    func install()
    func cancel()
    func uninstall() async
}

@MainActor
@Observable
final class ModelInstaller: ModelInstalling {
    enum Status: Equatable {
        case unknown
        case missing(reason: String)
        case installed(sizeBytes: Int64)
        case downloading(fraction: Double, message: String,
                         bytesPerSecond: Double, etaSeconds: Double)
        case extracting
        case installFailed(String)

        var isInstalled: Bool { if case .installed = self { true } else { false } }
        var inProgress: Bool {
            switch self {
            case .downloading, .extracting: true
            default: false
            }
        }
        var fraction: Double? { if case .downloading(let f, _, _, _) = self { f } else { nil } }
        var progressLabel: String? {
            switch self {
            case .downloading(_, let message, _, _): message
            case .extracting: "Finishing…"
            case .installFailed(let why): why
            default: nil
            }
        }
        var rateAndETA: String? {
            guard case .downloading(_, _, let bps, let eta) = self else { return nil }
            return DownloadFormat.rateAndETA(DownloadTick(written: 0, total: 0, bytesPerSecond: bps, etaSeconds: eta))
        }
    }

    struct Spec: Sendable {
        struct Download: Sendable {
            let artifact: String
            let file: String
            var parts = 1
            var optional = false

            var manifest: ModelArtifact? { ModelManifest.artifact(id: artifact) }
        }
        let activity: String
        let directory: URL
        let ownsDirectory: Bool
        let approxBytes: Int64
        let downloads: [Download]

        func destination(_ download: Download) -> URL { directory.appendingPathComponent(download.file) }
        var primaryFile: URL { destination(downloads[0]) }
        var requiredFiles: [URL] { downloads.filter { !$0.optional }.map(destination) }
    }

    static let ramPlus = ModelInstaller(Spec(
        activity: "Install RAM++ model",
        directory: AppSupportPath.models.appendingPathComponent("ram_plus", isDirectory: true),
        ownsDirectory: true,
        approxBytes: ModelManifest.artifact(id: "ram_plus_model")?.approxBytes ?? 0,
        downloads: [
            .init(artifact: "ram_plus_model", file: "ram_plus.onnx", parts: 12),
            .init(artifact: "ram_plus_tags", file: "ram_plus_tags.txt"),
            .init(artifact: "ram_plus_thresholds", file: "ram_plus_thresholds.txt", optional: true),
        ]))

    static let bge = ModelInstaller(Spec(
        activity: "Install BGE model",
        directory: AppSupportPath.models.appendingPathComponent("bge_text", isDirectory: true),
        ownsDirectory: true,
        approxBytes: ModelManifest.artifact(id: "bge_model")?.approxBytes ?? 0,
        downloads: [
            .init(artifact: "bge_model", file: "bge_small.onnx", parts: 8),
            .init(artifact: "bge_vocab", file: "vocab.txt"),
        ]))

    static let face = ModelInstaller(Spec(
        activity: "Install face model",
        directory: FaceEmbedderKind.modelsDirectory,
        ownsDirectory: false,
        approxBytes: FaceEmbedderKind.sface.approxBytes,
        downloads: [
            .init(artifact: "sface_embedder", file: FaceEmbedderKind.sface.modelFileName, parts: 12),
        ]))

    static let faceMesh = ModelInstaller(Spec(
        activity: "Install eye model",
        directory: AppSupportPath.models.appendingPathComponent("face_mesh", isDirectory: true),
        ownsDirectory: true,
        approxBytes: ModelManifest.artifact(id: "face_mesh_landmarks")?.approxBytes ?? 0,
        downloads: [
            .init(artifact: "face_mesh_landmarks", file: "face_landmarks.onnx"),
        ]))

    let spec: Spec
    private(set) var status: Status = .unknown
    private(set) var presentFilePaths: Set<String> = []
    private var task: Task<Void, Never>?
    private var installing = false
    private var uninstalling = false

    private init(_ spec: Spec) { self.spec = spec }

    func refreshStatus() {
        let fm = FileManager.default
        presentFilePaths = Set(spec.downloads.map { spec.destination($0).path }.filter { fm.fileExists(atPath: $0) })
        if spec.requiredFiles.allSatisfy({ presentFilePaths.contains($0.path) }) {
            let size = spec.ownsDirectory
                ? Self.diskSize(of: spec.directory)
                : spec.downloads.reduce(Int64(0)) { $0 + Self.diskSize(of: spec.destination($1)) }
            status = .installed(sizeBytes: size)
        } else {
            if status.inProgress, task != nil { return }
            if case .installFailed = status { return }
            status = .missing(reason: Self.notDownloaded)
        }
    }

    static let notDownloaded = "Not downloaded yet."

    /// Bytes on disk for a file, or for everything inside a folder.
    nonisolated static func diskSize(of url: URL) -> Int64 {
        let fm = FileManager.default
        var isDir: ObjCBool = false
        guard fm.fileExists(atPath: url.path, isDirectory: &isDir) else { return 0 }
        if !isDir.boolValue {
            return Int64((try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0)
        }
        var total: Int64 = 0
        if let entries = fm.enumerator(at: url, includingPropertiesForKeys: [.fileSizeKey]) {
            for case let file as URL in entries {
                total += Int64((try? file.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0)
            }
        }
        return total
    }

    func install() {
        guard task == nil, !uninstalling else { return }
        task = Task { [weak self, activity = spec.activity] in
            await AppSleepActivity.run(reason: activity) {
                await self?.runInstall()
            }
            self?.task = nil
        }
    }

    func cancel() { task?.cancel() }

    func uninstall() async {
        guard !uninstalling else { return }
        uninstalling = true
        defer { uninstalling = false }
        let active = task
        active?.cancel()
        await active?.value
        task = nil
        let targets = spec.ownsDirectory ? [spec.directory] : spec.downloads.map(spec.destination)
        guard targets.allSatisfy({ (try? ReadOnlyLocations.requireWritable($0)) != nil }) else {
            status = .installFailed(Self.protectedMessage); return
        }
        for target in targets { try? FileManager.default.removeItem(at: target) }
        status = .unknown
        refreshStatus()
    }

    static let protectedMessage = "FileID can't change files in this location."

    nonisolated static func notEnoughSpace(needed: Int64, available: Int64) -> String {
        "Not enough free space: needs \(ByteCountFormatter.string(fromByteCount: needed, countStyle: .file)), \(ByteCountFormatter.string(fromByteCount: available, countStyle: .file)) free."
    }

    private func runInstall() async {
        let dir = spec.directory
        guard (try? ReadOnlyLocations.requireWritable(dir)) != nil,
              spec.downloads.allSatisfy({ (try? ReadOnlyLocations.requireWritable(spec.destination($0))) != nil }) else {
            status = .installFailed(Self.protectedMessage); return
        }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        sweepStaleStagingEntries(in: dir.appendingPathComponent(".fileid-staging", isDirectory: true))

        if let free = Self.freeDiskBytes(at: dir), free < spec.approxBytes * 2 {
            status = .installFailed(Self.notEnoughSpace(needed: spec.approxBytes * 2, available: free))
            return
        }
        let remotes = spec.downloads.map { $0.manifest.flatMap { URL(string: $0.url) } }
        guard remotes.allSatisfy({ $0 != nil }) else {
            status = .installFailed("This download isn't available in this copy of FileID. Reinstall FileID to fix it.")
            return
        }

        status = .downloading(fraction: 0, message: "Connecting…", bytesPerSecond: 0, etaSeconds: 0)
        installing = true
        defer { installing = false }

        do {
            for (index, download) in spec.downloads.enumerated() {
                let remote = remotes[index]!
                let primary = index == 0
                do {
                    try await parallelStreamingDownload(
                        remote: remote, dest: spec.destination(download), parts: download.parts,
                        approxBytes: primary ? spec.approxBytes : 0,
                        expectedSHA256: download.manifest?.sha256
                    ) { tick in
                        guard primary else { return }
                        Task { @MainActor [weak self] in
                            guard let self, self.installing else { return }
                            self.status = .downloading(
                                fraction: tick.total > 0 ? min(1.0, Double(tick.written) / Double(tick.total)) : 0,
                                message: Self.progressMessage(written: tick.written, total: tick.total),
                                bytesPerSecond: tick.bytesPerSecond, etaSeconds: tick.etaSeconds)
                        }
                    }
                } catch where download.optional {
                    continue
                }
            }
        } catch {
            status = Self.failureStatus(error)
            return
        }
        refreshStatus()
    }

    static func progressMessage(written: Int64, total: Int64, suffix: String = "") -> String {
        let mb = Double(written) / 1_048_576.0
        return total > 0
            ? String(format: "Downloading… %.0f / %.0f MB%@", mb, Double(total) / 1_048_576.0, suffix)
            : String(format: "Downloading… %.0f MB%@", mb, suffix)
    }

    static func failureStatus(_ error: Error) -> Status {
        switch error {
        case is CancellationError:
            return .missing(reason: "Cancelled.")
        default:
            return .installFailed(failureText(error))
        }
    }

    /// One plain sentence for a failed download. Status codes, hashes and
    /// system error text stay out of it.
    nonisolated static func failureText(_ error: Error) -> String {
        switch error {
        case StreamingDownloadError.http(let code):
            switch code {
            case 404, 410: return "The download is no longer on the server. Update FileID, then try again."
            case 408, 429: return "The download server is busy. Try again in a few minutes."
            case 500...599: return "The download server is having trouble. Try again later."
            default: return "The download server refused the request. Try again later."
            }
        case StreamingDownloadError.checksumMismatch, StreamingDownloadError.sizeImplausible,
             StreamingDownloadError.missingTempFile:
            return "The download was damaged. Try again."
        case StreamingDownloadError.insufficientDiskSpace(let needed, let available):
            return notEnoughSpace(needed: needed, available: available)
        case StreamingDownloadError.pinningFailed, StreamingDownloadError.redirectBlocked:
            return "The connection to the download server wasn't secure, so FileID stopped. Try again on another network."
        case StreamingDownloadError.underlying(let inner):
            return failureText(inner)
        default:
            break
        }
        let ns = error as NSError
        if ns.domain == NSURLErrorDomain {
            switch URLError.Code(rawValue: ns.code) {
            case .notConnectedToInternet, .networkConnectionLost, .dataNotAllowed, .internationalRoamingOff,
                 .cannotFindHost, .dnsLookupFailed:
                return "No internet connection. Connect, then try again."
            case .timedOut, .cannotConnectToHost:
                return "The download server didn't answer. Check your internet connection, then try again."
            default:
                return "The download was interrupted. Try again."
            }
        }
        if ns.domain == NSCocoaErrorDomain, ns.code == NSFileWriteOutOfSpaceError {
            return "Not enough free space to finish the download. Free some space, then try again."
        }
        if ns.domain == NSPOSIXErrorDomain, ns.code == Int(ENOSPC) {
            return "Not enough free space to finish the download. Free some space, then try again."
        }
        return "The download didn't finish. Try again."
    }

    nonisolated static func freeDiskBytes(at url: URL) -> Int64? {
        (try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]))?
            .volumeAvailableCapacityForImportantUsage
    }
}

/// Plain words for a description-model download or load failure the helper reported.
enum ModelFailureText {
    static func isModelFailure(_ error: EngineError) -> Bool {
        if error.kind == "prewarm_cancelled" { return false }
        return error.kind.hasPrefix("prewarm_")
            || ["model_integrity_failed", "deep_load_failed", "deep_model_too_big", "unknown_model"].contains(error.kind)
    }

    static func plain(_ error: EngineError) -> String {
        switch error.kind {
        case "model_integrity_failed": return "The AI model download was damaged. Try again."
        case "deep_model_too_big": return "This AI model needs more memory than this Mac has. Choose a smaller one in Settings."
        case "unknown_model": return "This copy of FileID doesn't know that AI model. Choose another one in Settings."
        default: break
        }
        let text = error.message.lowercased()
        if ["offline", "internet", "network", "timed out", "hostname", "connection"].contains(where: text.contains) {
            return "FileID couldn't download the AI model. Check your internet connection, then try again."
        }
        if text.contains("space") || text.contains("disk full") {
            return "There isn't enough free space for the AI model. Free some space, then try again."
        }
        if text.contains("memory") {
            return "This Mac is short on memory right now. Close a few apps, then try again."
        }
        return "FileID couldn't get the AI model ready. Try again."
    }
}

/// Where a downloaded description model lives, and the only code that deletes one.
enum DescriptionModelStore {
    struct RemovalError: LocalizedError {
        let reason: String
        var errorDescription: String? { reason }
    }

    /// The model's own folder inside the model cache, or nil when its name
    /// isn't a plain "publisher/model" pair that stays inside the cache.
    static func folder(for kind: AIModelKind) -> URL? {
        let parts = kind.sourceRepo.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        guard parts.count == 2, parts.allSatisfy({ !$0.isEmpty && !$0.hasPrefix(".") && !$0.contains("\\") && !$0.contains("\0") }) else { return nil }
        let root = ModelCachePaths.huggingFaceModels.standardizedFileURL
        let folder = root.appendingPathComponent(parts[0], isDirectory: true)
            .appendingPathComponent(parts[1], isDirectory: true).standardizedFileURL
        let prefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        guard folder.path.hasPrefix(prefix), folder.pathComponents.count == root.pathComponents.count + 2 else { return nil }
        return folder
    }

    static func sizeOnDisk(_ kind: AIModelKind) -> Int64 {
        folder(for: kind).map(ModelInstaller.diskSize(of:)) ?? 0
    }

    static func downloadSizeText(_ kind: AIModelKind) -> String {
        String(format: "%.1f GB", Double(kind.approxBytes) / 1_073_741_824)
    }

    /// Free space on the drive that holds the model cache.
    static func freeDiskBytes() -> Int64? {
        var url = ModelCachePaths.huggingFaceModels
        while !FileManager.default.fileExists(atPath: url.path), url.pathComponents.count > 1 {
            url = url.deletingLastPathComponent()
        }
        return ModelInstaller.freeDiskBytes(at: url)
    }

    /// Deletes one downloaded model's folder. Only a real folder (not a link)
    /// that FileID finished downloading, directly inside the model cache.
    static func remove(_ kind: AIModelKind) throws {
        guard let folder = folder(for: kind) else { throw RemovalError(reason: "The model's folder couldn't be worked out safely.") }
        try ReadOnlyLocations.requireWritable(folder)
        let fm = FileManager.default
        let attributes = try fm.attributesOfItem(atPath: folder.path)
        guard attributes[.type] as? FileAttributeType == .typeDirectory else {
            throw RemovalError(reason: "The model's folder is a link, so FileID left it alone.")
        }
        guard fm.fileExists(atPath: folder.appendingPathComponent(".fileid-installed").path) else {
            throw RemovalError(reason: "FileID didn't finish downloading this model, so it left the folder alone.")
        }
        try fm.removeItem(at: folder)
    }
}
