// Structured JSONL logger for the engine.
//
// Writes one JSON object per line to ~/Library/Application Support/FileID/logs/scan.jsonl.
// Replaces the v1 freeform `scan.log`. Designed for `jq` queries — every event
// has the same canonical shape (timestamp + session + level + ev + payload).
//
// Concurrency: nonisolated; takes an NSLock around the write so multiple actors
// can call without races. Single fsync per N lines (default 1) — caller can
// flush explicitly at phase boundaries.
import Foundation
import FileIDShared

public final class JSONLog: @unchecked Sendable {
    public static let shared = JSONLog()

    private let lock = NSLock()
    private var handle: FileHandle?
    private let url: URL
    private let encoder: JSONEncoder

    public init() {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first!
            .appendingPathComponent("FileID/logs", isDirectory: true)
        try? FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        self.url = base.appendingPathComponent("scan.jsonl")

        let e = JSONEncoder()
        e.dateEncodingStrategy = .iso8601
        e.outputFormatting = []
        self.encoder = e

        // Rotate the log if it's >32 MB. Per-batch + per-file events average
        // ~250 bytes/line; a 60K-file scan emits ~10 MB. 32 MB cap means we
        // keep the most recent 1-2 scans before rotation (plenty of forensic
        // history without unbounded disk growth).
        if let size = try? FileManager.default
                .attributesOfItem(atPath: url.path)[.size] as? UInt64,
           size > 32 * 1024 * 1024 {
            let rotated = base.appendingPathComponent("scan.jsonl.1")
            try? FileManager.default.removeItem(at: rotated)
            try? FileManager.default.moveItem(at: url, to: rotated)
        }

        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil)
        }
        self.handle = try? FileHandle(forWritingTo: url)
        try? self.handle?.seekToEnd()
    }

    public struct Entry: Encodable {
        public let t: Date
        public let sess: String?
        public let lvl: String
        public let ev: String
        public let path: String?
        public let kind: String?
        public let ms: Double?
        public let error: String?
        public let extra: [String: AnyCodable]?
    }

    public func info(ev: String, sess: String? = nil, path: String? = nil, extra: [String: AnyCodable]? = nil) {
        write(level: "info", ev: ev, sess: sess, path: path, extra: extra)
    }
    public func warn(ev: String, sess: String? = nil, path: String? = nil, error: String? = nil) {
        write(level: "warn", ev: ev, sess: sess, path: path, error: error)
    }
    public func error(ev: String, sess: String? = nil, path: String? = nil, error: String? = nil) {
        write(level: "error", ev: ev, sess: sess, path: path, error: error)
    }

    private func write(level: String, ev: String, sess: String?, path: String?, error: String? = nil, extra: [String: AnyCodable]? = nil) {
        let entry = Entry(
            t: Date(), sess: sess, lvl: level, ev: ev,
            path: path, kind: nil, ms: nil, error: error, extra: extra
        )
        guard let data = try? encoder.encode(entry) else { return }
        var line = data
        line.append(0x0A)
        lock.lock()
        defer { lock.unlock() }
        do {
            try handle?.write(contentsOf: line)
        } catch {
            // Disk full / handle invalid — fall through to NSLog so the user sees
            // it in Console.app. Don't crash the engine over a log write failure.
            NSLog("FileIDEngine JSONLog write failed: %@", error.localizedDescription)
        }
    }

    /// Flush the file handle. Call at phase boundaries (discovery end, scan end)
    /// so a crash doesn't cost us the most recent few lines.
    public func flush() {
        lock.lock()
        defer { lock.unlock() }
        try? handle?.synchronize()
    }
}

// Tiny type-erased Codable wrapper so we can stuff heterogeneous extras in
// log lines without inventing a strict schema for every event.
public struct AnyCodable: Encodable, Sendable {
    private let _encode: @Sendable (Encoder) throws -> Void

    public init<T: Encodable & Sendable>(_ value: T) {
        self._encode = { encoder in try value.encode(to: encoder) }
    }

    public func encode(to encoder: Encoder) throws { try _encode(encoder) }
}
