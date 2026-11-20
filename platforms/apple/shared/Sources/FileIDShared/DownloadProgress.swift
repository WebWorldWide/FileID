// Progress and hashing helpers shared by the model installers: the tick the engine's download events become, how a rate and ETA read, and a streaming file digest.
import CryptoKit
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

/// Streaming SHA256 of a file on disk — 4 MB autoreleasepool chunks so
/// multi-GB models never load whole into RAM. Hex, lowercase.
public func sha256HexOfFile(at url: URL) throws -> String {
    let handle = try FileHandle(forReadingFrom: url)
    defer { try? handle.close() }
    var hasher = SHA256()
    while true {
        let chunk = try autoreleasepool { try handle.read(upToCount: 4 * 1024 * 1024) } ?? Data()
        if chunk.isEmpty { break }
        hasher.update(data: chunk)
    }
    return hasher.finalize().map { String(format: "%02x", $0) }.joined()
}

/// Move src → dst atomically when on the same volume; replace the

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
