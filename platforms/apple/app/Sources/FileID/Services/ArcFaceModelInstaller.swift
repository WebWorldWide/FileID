// Downloads ArcFace ONNX from Immich's HuggingFace mirror at runtime —
// same posture Immich itself uses; we never redistribute weights.
import Foundation
import AppKit
import FileIDShared

@MainActor
@Observable
public final class ArcFaceModelInstaller {

    public static let shared = ArcFaceModelInstaller()

    public enum Status: Equatable {
        case unknown
        case missing(reason: String)
        case installed(sizeBytes: Int64)
        case downloading(fraction: Double, message: String,
                         bytesPerSecond: Double, etaSeconds: Double)
        case installFailed(String)
    }

    public private(set) var status: [FaceEmbedderKind: Status] = [
        .sface: .unknown,
    ]

    private var tasks: [FaceEmbedderKind: Task<Void, Never>] = [:]

    private init() {}

    public static func destination(for kind: FaceEmbedderKind) -> URL {
        FaceEmbedderKind.modelsDirectory.appendingPathComponent(kind.modelFileName)
    }

    private static func sourceURL(for kind: FaceEmbedderKind) -> URL? {
        let base = "https://huggingface.co"
        let path: String
        switch kind {
        case .sface:
            // OpenCV Zoo SFace (Apache-2.0) — commercial-clean replacement for
            // the non-commercial InsightFace ArcFace (immich-app/buffalo_*).
            path = "/opencv/face_recognition_sface/resolve/main/face_recognition_sface_2021dec.onnx"
        }
        return URL(string: "\(base)\(path)")
    }

    public func refreshStatus() {
        for kind in FaceEmbedderKind.allCases {
            let url = Self.destination(for: kind)
            if FileManager.default.fileExists(atPath: url.path) {
                let sz = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? Int64) ?? 0
                status[kind] = .installed(sizeBytes: sz)
            } else {
                if case .downloading = status[kind] { continue }
                status[kind] = .missing(reason: "Not installed.")
            }
        }
    }

    public func install(_ kind: FaceEmbedderKind) {
        guard tasks[kind] == nil else { return }
        tasks[kind] = Task { [weak self] in
            await self?.runInstall(kind)
            self?.tasks[kind] = nil
        }
    }

    public func cancel(_ kind: FaceEmbedderKind) {
        tasks[kind]?.cancel()
    }

    public func uninstall(_ kind: FaceEmbedderKind) {
        cancel(kind)
        let url = Self.destination(for: kind)
        try? FileManager.default.removeItem(at: url)
        refreshStatus()
    }

    private func runInstall(_ kind: FaceEmbedderKind) async {
        let modelsRoot = FaceEmbedderKind.modelsDirectory
        try? FileManager.default.createDirectory(at: modelsRoot, withIntermediateDirectories: true)

        if let free = freeDiskBytes(at: modelsRoot),
           free < kind.approxBytes * 2 {
            status[kind] = .installFailed("Not enough free space.")
            return
        }

        let dest = Self.destination(for: kind)
        guard let remote = Self.sourceURL(for: kind) else {
            status[kind] = .installFailed("Internal error: bad model URL.")
            return
        }
        status[kind] = .downloading(fraction: 0, message: "Connecting…",
                                    bytesPerSecond: 0, etaSeconds: 0)

        do {
            // Multi-part — single-stream gets ~1 MB/s from HF/Cloudflare.
            try await parallelStreamingDownload(remote: remote, dest: dest, parts: 12,
                                                 approxBytes: kind.approxBytes,
                                                 expectedSHA256: ModelManifest.sha256(forURL: remote)) { tick in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    let frac = tick.total > 0
                        ? min(1.0, Double(tick.written) / Double(tick.total))
                        : 0
                    let mb = Double(tick.written) / 1_048_576.0
                    let totalMB = Double(tick.total) / 1_048_576.0
                    let msg = tick.total > 0
                        ? String(format: "Downloading… %.0f / %.0f MB", mb, totalMB)
                        : String(format: "Downloading… %.0f MB", mb)
                    self.status[kind] = .downloading(
                        fraction: frac, message: msg,
                        bytesPerSecond: tick.bytesPerSecond,
                        etaSeconds: tick.etaSeconds
                    )
                }
            }
        } catch is CancellationError {
            status[kind] = .missing(reason: "Cancelled.")
            return
        } catch let StreamingDownloadError.http(code) {
            status[kind] = .installFailed("Server returned HTTP \(code).")
            return
        } catch StreamingDownloadError.checksumMismatch(let expected, let actual) {
            status[kind] = .installFailed("Integrity check failed: the downloaded model's SHA-256 (\(actual.prefix(12))…) doesn't match the pinned manifest hash (\(expected.prefix(12))…). The file was discarded — try again; repeated failures may mean the download was tampered with.")
            return
        } catch {
            status[kind] = .installFailed("Download failed: \(error.localizedDescription)")
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
