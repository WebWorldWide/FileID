// VLMDownloader — fetches every file in a HuggingFace VLM repo using
// our 8-way parallelStreamingDownload, then lays them out in the
// directory shape swift-transformers' HubApi expects:
//   <documentsHF>/models/<repo>/<file>
//
// Why this exists: swift-transformers' Hub fetcher is single-stream
// per file. On a per-IP-throttled CDN like HF/Cloudflare that caps a
// single connection at ~500 KB/s, a 3 GB Qwen weight file takes an
// hour. Splitting one file into 8 ranged GETs over 8 separate TCP
// connections multiplies effective throughput. After we've laid the
// files down ourselves, DeepAnalyze calls VLMModelFactory with
// `useOfflineMode: true` and it loads from the local cache.
//
// Resumable: we skip files that already exist at the expected size —
// but only once a `.fileid-verified-<revision>` sentinel attests a
// fully verified fetch; without it, on-disk files are re-hashed
// against the LFS oid before being trusted.
// Integrity: the tree listing + file URLs are pinned to the immutable
// HF revision from ModelManifest, and each LFS file's sha256 (its LFS
// oid) is enforced by the downloader before the atomic promote. Small
// non-LFS files (configs, tokenizer JSON) carry no oid — they are
// fetched unverified and noted in the log.
// Progress: a single fraction across all files, weighted by byte
// total reported by the HF tree API.
import Foundation
import FileIDShared

public struct VLMRepoFile: Sendable {
    public let path: String
    public let size: Int64
    public let sha256: String?
}

public enum VLMDownloaderError: Error {
    case treeListFailed(status: Int)
    case treeDecodeFailed
    case noFilesListed
}

public actor VLMDownloader {
    public static let shared = VLMDownloader()

    private init() {}

    /// Fetch every file in `repo` (e.g. `lmstudio-community/Qwen3-…`)
    /// into `<documentsHF>/models/<repo>/`. Idempotent — files that
    /// already exist on disk at the expected size are skipped.
    /// `progress` is invoked with (fraction 0..1, bytesDoneAcrossAllFiles,
    /// totalBytesAcrossAllFiles) from URLSession's queue, throttled to
    /// at most 10 Hz by streamingDownload's delegate.
    public func fetchRepo(
        repo: String,
        documentsHF: URL,
        progress: @escaping @Sendable (Double, Int64, Int64) -> Void
    ) async throws {
        let revision = ModelManifest.vlmPin(forRepo: repo)?.revision ?? "main"
        let files = try await listRepoFiles(repo: repo, revision: revision)
        let downloadable = files.filter { Self.shouldDownload($0) }
        guard !downloadable.isEmpty else { throw VLMDownloaderError.noFilesListed }

        let modelDir = documentsHF
            .appending(component: "models")
            .appending(component: repo)
        try FileManager.default.createDirectory(at: modelDir, withIntermediateDirectories: true)

        // Size-based skip is only trusted once a prior fetch of THIS
        // revision fully verified — otherwise pre-hardening installs
        // (or a swapped file) would dodge the hash check forever. With
        // no sentinel, an on-disk file must re-prove its LFS sha256
        // (streamed re-hash, no download) before it's skipped.
        let verifiedSentinel = modelDir.appendingPathComponent(".fileid-verified-\(revision)")
        let sentinelValid = FileManager.default.fileExists(atPath: verifiedSentinel.path)

        let unverified = downloadable.filter { $0.sha256 == nil }.map(\.path)
        if !unverified.isEmpty {
            JSONLog.shared.info(ev: "vlm_fetch_unverified_configs",
                                extra: ["repo": AnyCodable(repo),
                                        "files": AnyCodable(unverified.joined(separator: ","))])
        }

        // Total = sum of bytes for files we'll *actually* fetch
        // (not skipped). UI progress is byte-weighted across files.
        var todo: [VLMRepoFile] = []
        let done: Int64 = 0
        var totalToFetch: Int64 = 0
        for f in downloadable {
            let dest = modelDir.appendingPathComponent(f.path)
            if let onDisk = fileSize(dest), onDisk == f.size, f.size > 0 {
                if sentinelValid {
                    continue
                }
                if let expected = f.sha256,
                   let actual = try? sha256HexOfFile(at: dest),
                   actual == expected.lowercased() {
                    continue
                }
            }
            todo.append(f)
            totalToFetch += f.size
        }

        if todo.isEmpty {
            Self.writeVerifiedSentinel(verifiedSentinel)
            progress(1.0, 0, 0)
            return
        }

        // Track per-file in-flight bytes so the aggregate progress is
        // monotonic instead of jumping per file. The streamingDownload
        // closure fires from URLSession's queue, so synchronization is
        // needed.
        let tracker = AggregateTracker()
        await tracker.setBaseDone(done)
        await tracker.setTotal(totalToFetch + done)

        for f in todo {
            try Task.checkCancellation()
            // Reject any HF tree path that would escape the model
            // dir. Treats `..` segments and absolute paths as hostile.
            // Belt-and-suspenders — HF's API doesn't return such paths
            // today, but a malicious repo could.
            guard !f.path.hasPrefix("/"),
                  !f.path.split(separator: "/").contains("..") else {
                continue
            }
            let dest = modelDir.appendingPathComponent(f.path)
            let normalizedDest = dest.standardizedFileURL.path
            let normalizedRoot = modelDir.standardizedFileURL.path
            guard normalizedDest.hasPrefix(normalizedRoot + "/") else {
                continue
            }
            try FileManager.default.createDirectory(
                at: dest.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            // HuggingFace `resolve/<revision>/<path>` works for both
            // LFS (safetensors) and small files. Range support is on.
            // URLs from the API may need percent-encoding.
            let pathEncoded = f.path
                .addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? f.path
            guard let url = URL(string:
                "https://huggingface.co/\(repo)/resolve/\(revision)/\(pathEncoded)"
            ) else { continue }

            let fileSize = f.size
            let fileStartTotalDone = await tracker.totalDone()
            try await parallelStreamingDownload(
                remote: url, dest: dest,
                // 1 part for tiny files, scaling to 12 for the big
                // safetensors. ~4 MB per part keeps each chunk in the
                // sweet spot between TCP slow-start and HF per-stream
                // throttle (~600 KB/s).
                parts: max(1, min(12, Int(fileSize / (4 * 1024 * 1024)))),
                approxBytes: fileSize,
                expectedSHA256: f.sha256
            ) { tick in
                Task {
                    let inFlight = tick.written
                    let total = await tracker.total()
                    let absDone = fileStartTotalDone + inFlight
                    let frac = total > 0 ? min(1.0, Double(absDone) / Double(total)) : 0
                    progress(frac, absDone, total)
                }
            }
            await tracker.commitFile(bytes: fileSize)
            let absDone = await tracker.totalDone()
            let total = await tracker.total()
            let frac = total > 0 ? min(1.0, Double(absDone) / Double(total)) : 1.0
            progress(frac, absDone, total)
        }

        Self.writeVerifiedSentinel(verifiedSentinel)
    }

    private static func writeVerifiedSentinel(_ sentinel: URL) {
        try? Data().write(to: sentinel)
    }

    // MARK: - HF tree listing

    private func listRepoFiles(repo: String, revision: String) async throws -> [VLMRepoFile] {
        guard let url = URL(string: "https://huggingface.co/api/models/\(repo)/tree/\(revision)") else {
            throw VLMDownloaderError.treeListFailed(status: 0)
        }
        var req = URLRequest(url: url)
        req.setValue("identity", forHTTPHeaderField: "Accept-Encoding")
        let pinDelegate = TLSPinningSessionDelegate()
        let session = URLSession(configuration: .ephemeral,
                                 delegate: pinDelegate, delegateQueue: nil)
        defer { session.finishTasksAndInvalidate() }
        let data: Data
        let resp: URLResponse
        do {
            (data, resp) = try await session.data(for: req)
        } catch {
            if pinDelegate.pinningRejected {
                throw StreamingDownloadError.pinningFailed
            }
            throw error
        }
        guard let http = resp as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw VLMDownloaderError.treeListFailed(status: (resp as? HTTPURLResponse)?.statusCode ?? 0)
        }
        let decoder = JSONDecoder()
        guard let raw = try? decoder.decode([HFTreeEntry].self, from: data) else {
            throw VLMDownloaderError.treeDecodeFailed
        }
        return raw
            .filter { $0.type == "file" }
            // `lfs.oid` IS the file's sha256 for LFS-tracked files; the
            // top-level `oid` is a git blob sha1 — never use it here.
            .map { VLMRepoFile(path: $0.path, size: $0.size ?? $0.lfs?.size ?? 0,
                               sha256: $0.lfs?.oid) }
    }

    private struct HFTreeEntry: Decodable {
        struct LFS: Decodable {
            let size: Int64?
            let oid: String?
        }
        let type: String
        let path: String
        let size: Int64?
        let lfs: LFS?
    }

    // MARK: - File ops

    private nonisolated func fileSize(_ url: URL) -> Int64? {
        guard let attrs = try? FileManager.default.attributesOfItem(atPath: url.path),
              let n = attrs[.size] as? Int64 else { return nil }
        return n
    }

    private static func shouldDownload(_ f: VLMRepoFile) -> Bool {
        // Skip docs and noise; everything else (configs, tokenizers,
        // safetensors, vocab) is needed by the loader.
        let p = f.path.lowercased()
        if p == "readme.md" || p == ".gitattributes" { return false }
        if p.hasSuffix(".png") || p.hasSuffix(".jpg") { return false }
        return true
    }
}

private actor AggregateTracker {
    private var baseDone: Int64 = 0
    private var totalBytes: Int64 = 0

    func setBaseDone(_ n: Int64) { baseDone = n }
    func setTotal(_ n: Int64) { totalBytes = n }
    func total() -> Int64 { totalBytes }

    /// Total bytes fully completed across all files so far.
    func totalDone() -> Int64 { baseDone }

    /// Mark a single file as done — promote its bytes from in-flight
    /// to baseDone so subsequent in-flight readings build on a clean
    /// foundation.
    func commitFile(bytes: Int64) {
        baseDone += bytes
    }
}
