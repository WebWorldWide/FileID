// In-app downloader for the CLIP semantic-search models. Pulls a
// single zip containing both the MobileCLIP-S2 image encoder and the
// text encoder + BPE vocab, extracts to ~/Library/Application Support/
// FileID/Models/, then re-loads CLIPTextEncoder so semantic search
// activates without an app restart.
//
// Mirrors the chunk-streaming download pattern from v1's
// AIModelDownloadService — async URLSession.bytes, 64 KB chunks
// written to a `.partial` temp file, atomic replace into the cache
// directory, native /usr/bin/unzip for extraction.
import Foundation
import AppKit

@MainActor
@Observable
public final class CLIPModelInstaller {

    public static let shared = CLIPModelInstaller()

    public enum Status: Equatable {
        case unknown
        case missing(reason: String)
        case installed(sizeBytes: Int64)
        case downloading(fraction: Double, message: String)
        case extracting
        case installFailed(String)
    }

    public private(set) var status: Status = .unknown
    private var task: Task<Void, Never>?

    private init() {}

    // MARK: - Required files

    /// Files the installer must produce on disk for both the image
    /// embedder (engine) and the text encoder (app) to be usable.
    public static var requiredFiles: [URL] {
        let models = modelsRoot
        return [
            models.appendingPathComponent("mobileclip_image/mobileclip_s2_image.mlpackage"),
            models.appendingPathComponent("clip_text/clip_text.mlpackage"),
            models.appendingPathComponent("clip_text/vocab.json"),
            models.appendingPathComponent("clip_text/merges.txt"),
        ]
    }

    public static var modelsRoot: URL { AppSupportPath.models }

    /// Per-file fetch plan. Each entry is (sourceURL, destination
    /// path on disk). Apple hosts the .mlpackage files; OpenAI hosts
    /// the BPE vocabulary. Both repos are public + stable, so this is
    /// the source of truth — no need for a self-hosted release artifact.
    private static var fetchPlan: [(remote: URL, dest: URL)] {
        let m = modelsRoot
        let imgPkg = m.appendingPathComponent("mobileclip_image/mobileclip_s2_image.mlpackage")
        // CLIPTextEncoder.swift looks for clip_text/clip_text.mlpackage —
        // Apple's file is named mobileclip_s2_text.mlpackage, but we land
        // it under the expected filename so the loader picks it up.
        let txtPkg = m.appendingPathComponent("clip_text/clip_text.mlpackage")
        let txtDir = m.appendingPathComponent("clip_text")
        func appleImg(_ rel: String) -> URL {
            URL(string: "https://huggingface.co/apple/coreml-mobileclip/resolve/main/mobileclip_s2_image.mlpackage/\(rel)")!
        }
        func appleTxt(_ rel: String) -> URL {
            URL(string: "https://huggingface.co/apple/coreml-mobileclip/resolve/main/mobileclip_s2_text.mlpackage/\(rel)")!
        }
        func openaiBpe(_ name: String) -> URL {
            URL(string: "https://huggingface.co/openai/clip-vit-base-patch32/resolve/main/\(name)")!
        }
        return [
            (appleImg("Manifest.json"),
             imgPkg.appendingPathComponent("Manifest.json")),
            (appleImg("Data/com.apple.CoreML/model.mlmodel"),
             imgPkg.appendingPathComponent("Data/com.apple.CoreML/model.mlmodel")),
            (appleImg("Data/com.apple.CoreML/weights/weight.bin"),
             imgPkg.appendingPathComponent("Data/com.apple.CoreML/weights/weight.bin")),
            (appleTxt("Manifest.json"),
             txtPkg.appendingPathComponent("Manifest.json")),
            (appleTxt("Data/com.apple.CoreML/model.mlmodel"),
             txtPkg.appendingPathComponent("Data/com.apple.CoreML/model.mlmodel")),
            (appleTxt("Data/com.apple.CoreML/weights/weight.bin"),
             txtPkg.appendingPathComponent("Data/com.apple.CoreML/weights/weight.bin")),
            (openaiBpe("vocab.json"), txtDir.appendingPathComponent("vocab.json")),
            (openaiBpe("merges.txt"), txtDir.appendingPathComponent("merges.txt")),
        ]
    }

    // MARK: - Status

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

    /// Download every required file from the canonical hosts (Apple
    /// for the .mlpackage encoders, OpenAI for the BPE vocabulary).
    public func install() {
        guard task == nil else { return }
        task = Task { [weak self] in
            await self?.runHubFetch()
            self?.task = nil
        }
    }

    /// Install from a pre-built zip on disk (offline / air-gapped
    /// fallback). Expects the same layout the hub fetch would produce —
    /// `mobileclip_image/...` and `clip_text/...` at the top level.
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

    /// Multi-file fetch from HuggingFace. Pre-sizes the whole job via
    /// HEAD requests so progress reads accurately end-to-end, then
    /// streams each file in 64 KB chunks to a `.partial` temp before
    /// atomic-replacing into place.
    private func runHubFetch() async {
        // Pre-flight: need ~250 MB free for the .mlpackage files +
        // tokenizer.
        if let free = freeDiskBytes(), free < 250 * 1024 * 1024 {
            status = .installFailed("Not enough free space (need ~250 MB).")
            return
        }

        let plan = Self.fetchPlan
        status = .downloading(fraction: 0, message: "Checking sizes…")

        // 1. HEAD every URL to compute total bytes for an accurate bar.
        var sizes: [Int64] = []
        var grandTotal: Int64 = 0
        for (remote, _) in plan {
            do {
                var req = URLRequest(url: remote)
                req.httpMethod = "HEAD"
                let (_, resp) = try await URLSession.shared.data(for: req)
                if let http = resp as? HTTPURLResponse {
                    if !(200..<400).contains(http.statusCode) {
                        status = .installFailed("Couldn't reach \(remote.lastPathComponent) (HTTP \(http.statusCode)).")
                        return
                    }
                    if let lenStr = http.value(forHTTPHeaderField: "Content-Length"),
                       let len = Int64(lenStr) {
                        sizes.append(len); grandTotal += len
                    } else {
                        sizes.append(0)
                    }
                } else {
                    sizes.append(0)
                }
            } catch is CancellationError {
                status = .missing(reason: "Cancelled.")
                return
            } catch {
                status = .installFailed("Couldn't reach the model server. Check your connection.")
                return
            }
        }

        // 2. Stream each file. Running total + accurate per-file
        // progress is shown as "MB / total MB".
        var running: Int64 = 0
        for (idx, entry) in plan.enumerated() {
            do {
                try await downloadOne(remote: entry.remote, dest: entry.dest,
                                       fileIndex: idx + 1, totalFiles: plan.count,
                                       runningStart: running, grandTotal: grandTotal)
            } catch is CancellationError {
                status = .missing(reason: "Cancelled.")
                return
            } catch let DownloadError.failed(msg) {
                status = .installFailed(msg)
                return
            } catch {
                status = .installFailed("Download failed: \(error.localizedDescription)")
                return
            }
            running += sizes[idx]
        }

        status = .extracting   // brief — final validation is fast.
        // Validate every required file landed.
        for f in Self.requiredFiles {
            if !FileManager.default.fileExists(atPath: f.path) {
                status = .installFailed("Missing after install: \(f.lastPathComponent).")
                return
            }
        }

        // Eagerly load the text encoder so search activates now.
        // Engine-side image encoder picks up the new file on next start.
        Task.detached(priority: .utility) { _ = CLIPTextEncoder.shared.load() }
        refreshStatus()
    }

    private enum DownloadError: Error {
        case failed(String)
    }

    /// Stream one file from `remote` into `dest`, atomic-replace on
    /// completion. Updates `status` with overall progress (running
    /// bytes / grand total) every 256 KB.
    private func downloadOne(remote: URL, dest: URL,
                              fileIndex: Int, totalFiles: Int,
                              runningStart: Int64, grandTotal: Int64) async throws {
        try FileManager.default.createDirectory(
            at: dest.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let tmp = dest.appendingPathExtension("partial")
        try? FileManager.default.removeItem(at: tmp)
        FileManager.default.createFile(atPath: tmp.path, contents: nil)
        guard let handle = try? FileHandle(forWritingTo: tmp) else {
            throw DownloadError.failed("Couldn't create temp file for \(dest.lastPathComponent).")
        }

        let label = dest.lastPathComponent
        status = .downloading(
            fraction: grandTotal > 0 ? Double(runningStart) / Double(grandTotal) : 0,
            message: "Downloading \(label) (\(fileIndex)/\(totalFiles))…"
        )

        do {
            let (bytes, response) = try await URLSession.shared.bytes(from: remote)
            guard let http = response as? HTTPURLResponse,
                  (200..<300).contains(http.statusCode) else {
                try? handle.close()
                try? FileManager.default.removeItem(at: tmp)
                let code = (response as? HTTPURLResponse)?.statusCode ?? 0
                throw DownloadError.failed("Download failed (HTTP \(code)) for \(label).")
            }

            var chunk = Data()
            chunk.reserveCapacity(64 * 1024)
            var fileWritten: Int64 = 0
            var sinceUI = 0
            for try await byte in bytes {
                try Task.checkCancellation()
                chunk.append(byte)
                if chunk.count >= 64 * 1024 {
                    try handle.write(contentsOf: chunk)
                    fileWritten += Int64(chunk.count)
                    sinceUI += chunk.count
                    chunk.removeAll(keepingCapacity: true)
                    if sinceUI >= 256 * 1024 {
                        let totalDone = runningStart + fileWritten
                        let frac = grandTotal > 0
                            ? min(1.0, Double(totalDone) / Double(grandTotal)) : 0
                        let mb = Double(totalDone) / 1_048_576.0
                        let totalMB = Double(grandTotal) / 1_048_576.0
                        let msg = grandTotal > 0
                            ? String(format: "Downloading… %.0f / %.0f MB", mb, totalMB)
                            : String(format: "Downloading… %.0f MB", mb)
                        status = .downloading(fraction: frac, message: msg)
                        sinceUI = 0
                    }
                }
            }
            if !chunk.isEmpty {
                try handle.write(contentsOf: chunk)
            }
            try handle.close()

            // Atomic replace.
            if (try? FileManager.default.replaceItemAt(dest, withItemAt: tmp)) == nil {
                try FileManager.default.moveItem(at: tmp, to: dest)
            }
        } catch is CancellationError {
            try? handle.close()
            try? FileManager.default.removeItem(at: tmp)
            throw CancellationError()
        } catch let e as DownloadError {
            try? handle.close()
            try? FileManager.default.removeItem(at: tmp)
            throw e
        } catch {
            try? handle.close()
            try? FileManager.default.removeItem(at: tmp)
            throw DownloadError.failed("\(label): \(error.localizedDescription)")
        }
    }

    private func runExtract(zipAt zipURL: URL, deleteZipAfter: Bool) async {
        status = .extracting
        let modelsRoot = Self.modelsRoot
        try? FileManager.default.createDirectory(at: modelsRoot, withIntermediateDirectories: true)

        // Validate the zip path before invoking unzip. `Process` arg
        // arrays don't go through a shell so command injection isn't
        // possible, but a missing/non-zip/symlinked path could confuse
        // unzip or leak unintended behavior. Hard-fail early with a
        // clear error.
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

        // Use /usr/bin/unzip — supports .mlpackage subdirectories
        // natively, no third-party dependency.
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/unzip")
        proc.arguments = ["-o", "-q", zipURL.path, "-d", modelsRoot.path]
        let stderr = Pipe()
        proc.standardError = stderr
        proc.standardOutput = Pipe()

        do {
            try proc.run()
            // Wait off-MainActor.
            await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
                proc.terminationHandler = { _ in cont.resume() }
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

        // Validate every required file landed.
        for f in Self.requiredFiles {
            if !FileManager.default.fileExists(atPath: f.path) {
                status = .installFailed("Zip didn't contain \(f.lastPathComponent).")
                return
            }
        }

        if deleteZipAfter {
            try? FileManager.default.removeItem(at: zipURL)
        }

        // Eagerly load the text encoder so search activates now.
        // Engine-side image encoder picks up the new file on next start.
        Task.detached(priority: .utility) { _ = CLIPTextEncoder.shared.load() }

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
