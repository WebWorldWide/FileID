// User-initiated installs of the single-directory ONNX models (RAM++, BGE,
// SFace, Face Mesh). The engine downloads them from their Hugging Face repos and verifies
// every file against ModelManifest; this app opens no connection of its own.
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
        /// The `prewarmModel` kind the Rust engine installs this as.
        let engineKind: String

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
        ],
        engineKind: "ram_plus"))

    static let bge = ModelInstaller(Spec(
        activity: "Install BGE model",
        directory: AppSupportPath.models.appendingPathComponent("bge_text", isDirectory: true),
        ownsDirectory: true,
        approxBytes: ModelManifest.artifact(id: "bge_model")?.approxBytes ?? 0,
        downloads: [
            .init(artifact: "bge_model", file: "bge_small.onnx", parts: 8),
            .init(artifact: "bge_vocab", file: "vocab.txt"),
        ],
        engineKind: "bge_text"))

    static let face = ModelInstaller(Spec(
        activity: "Install face model",
        directory: FaceEmbedderKind.modelsDirectory,
        ownsDirectory: false,
        approxBytes: FaceEmbedderKind.sface.approxBytes,
        downloads: [
            .init(artifact: "sface_embedder", file: FaceEmbedderKind.sface.modelFileName, parts: 12),
        ],
        engineKind: "arcface"))

    static let faceMesh = ModelInstaller(Spec(
        activity: "Install eye model",
        directory: AppSupportPath.models.appendingPathComponent("face_mesh", isDirectory: true),
        ownsDirectory: true,
        approxBytes: ModelManifest.artifact(id: "face_mesh_landmarks")?.approxBytes ?? 0,
        downloads: [
            .init(artifact: "face_mesh_landmarks", file: "face_landmarks.onnx"),
        ],
        engineKind: "face_mesh"))

    let spec: Spec
    private(set) var status: Status = .unknown
    private(set) var presentFilePaths: Set<String> = []
    private var task: Task<Void, Never>?
    private var installing = false
    private var uninstalling = false

    private init(_ spec: Spec) { self.spec = spec }

    func refreshStatus() {
        let found = spec.downloads.map(spec.destination).compactMap { destination in
            ModelFileLayout.installedFile(destination).map { (destination: destination, installed: $0) }
        }
        presentFilePaths = Set(found.map(\.destination.path))
        if spec.requiredFiles.allSatisfy({ presentFilePaths.contains($0.path) }) {
            let size = spec.ownsDirectory
                ? Self.diskSize(of: spec.directory)
                : found.reduce(Int64(0)) { $0 + Self.diskSize(of: $1.installed) }
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
        // Lists each folder itself: on exFAT the URL enumerator prefetching sizes fails with EINVAL
        // for some folders and then drops their contents without a word.
        var total: Int64 = 0
        var pending = [url]
        while let directory = pending.popLast() {
            guard let children = try? fm.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) else { continue }
            for child in children {
                let values = try? child.resourceValues(forKeys: [.isDirectoryKey, .fileSizeKey])
                if values?.isDirectory == true {
                    pending.append(child)
                } else {
                    total += Int64(values?.fileSize ?? 0)
                }
            }
        }
        return total
    }

    func install() {
        guard task == nil, !uninstalling else { return }
        task = Task { [weak self, activity = spec.activity] in
            await AppSleepActivity.run(reason: activity) {
                await self?.runEngineInstall()
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
        if !spec.ownsDirectory {
            for target in targets {
                if let other = ModelFileLayout.alternate(of: target, in: AppSupportPath.models) { try? FileManager.default.removeItem(at: other) }
            }
        }
        status = .unknown
        refreshStatus()
    }

    static let protectedMessage = "FileID can’t change files in this location."

    nonisolated static func notEnoughSpace(needed: Int64, available: Int64) -> String {
        "Not enough free space: needs \(ByteCountFormatter.string(fromByteCount: needed, countStyle: .file)), \(ByteCountFormatter.string(fromByteCount: available, countStyle: .file)) free."
    }

    private func runEngineInstall() async {
        guard EngineModelInstall.active else {
            status = .installFailed(EngineModelInstall.notResponding)
            return
        }
        status = .downloading(fraction: 0, message: "Connecting…", bytesPerSecond: 0, etaSeconds: 0)
        let outcome = await EngineModelInstall.install(kinds: [spec.engineKind], files: spec.downloads.map(spec.destination)) { [weak self] fraction, message in
            guard let self, self.task != nil else { return }
            self.status = .downloading(fraction: fraction, message: message, bytesPerSecond: 0, etaSeconds: 0)
        }
        switch outcome {
        case .finished: refreshStatus()
        case .cancelled: status = .missing(reason: "Cancelled.")
        case .failed(let message): status = .installFailed(message)
        }
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
            || ["model_integrity_failed", "deep_load_failed", "deep_model_too_big", "unknown_model", "vlm_model_missing"].contains(error.kind)
    }

    static func plain(_ error: EngineError) -> String {
        switch error.kind {
        case "model_integrity_failed": return "The AI model download was damaged. Try again."
        case "deep_model_too_big": return "This AI model needs more memory than this Mac has. Choose a smaller one in Settings."
        case "unknown_model": return "This copy of FileID doesn’t know that AI model. Choose another one in Settings."
        case "vlm_model_missing": return "This AI model isn’t downloaded yet. Try again to download it first."
        default: break
        }
        let text = error.message.lowercased()
        if ["offline", "internet", "network", "timed out", "hostname", "connection"].contains(where: text.contains) {
            return "FileID couldn’t download the AI model. Check your internet connection, then try again."
        }
        if text.contains("space") || text.contains("disk full") {
            return "There isn’t enough free space for the AI model. Free some space, then try again."
        }
        if text.contains("memory") {
            return "This Mac is short on memory right now. Close a few apps, then try again."
        }
        return "FileID couldn’t get the AI model ready. Try again."
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
        ModelSize.label(Int64(kind.approxBytes))
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
        guard let folder = folder(for: kind) else { throw RemovalError(reason: "The model’s folder couldn’t be worked out safely.") }
        try ReadOnlyLocations.requireWritable(folder)
        let fm = FileManager.default
        let attributes = try fm.attributesOfItem(atPath: folder.path)
        guard attributes[.type] as? FileAttributeType == .typeDirectory else {
            throw RemovalError(reason: "The model’s folder is a link, so FileID left it alone.")
        }
        guard fm.fileExists(atPath: folder.appendingPathComponent(".fileid-installed").path) else {
            throw RemovalError(reason: "FileID didn’t finish downloading this model, so it left the folder alone.")
        }
        try fm.removeItem(at: folder)
    }
}
