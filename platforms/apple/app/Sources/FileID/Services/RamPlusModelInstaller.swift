// Downloads the RAM++ tagger (Apache-2.0, Swin-Large @384, 4585-tag) from the
// project's HuggingFace mirror at runtime — same posture as the CLIP/ArcFace
// installers; weights are never redistributed in-app. User-initiated only (the
// no-telemetry rule: the sole network egress is an explicit model download).
//
// Three files into ~/Library/Application Support/FileID/Models/ram_plus/:
//   ram_plus.onnx (required, large), ram_plus_tags.txt (required),
//   ram_plus_thresholds.txt (optional per-class cutoffs — best-effort).
// Mirrors ArcFaceModelInstaller's structure; the engine's RamPlusService reads
// whatever lands here and falls back to Vision tags if absent.
import Foundation
import AppKit
import FileIDShared

@MainActor
@Observable
public final class RamPlusModelInstaller {

    public static let shared = RamPlusModelInstaller()

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
    /// True only while a download is actually in flight — gates progress ticks
    /// that can be scheduled after a terminal status (phantom "Downloading…"
    /// guard, mirrors ArcFaceModelInstaller.active / CLIPModelInstaller).
    private var installing = false

    private static let repoBase = "https://huggingface.co/Web-World-Wide/ram-plus-onnx/resolve/main"
    private static let approxOnnxBytes: Int64 = 925_600_000  // Swin-L @384 ONNX, per manifest.json

    private init() {}

    // Paths computed locally (the engine's RamPlusService lives in another module);
    // these MUST match RamPlusService's: FileID/Models/ram_plus/<file>.
    public static var modelsRoot: URL {
        FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("FileID/Models", isDirectory: true)
    }
    private static var dir: URL { modelsRoot.appendingPathComponent("ram_plus", isDirectory: true) }
    private static var onnxDest: URL { dir.appendingPathComponent("ram_plus.onnx") }
    private static var tagsDest: URL { dir.appendingPathComponent("ram_plus_tags.txt") }
    private static var thresholdsDest: URL { dir.appendingPathComponent("ram_plus_thresholds.txt") }

    public func refreshStatus() {
        let fm = FileManager.default
        if fm.fileExists(atPath: Self.onnxDest.path), fm.fileExists(atPath: Self.tagsDest.path) {
            let sz = (try? fm.attributesOfItem(atPath: Self.onnxDest.path)[.size] as? Int64) ?? 0
            status = .installed(sizeBytes: sz)
        } else {
            // Preserve .downloading only while a task is alive, so a phantom row
            // self-heals from disk state (mirrors the other installers).
            if case .downloading = status, task != nil { return }
            status = .missing(reason: "Not installed.")
        }
    }

    public func install() {
        guard task == nil else { return }
        task = Task { [weak self] in
            await self?.runInstall()
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

        // Reclaim parts orphaned by a kill mid-download BEFORE the free-space
        // preflight, so ~900 MB of stale staging can't false-fail it (mirrors
        // ArcFaceModelInstaller). Staging lives under the ram_plus subdir.
        sweepStaleStagingEntries(
            in: Self.dir.appendingPathComponent(".fileid-staging", isDirectory: true))

        if let free = freeDiskBytes(at: Self.dir), free < Self.approxOnnxBytes * 2 {
            status = .installFailed("Not enough free space.")
            return
        }
        guard let onnxURL = URL(string: "\(Self.repoBase)/ram_plus.onnx"),
              let tagsURL = URL(string: "\(Self.repoBase)/ram_plus_tags.txt"),
              let thrURL = URL(string: "\(Self.repoBase)/ram_plus_thresholds.txt") else {
            status = .installFailed("Internal error: bad model URL.")
            return
        }

        status = .downloading(fraction: 0, message: "Connecting…", bytesPerSecond: 0, etaSeconds: 0)
        installing = true
        defer { installing = false }

        do {
            // The large ONNX — multi-part (single-stream gets ~1 MB/s from HF).
            try await parallelStreamingDownload(
                remote: onnxURL, dest: Self.onnxDest, parts: 12,
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
            // The required tag list (small, single-stream).
            try await parallelStreamingDownload(
                remote: tagsURL, dest: Self.tagsDest, parts: 1,
                expectedSHA256: ModelManifest.sha256(forURL: tagsURL)
            ) { _ in }
            // The optional per-class thresholds sidecar — best-effort; the engine
            // falls back to the global cutoff if it's absent.
            do {
                try await parallelStreamingDownload(
                    remote: thrURL, dest: Self.thresholdsDest, parts: 1,
                    expectedSHA256: ModelManifest.sha256(forURL: thrURL)
                ) { _ in }
            } catch {
                JSONLogClientNote("ram_plus thresholds sidecar download skipped: \(error.localizedDescription)")
            }
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

// Local-only note (no telemetry) for the best-effort sidecar skip.
private func JSONLogClientNote(_ message: String) {
    #if DEBUG
    print("[RamPlusModelInstaller] \(message)")
    #endif
}
