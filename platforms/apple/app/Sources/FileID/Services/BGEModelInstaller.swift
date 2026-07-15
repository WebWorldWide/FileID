// Downloads the BGE-small document text embedder (MIT, 384-d) from HuggingFace at runtime —
// same posture as the RAM++/CLIP/ArcFace installers; weights are never redistributed in-app.
// User-initiated only (no-telemetry rule: the sole network egress is an explicit download).
//
// Two files into ~/Library/Application Support/FileID/Models/bge_text/:
//   bge_small.onnx (required, ~135 MB), vocab.txt (required, BERT WordPiece vocab).
// The engine's BGETextService reads whatever lands here; restructure's document pass
// clusters by content when present and falls back to filename tokens if absent.
import Foundation
import AppKit
import FileIDShared

@MainActor
@Observable
public final class BGEModelInstaller {

    public static let shared = BGEModelInstaller()

    public enum Status: Equatable {
        case unknown
        case missing(reason: String)
        case installed(sizeBytes: Int64)
        case downloading(fraction: Double, message: String,
                         bytesPerSecond: Double, etaSeconds: Double)
        case installFailed(String)
    }

    public private(set) var status: Status = .unknown

    private var task: Task<Void, Never>?
    private var installing = false

    // Pins live in ModelManifest (bge_model / bge_vocab) + shared/models/manifest.json.
    private static let onnxURLString = "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model.onnx"
    private static let vocabURLString = "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/vocab.txt"
    private static let approxOnnxBytes: Int64 = 135_000_000

    private init() {}

    // MUST match the engine's BGETextService load path: FileID/Models/bge_text/<file>.
    public static var modelsRoot: URL {
        FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID/Models", isDirectory: true)
    }
    private static var dir: URL { modelsRoot.appendingPathComponent("bge_text", isDirectory: true) }
    private static var onnxDest: URL { dir.appendingPathComponent("bge_small.onnx") }
    private static var vocabDest: URL { dir.appendingPathComponent("vocab.txt") }

    public func refreshStatus() {
        let fm = FileManager.default
        if fm.fileExists(atPath: Self.onnxDest.path), fm.fileExists(atPath: Self.vocabDest.path) {
            let sz = (try? fm.attributesOfItem(atPath: Self.onnxDest.path)[.size] as? Int64) ?? 0
            status = .installed(sizeBytes: sz)
        } else {
            if case .downloading = status, task != nil { return }
            status = .missing(reason: "Not installed.")
        }
    }

    public func install() {
        guard task == nil else { return }
        task = Task { [weak self] in
            await AppSleepActivity.run(reason: "Install BGE model") {
                await self?.runInstall()
            }
            self?.task = nil
        }
    }

    public func cancel() { task?.cancel() }

    public func uninstall() {
        cancel()
        try? FileManager.default.removeItem(at: Self.dir)
        refreshStatus()
    }

    private func runInstall() async {
        try? FileManager.default.createDirectory(at: Self.dir, withIntermediateDirectories: true)
        sweepStaleStagingEntries(
            in: Self.dir.appendingPathComponent(".fileid-staging", isDirectory: true))

        if let free = freeDiskBytes(at: Self.dir), free < Self.approxOnnxBytes * 2 {
            status = .installFailed("Not enough free space.")
            return
        }
        guard let onnxURL = URL(string: Self.onnxURLString),
              let vocabURL = URL(string: Self.vocabURLString) else {
            status = .installFailed("Internal error: bad model URL.")
            return
        }

        status = .downloading(fraction: 0, message: "Connecting…", bytesPerSecond: 0, etaSeconds: 0)
        installing = true
        defer { installing = false }

        do {
            try await parallelStreamingDownload(
                remote: onnxURL, dest: Self.onnxDest, parts: 8,
                approxBytes: Self.approxOnnxBytes,
                expectedSHA256: ModelManifest.sha256(forURL: onnxURL)
            ) { tick in
                Task { @MainActor [weak self] in
                    guard let self, self.installing else { return }
                    let frac = tick.total > 0 ? min(1.0, Double(tick.written) / Double(tick.total)) : 0
                    let mb = Double(tick.written) / 1_048_576.0
                    let totalMB = Double(tick.total) / 1_048_576.0
                    let msg = tick.total > 0
                        ? String(format: "Downloading model… %.0f / %.0f MB", mb, totalMB)
                        : String(format: "Downloading model… %.0f MB", mb)
                    self.status = .downloading(fraction: frac, message: msg,
                                               bytesPerSecond: tick.bytesPerSecond,
                                               etaSeconds: tick.etaSeconds)
                }
            }
            try await parallelStreamingDownload(
                remote: vocabURL, dest: Self.vocabDest, parts: 1,
                expectedSHA256: ModelManifest.sha256(forURL: vocabURL)
            ) { _ in }
        } catch is CancellationError {
            status = .missing(reason: "Cancelled.")
            return
        } catch let StreamingDownloadError.http(code) {
            status = .installFailed("Server returned HTTP \(code).")
            return
        } catch StreamingDownloadError.checksumMismatch(let expected, let actual) {
            status = .installFailed("Integrity check failed: the downloaded model's SHA-256 (\(actual.prefix(12))…) doesn't match the pinned hash (\(expected.prefix(12))…). The file was discarded — try again.")
            return
        } catch {
            status = .installFailed("Download failed: \(error.localizedDescription)")
            return
        }
        refreshStatus()
    }

    private func freeDiskBytes(at url: URL) -> Int64? {
        guard let values = try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]),
              let avail = values.volumeAvailableCapacityForImportantUsage else { return nil }
        return avail
    }
}
