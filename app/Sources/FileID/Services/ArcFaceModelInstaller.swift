// In-app downloader for ArcFace face-recognition .mlpackages. Mirrors
// CLIPModelInstaller's pattern: a single zip per variant fetched from
// the GitHub release, streamed in 64 KB chunks, atomic-replaced into
// ~/Library/Application Support/FileID/Models/.
//
// The zips are produced once per release by `scripts/build_arcface_assets.sh`
// (which runs convert_arcface.py and zips the output). Hosting them on
// the GitHub release means users never need Python locally.
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
        case extracting
        case installFailed(String)
    }

    public private(set) var status: [FaceEmbedderKind: Status] = [
        .arcfaceIResNet50:  .unknown,
        .arcfaceMobileFace: .unknown,
    ]

    private var tasks: [FaceEmbedderKind: Task<Void, Never>] = [:]

    private init() {}

    // MARK: - Asset URLs

    /// Where on disk each variant's .mlpackage lives.
    public static func destination(for kind: FaceEmbedderKind) -> URL {
        FaceEmbedderKind.modelsDirectory.appendingPathComponent(kind.modelFileName)
    }

    /// GitHub release asset for each variant. Must match the asset name
    /// used by `scripts/build_arcface_assets.sh` when uploading.
    private static func assetURL(for kind: FaceEmbedderKind) -> URL {
        let base = "https://github.com/AdamNolle/FileID/releases/download/v0.1.0"
        let name: String
        switch kind {
        case .arcfaceIResNet50:  name = "arcface_iresnet50.mlpackage.zip"
        case .arcfaceMobileFace: name = "arcface_mobileface.mlpackage.zip"
        }
        return URL(string: "\(base)/\(name)")!
    }

    // MARK: - Status

    public func refreshStatus() {
        for kind in FaceEmbedderKind.allCases {
            let url = Self.destination(for: kind)
            if FileManager.default.fileExists(atPath: url.path) {
                status[kind] = .installed(sizeBytes: directorySize(url))
            } else {
                if case .downloading = status[kind] { continue }
                if case .extracting  = status[kind] { continue }
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

        if let free = freeDiskBytes(at: modelsRoot),
           free < kind.approxBytes * 3 {
            status[kind] = .installFailed("Not enough free space.")
            return
        }

        let remote = Self.assetURL(for: kind)
        let tmpZip = modelsRoot.appendingPathComponent(".\(kind.rawValue).download.zip")
        try? FileManager.default.removeItem(at: tmpZip)

        status[kind] = .downloading(fraction: 0, message: "Connecting…")

        do {
            try await downloadZip(remote: remote, dest: tmpZip, kind: kind)
        } catch is CancellationError {
            try? FileManager.default.removeItem(at: tmpZip)
            status[kind] = .missing(reason: "Cancelled.")
            return
        } catch let DownloadError.failed(msg) {
            try? FileManager.default.removeItem(at: tmpZip)
            status[kind] = .installFailed(msg)
            return
        } catch {
            try? FileManager.default.removeItem(at: tmpZip)
            status[kind] = .installFailed("Download failed: \(error.localizedDescription)")
            return
        }

        status[kind] = .extracting
        let extracted = await extractZip(zipURL: tmpZip, into: modelsRoot, kind: kind)
        try? FileManager.default.removeItem(at: tmpZip)

        if !extracted {
            // status already set to installFailed inside extractZip
            return
        }

        let dest = Self.destination(for: kind)
        guard FileManager.default.fileExists(atPath: dest.path) else {
            status[kind] = .installFailed("Extracted archive didn't contain \(kind.modelFileName).")
            return
        }

        refreshStatus()
    }

    private enum DownloadError: Error {
        case failed(String)
    }

    /// Stream the release asset into `dest`. Updates per-variant status
    /// every ~256 KB. Atomic-replaces on completion.
    private func downloadZip(remote: URL, dest: URL, kind: FaceEmbedderKind) async throws {
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

    /// Run /usr/bin/unzip with a 5-minute watchdog, same pattern the
    /// CLIP installer uses for its offline-zip path.
    private func extractZip(zipURL: URL, into target: URL, kind: FaceEmbedderKind) async -> Bool {
        // Pre-clear any prior install so the new package replaces it
        // wholesale rather than getting merged on top.
        let dest = Self.destination(for: kind)
        try? FileManager.default.removeItem(at: dest)

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/unzip")
        proc.arguments = ["-o", "-q", zipURL.path, "-d", target.path]
        let stderr = Pipe()
        proc.standardError = stderr
        proc.standardOutput = Pipe()

        do {
            try proc.run()
        } catch {
            status[kind] = .installFailed("Couldn't run unzip: \(error.localizedDescription)")
            return false
        }

        // Bound at 5 minutes. iresnet50 unzips in <30 s in practice; the
        // watchdog protects against a degenerate archive.
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

        guard proc.terminationStatus == 0 else {
            let errData: Data = ((try? stderr.fileHandleForReading.readToEnd()) ?? nil) ?? Data()
            let errStr = String(data: errData, encoding: .utf8) ?? ""
            status[kind] = .installFailed("Extract failed (\(proc.terminationStatus)): \(errStr.trimmingCharacters(in: .whitespacesAndNewlines))")
            return false
        }
        return true
    }

    // MARK: - Utilities

    private func freeDiskBytes(at url: URL) -> Int64? {
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
