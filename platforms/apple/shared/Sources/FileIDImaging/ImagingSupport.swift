import Foundation

/// Log lines from the shared imaging code. The Swift engine routes them into its
/// JSONL log at launch (`ImagingLog.route`); without a route they go to stderr
/// as JSON lines, which under the Rust host is the engine-stderr log.
package enum ImagingLog {
    package typealias Route = @Sendable (_ level: String, _ event: String, _ error: String?, _ metrics: [String: Double]) -> Void

    private static let lock = NSLock()
    nonisolated(unsafe) private static var route: Route?

    package static func route(to handler: @escaping Route) {
        lock.lock(); defer { lock.unlock() }
        route = handler
    }

    package static func info(_ event: String, metrics: [String: Double] = [:]) {
        emit("info", event, error: nil, metrics: metrics)
    }

    package static func warn(_ event: String, error: String? = nil) {
        emit("warn", event, error: error, metrics: [:])
    }

    private static func emit(_ level: String, _ event: String, error: String?, metrics: [String: Double]) {
        lock.lock()
        let handler = route
        lock.unlock()
        if let handler { return handler(level, event, error, metrics) }
        var entry: [String: Any] = ["lvl": level, "ev": event, "src": "FileIDImaging"]
        entry["error"] = error
        for (key, value) in metrics { entry[key] = value }
        guard var line = try? JSONSerialization.data(withJSONObject: entry, options: [.sortedKeys]) else { return }
        line.append(0x0A)
        lock.lock(); defer { lock.unlock() }
        FileHandle.standardError.write(line)
    }
}

/// A value handed back from work that runs on another thread.
package final class ScanSideResult<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: T?
    package init() {}
    package func set(_ v: T) { lock.lock(); value = v; lock.unlock() }
    package func get() -> T? { lock.lock(); defer { lock.unlock() }; return value }
}
