import Foundation

/// The log files either engine leaves under `<state>/logs`: the Swift engine writes `scan.jsonl` (and `.1`),
/// the Rust engine `engine.jsonl.<date>`; both write `engine-stderr.log`, and the app writes `app.log`.
enum EngineLogFiles {
    nonisolated static func isEngineLog(_ name: String) -> Bool {
        name == "scan.jsonl" || name == "scan.jsonl.1" || name == "engine.jsonl" || name.hasPrefix("engine.jsonl.")
    }

    nonisolated static func isStderrLog(_ name: String) -> Bool {
        name == "engine-stderr.log" || name == "engine-stderr.log.1"
    }

    /// Names "Start over" removes: every engine log, the stderr capture and the app log.
    nonisolated static func removableNames(in logs: URL) -> [String] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: logs.path)) ?? []
        return names.filter { isEngineLog($0) || isStderrLog($0) || $0 == "app.log" }
    }

    /// The newest structured engine log, else the newest stderr capture, else nil.
    nonisolated static func newestLog(in logs: URL) -> URL? {
        let fm = FileManager.default
        let names = (try? fm.contentsOfDirectory(atPath: logs.path)) ?? []
        func newest(_ matching: (String) -> Bool) -> URL? {
            names.filter(matching).compactMap { name -> (URL, Date)? in
                let url = logs.appendingPathComponent(name)
                guard let date = (try? fm.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date else { return nil }
                return (url, date)
            }.max { $0.1 < $1.1 }?.0
        }
        return newest(isEngineLog) ?? newest(isStderrLog)
    }

    /// What "Show scan log in Finder" selects: the newest log, else the logs folder, else the state folder.
    nonisolated static func revealTarget(state: URL) -> URL {
        let logs = state.appendingPathComponent("logs", isDirectory: true)
        if let log = newestLog(in: logs) { return log }
        return FileManager.default.fileExists(atPath: logs.path) ? logs : state
    }
}
