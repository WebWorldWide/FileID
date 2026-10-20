// User-initiated downloads of the single-directory ONNX models (RAM++, BGE,
// SFace) from their Hugging Face repos. Weights are never redistributed in-app,
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
            case .extracting: "Extracting…"
            case .installFailed(let why): "Failed: \(why)"
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
            let url: String
            let file: String
            var parts = 1
            var optional = false
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
        approxBytes: 925_600_000,
        downloads: [
            .init(url: "https://huggingface.co/Web-World-Wide/ram-plus-onnx/resolve/main/ram_plus.onnx", file: "ram_plus.onnx", parts: 12),
            .init(url: "https://huggingface.co/Web-World-Wide/ram-plus-onnx/resolve/main/ram_plus_tags.txt", file: "ram_plus_tags.txt"),
            .init(url: "https://huggingface.co/Web-World-Wide/ram-plus-onnx/resolve/main/ram_plus_thresholds.txt", file: "ram_plus_thresholds.txt", optional: true),
        ]))

    static let bge = ModelInstaller(Spec(
        activity: "Install BGE model",
        directory: AppSupportPath.models.appendingPathComponent("bge_text", isDirectory: true),
        ownsDirectory: true,
        approxBytes: 135_000_000,
        downloads: [
            .init(url: "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model.onnx", file: "bge_small.onnx", parts: 8),
            .init(url: "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/vocab.txt", file: "vocab.txt"),
        ]))

    static let face = ModelInstaller(Spec(
        activity: "Install face model",
        directory: FaceEmbedderKind.modelsDirectory,
        ownsDirectory: false,
        approxBytes: FaceEmbedderKind.sface.approxBytes,
        downloads: [
            .init(url: "https://huggingface.co/opencv/face_recognition_sface/resolve/main/face_recognition_sface_2021dec.onnx",
                  file: FaceEmbedderKind.sface.modelFileName, parts: 12),
        ]))

    let spec: Spec
    private(set) var status: Status = .unknown
    private(set) var presentFilePaths: Set<String> = []
    private var task: Task<Void, Never>?
    private var installing = false

    private init(_ spec: Spec) { self.spec = spec }

    func refreshStatus() {
        let fm = FileManager.default
        presentFilePaths = Set(spec.downloads.map { spec.destination($0).path }.filter { fm.fileExists(atPath: $0) })
        if spec.requiredFiles.allSatisfy({ presentFilePaths.contains($0.path) }) {
            let size = (try? fm.attributesOfItem(atPath: spec.primaryFile.path)[.size] as? Int64) ?? 0
            status = .installed(sizeBytes: size)
        } else {
            if case .downloading = status, task != nil { return }
            status = .missing(reason: "Not installed.")
        }
    }

    func install() {
        guard task == nil else { return }
        task = Task { [weak self, activity = spec.activity] in
            await AppSleepActivity.run(reason: activity) {
                await self?.runInstall()
            }
            self?.task = nil
        }
    }

    func cancel() { task?.cancel() }

    func uninstall() {
        cancel()
        let targets = spec.ownsDirectory ? [spec.directory] : spec.downloads.map(spec.destination)
        guard targets.allSatisfy({ (try? ReadOnlyLocations.requireWritable($0)) != nil }) else {
            status = .installFailed(Self.protectedMessage); return
        }
        for target in targets { try? FileManager.default.removeItem(at: target) }
        refreshStatus()
    }

    private static let protectedMessage = "This model location is protected example data."

    private func runInstall() async {
        let dir = spec.directory
        guard (try? ReadOnlyLocations.requireWritable(dir)) != nil,
              spec.downloads.allSatisfy({ (try? ReadOnlyLocations.requireWritable(spec.destination($0))) != nil }) else {
            status = .installFailed(Self.protectedMessage); return
        }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        sweepStaleStagingEntries(in: dir.appendingPathComponent(".fileid-staging", isDirectory: true))

        if let free = Self.freeDiskBytes(at: dir), free < spec.approxBytes * 2 {
            status = .installFailed("Not enough free space.")
            return
        }
        let remotes = spec.downloads.map { URL(string: $0.url) }
        guard remotes.allSatisfy({ $0 != nil }) else {
            status = .installFailed("Internal error: bad model URL.")
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
                        expectedSHA256: ModelManifest.sha256(forURL: remote)
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
        case StreamingDownloadError.http(let code):
            return .installFailed("Server returned HTTP \(code).")
        case StreamingDownloadError.checksumMismatch(let expected, let actual):
            return .installFailed("Integrity check failed: the downloaded model's SHA-256 (\(actual.prefix(12))…) doesn't match the pinned manifest hash (\(expected.prefix(12))…). The file was discarded — try again; repeated failures may mean the download was tampered with.")
        default:
            return .installFailed("Download failed: \(error.localizedDescription)")
        }
    }

    static func freeDiskBytes(at url: URL) -> Int64? {
        (try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]))?
            .volumeAvailableCapacityForImportantUsage
    }
}
