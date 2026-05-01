// In-app downloader for the ArcFace face-recognition ONNX models.
// Pulls the original Buffalo ONNX from Immich's HuggingFace repo at
// runtime — same legal posture Immich itself uses, no redistribution
// of the InsightFace pre-trained weights on our part.
//
// Mirrors CLIPModelInstaller's chunk-streaming pattern: async
// URLSession.bytes, 64 KB chunks written to a `.partial` temp file,
// atomic replace into the destination. Single file per variant.
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
        case downloading(fraction: Double, message: String)
        case installFailed(String)
    }

    public private(set) var status: [FaceEmbedderKind: Status] = [
        .arcfaceIResNet50:  .unknown,
        .arcfaceMobileFace: .unknown,
    ]

    private var tasks: [FaceEmbedderKind: Task<Void, Never>] = [:]

    private init() {}

    // MARK: - Asset URLs

    /// Where on disk each variant's ONNX file lives.
    public static func destination(for kind: FaceEmbedderKind) -> URL {
        FaceEmbedderKind.modelsDirectory.appendingPathComponent(kind.modelFileName)
    }

    /// Upstream ONNX URL for each variant. Immich hosts the same model
    /// they ship with their own self-hosted server — InsightFace's
    /// original ONNX, untouched. We fetch it byte-for-byte at first
    /// launch.
    private static func sourceURL(for kind: FaceEmbedderKind) -> URL {
        let base = "https://huggingface.co"
        let path: String
        switch kind {
        case .arcfaceIResNet50:
            path = "/immich-app/buffalo_l/resolve/main/recognition/model.onnx"
        case .arcfaceMobileFace:
            path = "/immich-app/buffalo_s/resolve/main/recognition/model.onnx"
        }
        return URL(string: "\(base)\(path)")!
    }

    // MARK: - Status

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

    // MARK: - Install

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

    // MARK: - Implementation

    private func runInstall(_ kind: FaceEmbedderKind) async {
        let modelsRoot = FaceEmbedderKind.modelsDirectory
        try? FileManager.default.createDirectory(at: modelsRoot, withIntermediateDirectories: true)

        // Pre-flight: need ~2x the approximate model size free.
        if let free = freeDiskBytes(at: modelsRoot),
           free < kind.approxBytes * 2 {
            status[kind] = .installFailed("Not enough free space.")
            return
        }

        let dest = Self.destination(for: kind)
        let remote = Self.sourceURL(for: kind)
        status[kind] = .downloading(fraction: 0, message: "Connecting…")

        do {
            try await downloadFile(remote: remote, dest: dest, kind: kind)
        } catch is CancellationError {
            status[kind] = .missing(reason: "Cancelled.")
            return
        } catch let DownloadError.failed(msg) {
            status[kind] = .installFailed(msg)
            return
        } catch {
            status[kind] = .installFailed("Download failed: \(error.localizedDescription)")
            return
        }
        refreshStatus()
    }

    private enum DownloadError: Error {
        case failed(String)
    }

    /// Stream the ONNX file into `dest`. Updates per-variant status
    /// every ~256 KB. Atomic-replaces on completion.
    private func downloadFile(remote: URL, dest: URL, kind: FaceEmbedderKind) async throws {
        let partial = dest.appendingPathExtension("partial")
        try? FileManager.default.removeItem(at: partial)
        FileManager.default.createFile(atPath: partial.path, contents: nil)
        guard let handle = try? FileHandle(forWritingTo: partial) else {
            throw DownloadError.failed("Couldn't create temp file.")
        }
        defer { try? handle.close() }

        let (bytes, response) = try await URLSession.shared.bytes(from: remote)
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode) else {
            let code = (response as? HTTPURLResponse)?.statusCode ?? 0
            throw DownloadError.failed("Server returned HTTP \(code).")
        }

        let total = Int64(http.value(forHTTPHeaderField: "Content-Length") ?? "") ?? 0
        var chunk = Data()
        chunk.reserveCapacity(64 * 1024)
        var written: Int64 = 0
        var sinceUI = 0

        for try await byte in bytes {
            try Task.checkCancellation()
            chunk.append(byte)
            if chunk.count >= 64 * 1024 {
                try handle.write(contentsOf: chunk)
                written += Int64(chunk.count)
                sinceUI += chunk.count
                chunk.removeAll(keepingCapacity: true)
                if sinceUI >= 256 * 1024 {
                    let frac = total > 0 ? min(1.0, Double(written) / Double(total)) : 0
                    let mb = Double(written) / 1_048_576.0
                    let totalMB = Double(total) / 1_048_576.0
                    let msg = total > 0
                        ? String(format: "Downloading… %.0f / %.0f MB", mb, totalMB)
                        : String(format: "Downloading… %.0f MB", mb)
                    status[kind] = .downloading(fraction: frac, message: msg)
                    sinceUI = 0
                }
            }
        }
        if !chunk.isEmpty {
            try handle.write(contentsOf: chunk)
        }
        try handle.close()

        if (try? FileManager.default.replaceItemAt(dest, withItemAt: partial)) == nil {
            try FileManager.default.moveItem(at: partial, to: dest)
        }
    }

    // MARK: - Utilities

    private func freeDiskBytes(at url: URL) -> Int64? {
        guard let values = try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]),
              let avail = values.volumeAvailableCapacityForImportantUsage else { return nil }
        return avail
    }
}
