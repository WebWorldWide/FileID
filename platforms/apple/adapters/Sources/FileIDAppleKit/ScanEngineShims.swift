// Stand-ins for the few Swift-engine types that the sources in ScanShared/
// (symlinks to platforms/apple/engine) refer to. Each mirrors only the members
// those files use, so the shared decode, Vision and MLX code compiles here
// unchanged. A new engine dependency in a shared file fails this build, which
// is the point: the two engines must keep running the same code.
import Foundation

/// The engine's JSONL logger. Under the Rust host fd 2 is the engine-stderr
/// log, so adapter log lines land there.
final class JSONLog: @unchecked Sendable {
    static let shared = JSONLog()
    private let lock = NSLock()

    func info(ev: String, sess: String? = nil, path: String? = nil, extra: [String: AnyCodable]? = nil) {
        write("info", ev, path: path, error: nil)
    }

    func warn(ev: String, sess: String? = nil, path: String? = nil, error: String? = nil) {
        write("warn", ev, path: path, error: error)
    }

    func error(ev: String, sess: String? = nil, path: String? = nil, error: String? = nil) {
        write("error", ev, path: path, error: error)
    }

    private func write(_ level: String, _ ev: String, path: String?, error: String?) {
        var entry: [String: String] = ["lvl": level, "ev": ev, "src": "FileIDAppleKit"]
        entry["path"] = path
        entry["error"] = error
        guard var line = try? JSONSerialization.data(withJSONObject: entry, options: [.sortedKeys]) else { return }
        line.append(0x0A)
        lock.lock()
        defer { lock.unlock() }
        FileHandle.standardError.write(line)
    }
}

struct AnyCodable: Sendable {
    init<T: Sendable>(_ value: T) {}
}

/// `AccurateOCRWarmup.startWhenIdle` polls the engine's job queue; the Rust
/// engine decides when it is idle and calls `AccurateOCRWarmup.start()` itself.
actor JobQueue {
    enum Category: Sendable { case scan }
    static let shared = JobQueue()
    func hasActive(category: Category) -> Bool { false }
}

enum Tagging {
    final class ScanSideResult<T>: @unchecked Sendable {
        private let lock = NSLock()
        private var value: T?
        func set(_ v: T) { lock.lock(); value = v; lock.unlock() }
        func get() -> T? { lock.lock(); defer { lock.unlock() }; return value }
    }
}

enum RamPlusService {
    static let peopleWords = ScanRamPlus.peopleWords
}
