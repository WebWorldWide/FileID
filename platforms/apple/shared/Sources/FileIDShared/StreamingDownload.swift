// URLSessionDownloadTask wrappers with throttled progress and EMA
// bandwidth/ETA. Single-stream `streamingDownload` for small files;
// `parallelStreamingDownload` for big files served by hosts that
// per-connection-throttle (HF/Cloudflare). The multi-part path:
//   1. HEADs the canonical URL, captures the redirected CDN URL.
//   2. Requires explicit Accept-Ranges: bytes + Content-Length.
//   3. Issues N range GETs against the resolved CDN URL, each
//      enforced to return 206 (else falls back to single-stream).
//   4. Streams part files into the destination, fsyncs, atomic-renames.
//
// Nonisolated by design — runs off the main thread. SwiftUI callers
// hop to MainActor inside their own onTick if they need it.
import Foundation

public struct DownloadTick: Sendable, Equatable {
    public let written: Int64
    public let total: Int64
    public let bytesPerSecond: Double
    public let etaSeconds: Double

    public init(written: Int64, total: Int64,
                bytesPerSecond: Double, etaSeconds: Double) {
        self.written = written
        self.total = total
        self.bytesPerSecond = bytesPerSecond
        self.etaSeconds = etaSeconds
    }
}

public enum StreamingDownloadError: Error {
    case http(status: Int)
    case missingTempFile
    case rangeNotSupported
    case underlying(Error)
}

public func streamingDownload(
    remote: URL,
    dest: URL,
    onTick: @escaping @Sendable (DownloadTick) -> Void
) async throws {
    let delegate = StreamingDownloadDelegate()
    let session = URLSession(configuration: streamingConfig(),
                              delegate: delegate, delegateQueue: nil)
    var didSucceed = false
    defer {
        if didSucceed { session.finishTasksAndInvalidate() }
        else          { session.invalidateAndCancel() }
    }

    delegate.onTick = { tick in onTick(tick) }

    try await withTaskCancellationHandler {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            delegate.completion = { result in
                switch result {
                case .success(let tempURL):
                    do {
                        try moveAtomically(from: tempURL, to: dest, fsync: true)
                        onTick(DownloadTick(
                            written: delegate.lastWritten,
                            total:   delegate.lastWritten,
                            bytesPerSecond: delegate.smoothedBytesPerSec,
                            etaSeconds: 0
                        ))
                        cont.resume()
                    } catch {
                        try? FileManager.default.removeItem(at: tempURL)
                        cont.resume(throwing: StreamingDownloadError.underlying(error))
                    }
                case .failure(let err):
                    cont.resume(throwing: err)
                }
            }
            let task = session.downloadTask(with: remote)
            task.priority = URLSessionTask.highPriority
            delegate.task = task
            if Task.isCancelled {
                // Clear completion FIRST so the delegate's didCompleteWithError
                // (fired by task.cancel()) can't resume this continuation a
                // second time — a fatal double-resume.
                delegate.completion = nil
                task.cancel()
                cont.resume(throwing: CancellationError())
                return
            }
            task.resume()
        }
        didSucceed = true
    } onCancel: {
        delegate.task?.cancel()
    }
}

/// Multi-part range download. HEADs to confirm `Content-Length` +
/// `Accept-Ranges: bytes`, captures the redirected CDN URL, splits
/// into `parts` chunks downloaded concurrently, streams them into
/// the final file, fsyncs, and atomic-renames into place. Falls
/// back to `streamingDownload` on hosts without range support or
/// when any part returns 200 instead of 206.
public func parallelStreamingDownload(
    remote: URL,
    dest: URL,
    parts: Int = 12,
    approxBytes: Int64 = 0,
    onTick: @escaping @Sendable (DownloadTick) -> Void
) async throws {
    let probeCapture = RedirectCapture()
    let probeSession = URLSession(configuration: streamingConfig(approxBytes: approxBytes),
                                  delegate: probeCapture, delegateQueue: nil)
    var headReq = URLRequest(url: remote)
    headReq.httpMethod = "HEAD"
    headReq.setValue("identity", forHTTPHeaderField: "Accept-Encoding")
    let headResp: URLResponse
    do {
        (_, headResp) = try await probeSession.data(for: headReq)
    } catch {
        probeSession.invalidateAndCancel()
        throw StreamingDownloadError.underlying(error)
    }
    probeSession.finishTasksAndInvalidate()

    guard let http = headResp as? HTTPURLResponse,
          (200..<300).contains(http.statusCode) else {
        let code = (headResp as? HTTPURLResponse)?.statusCode ?? 0
        throw StreamingDownloadError.http(status: code)
    }

    let resolvedURL = http.url ?? probeCapture.finalURL ?? remote
    let total = Int64(http.value(forHTTPHeaderField: "Content-Length") ?? "") ?? 0
    let acceptsRanges = (http.value(forHTTPHeaderField: "Accept-Ranges") ?? "")
        .lowercased() == "bytes"

    if total <= 0 || !acceptsRanges {
        try await streamingDownload(remote: remote, dest: dest, onTick: onTick)
        return
    }

    let chunkCount = max(1, parts)
    let chunkSize = total / Int64(chunkCount)
    var ranges: [(start: Int64, end: Int64)] = []
    for i in 0..<chunkCount {
        let start = Int64(i) * chunkSize
        let end = (i == chunkCount - 1) ? total - 1 : start + chunkSize - 1
        ranges.append((start, end))
    }

    let tracker = MultiPartProgressTracker(partCount: chunkCount, totalBytes: total)
    let runID = UUID().uuidString
    // Stage parts in the destination's parent dir so the final concat
    // file ends up on the same volume as `dest` — the final move is
    // then an intra-volume rename instead of a multi-GB cross-volume
    // copy from /tmp.
    let stagingDir = dest.deletingLastPathComponent()
        .appendingPathComponent(".fileid-staging", isDirectory: true)
    try? FileManager.default.createDirectory(at: stagingDir, withIntermediateDirectories: true)
    let partURLs: [URL] = (0..<chunkCount).map { i in
        stagingDir.appendingPathComponent("\(runID)-part-\(i)")
    }

    func cleanupParts() {
        for u in partURLs { try? FileManager.default.removeItem(at: u) }
    }

    do {
        try await withThrowingTaskGroup(of: Void.self) { group in
            for i in 0..<chunkCount {
                let range = ranges[i]
                let partURL = partURLs[i]
                group.addTask {
                    try await downloadRange(
                        remote: resolvedURL, partURL: partURL,
                        start: range.start, end: range.end,
                        partIndex: i, tracker: tracker,
                        approxBytes: approxBytes, onTick: onTick
                    )
                }
            }
            try await group.waitForAll()
        }
    } catch StreamingDownloadError.rangeNotSupported {
        cleanupParts()
        // Fall back against the CANONICAL remote, not the resolved CDN URL —
        // a resolved per-redirect URL can be short-lived/expired, whereas the
        // sibling single-stream fallback (above) correctly re-follows `remote`.
        try await streamingDownload(remote: remote, dest: dest, onTick: onTick)
        return
    } catch {
        cleanupParts()
        throw error
    }

    let finalTemp = stagingDir.appendingPathComponent("\(runID)-final")
    FileManager.default.createFile(atPath: finalTemp.path, contents: nil)
    guard let writer = try? FileHandle(forWritingTo: finalTemp) else {
        cleanupParts()
        throw StreamingDownloadError.underlying(NSError(domain: "FileID", code: -1))
    }
    do {
        for u in partURLs {
            let reader = try FileHandle(forReadingFrom: u)
            while true {
                // Propagate read errors instead of swallowing them as EOF —
                // a genuine I/O error used to silently truncate the assembled
                // file (and then ship a corrupt model with no error).
                let chunk = try autoreleasepool { try reader.read(upToCount: 4 * 1024 * 1024) } ?? Data()
                if chunk.isEmpty { break }
                try writer.write(contentsOf: chunk)
            }
            try? reader.close()
        }
        try writer.synchronize()
        try writer.close()
    } catch {
        try? writer.close()
        try? FileManager.default.removeItem(at: finalTemp)
        cleanupParts()
        throw StreamingDownloadError.underlying(error)
    }
    // Verify the assembled file is exactly the advertised size before trusting
    // it — guards against a short/truncated concat shipping a corrupt model.
    let assembledSize = ((try? FileManager.default.attributesOfItem(atPath: finalTemp.path))?[.size] as? NSNumber)?.int64Value ?? -1
    guard assembledSize == total else {
        try? FileManager.default.removeItem(at: finalTemp)
        cleanupParts()
        throw StreamingDownloadError.underlying(NSError(
            domain: "FileID", code: -2,
            userInfo: [NSLocalizedDescriptionKey: "Download size mismatch: assembled \(assembledSize) bytes, expected \(total)"]))
    }
    cleanupParts()

    do {
        try moveAtomically(from: finalTemp, to: dest, fsync: false)
    } catch {
        try? FileManager.default.removeItem(at: finalTemp)
        throw StreamingDownloadError.underlying(error)
    }

    onTick(DownloadTick(written: total, total: total,
                         bytesPerSecond: tracker.combinedBytesPerSec(),
                         etaSeconds: 0))
}

private func downloadRange(
    remote: URL, partURL: URL,
    start: Int64, end: Int64,
    partIndex: Int,
    tracker: MultiPartProgressTracker,
    approxBytes: Int64,
    onTick: @escaping @Sendable (DownloadTick) -> Void
) async throws {
    let delegate = StreamingDownloadDelegate()
    delegate.requireRangedResponse = true
    let session = URLSession(configuration: streamingConfig(approxBytes: approxBytes),
                              delegate: delegate, delegateQueue: nil)
    var didSucceed = false
    defer {
        if didSucceed { session.finishTasksAndInvalidate() }
        else          { session.invalidateAndCancel() }
    }

    delegate.onTick = { tick in
        tracker.update(part: partIndex,
                       written: tick.written,
                       bytesPerSec: tick.bytesPerSecond)
        let snap = tracker.snapshot()
        onTick(DownloadTick(
            written: snap.written, total: snap.total,
            bytesPerSecond: snap.bytesPerSec, etaSeconds: snap.eta
        ))
    }

    var req = URLRequest(url: remote)
    req.setValue("bytes=\(start)-\(end)", forHTTPHeaderField: "Range")
    req.setValue("identity", forHTTPHeaderField: "Accept-Encoding")

    try await withTaskCancellationHandler {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            delegate.completion = { result in
                switch result {
                case .success(let tempURL):
                    do {
                        try? FileManager.default.removeItem(at: partURL)
                        try FileManager.default.moveItem(at: tempURL, to: partURL)
                        tracker.markComplete(part: partIndex)
                        cont.resume()
                    } catch {
                        try? FileManager.default.removeItem(at: tempURL)
                        cont.resume(throwing: StreamingDownloadError.underlying(error))
                    }
                case .failure(let err):
                    cont.resume(throwing: err)
                }
            }
            let task = session.downloadTask(with: req)
            task.priority = URLSessionTask.highPriority
            delegate.task = task
            if Task.isCancelled {
                // Clear completion FIRST so the delegate's didCompleteWithError
                // (fired by task.cancel()) can't resume this continuation a
                // second time — a fatal double-resume.
                delegate.completion = nil
                task.cancel()
                cont.resume(throwing: CancellationError())
                return
            }
            task.resume()
        }
        didSucceed = true
    } onCancel: {
        delegate.task?.cancel()
    }
}

/// Move src → dst atomically when on the same volume; replace the
/// destination if it exists. Optionally fsyncs the src before move
/// (single-stream path) so a kernel panic between move and journal
/// commit can't leave a zero-byte stub.
private func moveAtomically(from src: URL, to dst: URL, fsync: Bool) throws {
    try FileManager.default.createDirectory(
        at: dst.deletingLastPathComponent(),
        withIntermediateDirectories: true
    )
    if fsync, let h = try? FileHandle(forWritingTo: src) {
        try? h.synchronize()
        try? h.close()
    }
    if FileManager.default.fileExists(atPath: dst.path) {
        var isDir: ObjCBool = false
        FileManager.default.fileExists(atPath: dst.path, isDirectory: &isDir)
        if isDir.boolValue {
            try FileManager.default.removeItem(at: dst)
            try FileManager.default.moveItem(at: src, to: dst)
        } else {
            _ = try FileManager.default.replaceItemAt(dst, withItemAt: src)
        }
    } else {
        try FileManager.default.moveItem(at: src, to: dst)
    }
}

private func streamingConfig(approxBytes: Int64 = 0) -> URLSessionConfiguration {
    let config = URLSessionConfiguration.ephemeral
    config.waitsForConnectivity = true
    // 180 s per-request: a single TCP buffer flush during Cloudflare
    // backpressure can stall well over 60 s on flaky uplinks.
    config.timeoutIntervalForRequest = 180
    // Allow 100 KB/s minimum effective speed; never less than 30 min.
    let computed = max(1800, TimeInterval(approxBytes) / (100 * 1024))
    config.timeoutIntervalForResource = computed
    config.httpAdditionalHeaders = [
        // Force identity encoding — gzipped content over a Range request
        // is undefined territory; the Content-Length header refers to
        // post-gzip bytes and the math falls apart.
        "Accept-Encoding": "identity"
    ]
    // Each parallelStreamingDownload creates one ephemeral session per
    // part. Within a session we want the URLSession pool to keep TCP
    // connections alive across retries/redirects — DON'T set
    // Connection: close (it forces a fresh TCP+TLS handshake every
    // request, defeating the pool entirely).
    config.httpMaximumConnectionsPerHost = 16
    return config
}

private final class RedirectCapture: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    var finalURL: URL?
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        self.finalURL = request.url
        completionHandler(request)
    }
}

private final class MultiPartProgressTracker: @unchecked Sendable {
    struct Snapshot { let written: Int64; let total: Int64; let bytesPerSec: Double; let eta: Double }
    private let lock = NSLock()
    private var partWritten: [Int64]
    private var partDone: [Bool]
    let total: Int64

    /// Aggregate bandwidth measured from byte delta over time at the
    /// snapshot level. Per-part EMAs summed over still-active parts
    /// drop visibly toward zero as parts complete sequentially —
    /// that's mathematically right but reads as "downloads got slow"
    /// to the user. The aggregate is what matters.
    private var smoothedBytesPerSec: Double = 0
    private var lastSampleAt: TimeInterval = 0
    private var lastSampleWritten: Int64 = 0

    init(partCount: Int, totalBytes: Int64) {
        self.partWritten = Array(repeating: 0, count: partCount)
        self.partDone = Array(repeating: false, count: partCount)
        self.total = totalBytes
    }

    func update(part: Int, written: Int64, bytesPerSec: Double) {
        lock.lock(); defer { lock.unlock() }
        guard partWritten.indices.contains(part) else { return }
        partWritten[part] = written
    }

    func markComplete(part: Int) {
        lock.lock(); defer { lock.unlock() }
        guard partDone.indices.contains(part) else { return }
        partDone[part] = true
    }

    func combinedBytesPerSec() -> Double {
        lock.lock(); defer { lock.unlock() }
        return smoothedBytesPerSec
    }

    func snapshot() -> Snapshot {
        lock.lock(); defer { lock.unlock() }
        let written = partWritten.reduce(0, +)

        let now = Date().timeIntervalSinceReferenceDate
        if lastSampleAt == 0 {
            lastSampleAt = now
            lastSampleWritten = written
        } else {
            let dt = now - lastSampleAt
            // Resample at most every 500ms — TCP slow-start otherwise
            // dominates the first second of the displayed rate.
            if dt >= 0.5 {
                let delta = Double(written - lastSampleWritten)
                let instant = max(0, delta) / dt
                smoothedBytesPerSec = smoothedBytesPerSec == 0
                    ? instant
                    : 0.7 * smoothedBytesPerSec + 0.3 * instant
                lastSampleAt = now
                lastSampleWritten = written
            }
        }

        let bps = smoothedBytesPerSec
        let remaining = max(0, total - written)
        let eta = bps > 0 ? Double(remaining) / bps : 0
        return Snapshot(written: written, total: total, bytesPerSec: bps, eta: eta)
    }
}

private final class StreamingDownloadDelegate: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
    var task: URLSessionDownloadTask?
    var onTick: ((DownloadTick) -> Void)?
    var completion: ((Result<URL, Error>) -> Void)?
    /// When true, only HTTP 206 is accepted on completion. Anything
    /// else (including a 200 with full body) is reported as
    /// `rangeNotSupported` so the caller can fall back to single-stream.
    var requireRangedResponse: Bool = false

    private let didFinish = NSLock()
    private var didFinishFlag = false
    private func tryFinish() -> Bool {
        didFinish.lock(); defer { didFinish.unlock() }
        if didFinishFlag { return false }
        didFinishFlag = true
        return true
    }

    private let startedAt = Date()
    private var lastTickAt: TimeInterval = 0
    private var lastWriteSampleAt: TimeInterval = 0
    private var lastWriteSampleBytes: Int64 = 0
    private(set) var lastWritten: Int64 = 0
    private(set) var smoothedBytesPerSec: Double = 0
    private var sampleCount: Int = 0

    private static let tickIntervalSec: TimeInterval = 0.1
    private static let warmupSec: TimeInterval = 0.5

    func urlSession(_ session: URLSession,
                    downloadTask: URLSessionDownloadTask,
                    didWriteData bytesWritten: Int64,
                    totalBytesWritten: Int64,
                    totalBytesExpectedToWrite: Int64) {
        let now = Date().timeIntervalSinceReferenceDate
        lastWritten = totalBytesWritten

        if lastWriteSampleAt == 0 {
            lastWriteSampleAt = now
            lastWriteSampleBytes = totalBytesWritten
        }

        guard sampleCount == 0 || (now - lastTickAt) >= Self.tickIntervalSec else { return }
        defer { lastTickAt = now; sampleCount += 1 }

        let elapsed = now - startedAt.timeIntervalSinceReferenceDate
        if elapsed >= Self.warmupSec {
            let dt = now - lastWriteSampleAt
            if dt > 0 {
                let dBytes = totalBytesWritten - lastWriteSampleBytes
                let instant = Double(dBytes) / dt
                smoothedBytesPerSec = smoothedBytesPerSec == 0
                    ? instant
                    : 0.7 * smoothedBytesPerSec + 0.3 * instant
            }
            lastWriteSampleAt = now
            lastWriteSampleBytes = totalBytesWritten
        }

        let total = max(0, totalBytesExpectedToWrite)
        var eta: Double = 0
        if total > 0, smoothedBytesPerSec > 0 {
            eta = Double(max(0, total - totalBytesWritten)) / smoothedBytesPerSec
        }
        onTick?(DownloadTick(
            written: totalBytesWritten, total: total,
            bytesPerSecond: smoothedBytesPerSec, etaSeconds: eta
        ))
    }

    func urlSession(_ session: URLSession,
                    downloadTask: URLSessionDownloadTask,
                    didFinishDownloadingTo location: URL) {
        guard tryFinish() else {
            try? FileManager.default.removeItem(at: location)
            return
        }
        if let http = downloadTask.response as? HTTPURLResponse {
            let status = http.statusCode
            if !(200..<300).contains(status) {
                try? FileManager.default.removeItem(at: location)
                completion?(.failure(StreamingDownloadError.http(status: status)))
                completion = nil
                return
            }
            if requireRangedResponse, status != 206 {
                try? FileManager.default.removeItem(at: location)
                completion?(.failure(StreamingDownloadError.rangeNotSupported))
                completion = nil
                return
            }
        }

        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-dl-\(UUID().uuidString)")
        do {
            try FileManager.default.moveItem(at: location, to: tmp)
            completion?(.success(tmp))
        } catch {
            completion?(.failure(StreamingDownloadError.underlying(error)))
        }
        completion = nil
    }

    func urlSession(_ session: URLSession,
                    task: URLSessionTask,
                    didCompleteWithError error: Error?) {
        guard tryFinish() else { return }
        guard let completion else { return }
        if let error {
            if let urlError = error as? URLError, urlError.code == .cancelled {
                completion(.failure(CancellationError()))
            } else {
                completion(.failure(StreamingDownloadError.underlying(error)))
            }
        } else {
            completion(.failure(StreamingDownloadError.missingTempFile))
        }
        self.completion = nil
    }
}

extension StreamingDownloadError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .http(let status):
            switch status {
            case 401, 403: return "Server refused the download (HTTP \(status)). The repo may be gated."
            case 404:      return "File not found on HuggingFace (HTTP 404)."
            case 429:      return "HuggingFace rate-limited the download (HTTP 429). Try again in a minute."
            case 500..<600: return "HuggingFace returned a server error (HTTP \(status)). Try again."
            default:       return "Download failed (HTTP \(status))."
            }
        case .missingTempFile:
            return "Downloaded data couldn't be saved. The destination disk may be full."
        case .rangeNotSupported:
            return "Server didn't honor the range request. Falling back to single-stream."
        case .underlying(let err):
            if let urlError = err as? URLError {
                switch urlError.code {
                case .notConnectedToInternet:    return "No internet connection."
                case .timedOut:                  return "Download timed out — try again on a stronger connection."
                case .cannotFindHost:            return "Couldn't reach huggingface.co (DNS lookup failed)."
                case .cannotConnectToHost:       return "Couldn't connect to huggingface.co. Try again."
                case .networkConnectionLost:     return "Network connection dropped mid-download."
                case .secureConnectionFailed:    return "Secure connection to HuggingFace failed."
                case .dataNotAllowed:            return "Cellular data restrictions are blocking the download."
                default:                         return "Network error: \(urlError.localizedDescription)"
                }
            }
            return err.localizedDescription
        }
    }
}

public enum DownloadFormat {
    public static func bandwidth(_ bytesPerSec: Double) -> String {
        guard bytesPerSec > 0 else { return "" }
        let mbps = bytesPerSec / 1_048_576.0
        if mbps >= 1.0 {
            return String(format: "%.1f MB/s", mbps)
        }
        let kbps = bytesPerSec / 1024.0
        if kbps < 1.0 {
            return "<1 KB/s"
        }
        return String(format: "%.0f KB/s", kbps)
    }

    public static func eta(_ seconds: Double) -> String {
        guard seconds > 0, seconds.isFinite else { return "" }
        let s = Int(seconds.rounded())
        if s < 60 { return "\(s)s remaining" }
        if s < 3600 {
            let m = s / 60
            let r = s % 60
            return r > 0 ? "\(m)m \(r)s remaining" : "\(m)m remaining"
        }
        let h = s / 3600
        return "~\(h)h remaining"
    }

    public static func rateAndETA(_ tick: DownloadTick) -> String {
        let bw = bandwidth(tick.bytesPerSecond)
        let eta = eta(tick.etaSeconds)
        switch (bw.isEmpty, eta.isEmpty) {
        case (true, true):  return "calculating…"
        case (false, true): return bw
        case (true, false): return eta
        case (false, false): return "\(bw) · \(eta)"
        }
    }
}
