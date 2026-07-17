import Foundation
import Testing
@testable import FileID

@Suite("CLIP model installer", .serialized)
@MainActor
struct CLIPModelInstallerTests {
    private struct InjectedFailure: Error {}

    private func temporaryRoot(_ label: String) throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appending(component: "fileid-clip-\(label)-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        return root
    }

    private func run(_ executable: String, _ arguments: [String], cwd: URL) throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.currentDirectoryURL = cwd
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else { throw InjectedFailure() }
    }

    @Test("promotion succeeds without leaving a backup")
    func promotionSuccess() throws {
        let root = try temporaryRoot("promote")
        defer { try? FileManager.default.removeItem(at: root) }
        let models = root.appending(component: "models", directoryHint: .isDirectory)
        let staging = root.appending(component: "staging", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: models, withIntermediateDirectories: false)
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: false)
        let staged = staging.appending(component: "model.onnx")
        let live = models.appending(component: "clip/model.onnx")
        try Data("new".utf8).write(to: staged)

        try CLIPModelInstaller.promoteStaged([(staged, live)], modelsRoot: models)

        #expect(try String(contentsOf: live, encoding: .utf8) == "new")
        #expect(!FileManager.default.fileExists(atPath: staged.path))
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: models.path)
            .filter { $0.hasPrefix(".clip-backup-") }
        #expect(leftovers.isEmpty)
    }

    @Test("promotion failure restores every original")
    func promotionRollback() throws {
        let root = try temporaryRoot("rollback")
        defer { try? FileManager.default.removeItem(at: root) }
        let models = root.appending(component: "models", directoryHint: .isDirectory)
        let staging = root.appending(component: "staging", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: models, withIntermediateDirectories: false)
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: false)
        let liveA = models.appending(component: "a/model.onnx")
        let liveB = models.appending(component: "b/model.onnx")
        let stagedA = staging.appending(component: "a.onnx")
        let stagedB = staging.appending(component: "b.onnx")
        try FileManager.default.createDirectory(at: liveA.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: liveB.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("old-a".utf8).write(to: liveA)
        try Data("old-b".utf8).write(to: liveB)
        try Data("new-a".utf8).write(to: stagedA)
        try Data("new-b".utf8).write(to: stagedB)
        var moves = 0

        #expect(throws: (any Error).self) {
            try CLIPModelInstaller.promoteStaged(
                [(stagedA, liveA), (stagedB, liveB)],
                modelsRoot: models,
                moveItem: { source, destination in
                    moves += 1
                    if moves == 4 { throw InjectedFailure() }
                    try FileManager.default.moveItem(at: source, to: destination)
                }
            )
        }

        #expect(try String(contentsOf: liveA, encoding: .utf8) == "old-a")
        #expect(try String(contentsOf: liveB, encoding: .utf8) == "old-b")
        #expect(try String(contentsOf: stagedA, encoding: .utf8) == "new-a")
        #expect(try String(contentsOf: stagedB, encoding: .utf8) == "new-b")
    }

    @Test("real ZIP preflight accepts normal files and rejects symlinks and encryption")
    func archiveExecution() async throws {
        let root = try temporaryRoot("archives")
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appending(component: "source", directoryHint: .isDirectory)
        let work = root.appending(component: "work", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: source, withIntermediateDirectories: false)
        try FileManager.default.createDirectory(at: work, withIntermediateDirectories: false)
        try Data("model".utf8).write(to: source.appending(component: "model.onnx"))
        let normal = root.appending(component: "normal.zip")
        try run("/usr/bin/zip", ["-q", normal.path, "model.onnx"], cwd: source)
        #expect(try await CLIPModelInstaller.shared.preflightArchive(normal, workRoot: work) == 5)

        let link = source.appending(component: "model-link")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: source.appending(component: "model.onnx"))
        let symlink = root.appending(component: "symlink.zip")
        try run("/usr/bin/zip", ["-q", "-y", symlink.path, "model-link"], cwd: source)
        do {
            _ = try await CLIPModelInstaller.shared.preflightArchive(symlink, workRoot: work)
            Issue.record("symlink archive passed preflight")
        } catch {
            #expect(error.localizedDescription.contains("symlink or special"))
        }

        let encrypted = root.appending(component: "encrypted.zip")
        try run("/usr/bin/zip", ["-q", "-P", "secret", encrypted.path, "model.onnx"], cwd: source)
        do {
            _ = try await CLIPModelInstaller.shared.preflightArchive(encrypted, workRoot: work)
            Issue.record("encrypted archive passed preflight")
        } catch {
            #expect(error.localizedDescription.contains("encrypted archives"))
        }
    }

    @Test("archive inspection honors cancellation")
    func archiveCancellation() async throws {
        let root = try temporaryRoot("cancel")
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appending(component: "source", directoryHint: .isDirectory)
        let work = root.appending(component: "work", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: source, withIntermediateDirectories: false)
        try FileManager.default.createDirectory(at: work, withIntermediateDirectories: false)
        try Data(repeating: 7, count: 4 * 1_024 * 1_024).write(to: source.appending(component: "model.onnx"))
        let archive = root.appending(component: "cancel.zip")
        try run("/usr/bin/zip", ["-q", archive.path, "model.onnx"], cwd: source)

        let task = Task {
            try await CLIPModelInstaller.shared.preflightArchive(archive, workRoot: work)
        }
        task.cancel()
        do {
            _ = try await task.value
            Issue.record("cancelled archive inspection completed successfully")
        } catch is CancellationError {
        } catch {
            Issue.record("cancelled archive inspection returned \(error)")
        }
    }
}
