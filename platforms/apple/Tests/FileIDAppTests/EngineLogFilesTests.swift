import Foundation
import Testing
@testable import FileID

@Suite("Engine log files", .serialized)
struct EngineLogFilesTests {
    private func state(_ files: [String: Double]) throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-logs-\(UUID().uuidString)", directoryHint: .isDirectory)
        let logs = root.appending(component: "logs", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        for (name, age) in files {
            let url = logs.appending(component: name)
            try Data("x".utf8).write(to: url)
            try FileManager.default.setAttributes([.modificationDate: Date(timeIntervalSinceNow: -age)], ofItemAtPath: url.path)
        }
        return root
    }

    @Test("Swift layout: scan.jsonl is revealed and Start over removes every log")
    func swiftLayout() throws {
        let root = try state(["scan.jsonl": 10, "scan.jsonl.1": 100, "engine-stderr.log": 5, "app.log": 1])
        defer { try? FileManager.default.removeItem(at: root) }
        #expect(EngineLogFiles.revealTarget(state: root).lastPathComponent == "scan.jsonl")
        let logs = root.appending(component: "logs")
        #expect(Set(EngineLogFiles.removableNames(in: logs)) == ["scan.jsonl", "scan.jsonl.1", "engine-stderr.log", "app.log"])
    }

    @Test("Rust layout: the newest dated engine log is revealed and Start over removes the dated logs")
    func rustLayout() throws {
        let root = try state(["engine.jsonl.2026-11-13": 200, "engine.jsonl.2026-11-15": 10,
                              "engine-stderr.log": 1, "engine-stderr.log.1": 500, "app.log": 1])
        defer { try? FileManager.default.removeItem(at: root) }
        #expect(EngineLogFiles.revealTarget(state: root).lastPathComponent == "engine.jsonl.2026-11-15")
        let logs = root.appending(component: "logs")
        #expect(Set(EngineLogFiles.removableNames(in: logs)) == [
            "engine.jsonl.2026-11-13", "engine.jsonl.2026-11-15", "engine-stderr.log", "engine-stderr.log.1", "app.log"])
    }

    @Test("only a stderr capture: it is revealed; nothing at all: the logs folder, then the state folder")
    func fallbacks() throws {
        let onlyStderr = try state(["engine-stderr.log": 1])
        defer { try? FileManager.default.removeItem(at: onlyStderr) }
        #expect(EngineLogFiles.revealTarget(state: onlyStderr).lastPathComponent == "engine-stderr.log")
        let empty = try state([:])
        defer { try? FileManager.default.removeItem(at: empty) }
        #expect(EngineLogFiles.revealTarget(state: empty).lastPathComponent == "logs")
        let none = FileManager.default.temporaryDirectory.appending(component: "fileid-logs-none-\(UUID().uuidString)")
        #expect(EngineLogFiles.revealTarget(state: none) == none)
    }
}
