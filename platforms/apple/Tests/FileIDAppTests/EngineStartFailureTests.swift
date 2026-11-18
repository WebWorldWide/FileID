import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Engine start failure wording")
struct EngineStartFailureWordingTests {
    private static let causes: [EngineStartFailure.Cause] = [.missing, .signatureMismatch, .couldNotLaunch, .exitsAtStart, .keepsStopping]

    @Test func everyCauseIsOneSentenceThatNamesTheHelper() {
        for cause in Self.causes {
            let summary = EngineStartFailure(cause).summary
            #expect(summary.hasPrefix("FileID's helper") || summary.hasPrefix("macOS wouldn't"), "\(cause)")
            #expect(summary.hasSuffix("."), "\(cause)")
            #expect(summary.filter { $0 == "." }.count == 1, "\(cause)")
        }
    }

    @Test func reinstallIsTheFixOnlyWhenTheFileIsMissingOrNotTheOneShipped() {
        #expect(EngineStartFailure(.missing).suggestsReinstall)
        #expect(EngineStartFailure(.signatureMismatch).suggestsReinstall)
        #expect(EngineStartFailure(.couldNotLaunch).suggestsReinstall)
        #expect(!EngineStartFailure(.exitsAtStart).suggestsReinstall)
        #expect(!EngineStartFailure(.keepsStopping).suggestsReinstall)
    }

    @Test func theCardNamesTheProblemAndTheWayOut() {
        #expect(EngineUnavailableCard.title == "FileID's helper couldn't start")
        let missing = EngineUnavailableCard.reinstallHint(for: EngineStartFailure(.missing))
        let stops = EngineUnavailableCard.reinstallHint(for: EngineStartFailure(.exitsAtStart))
        #expect(missing.contains("Reinstall FileID"))
        #expect(stops.contains("If it keeps happening, reinstall FileID"))
        #expect(missing != stops)
    }

    @Test func anEngineThatStopsTwiceBeforeReadyIsGivenUpOn() {
        var exits = EngineStartupExits()
        let first = exits.exitedBeforeReady()
        let second = exits.exitedBeforeReady()
        exits.reachedReady()
        let afterReady = exits.exitedBeforeReady()
        #expect(!first)
        #expect(second)
        #expect(!afterReady, "reaching ready starts the count over")
    }
}

/// EngineClient itself against stand-in engines in a scratch folder. There is one engine,
/// `fileidd`; a Swift engine stand-in beside it must never be started.
@MainActor
@Suite("Engine unavailable", .serialized)
struct EngineUnavailableTests {
    private final class Folder {
        let url: URL
        init() throws {
            url = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-engine-unavailable-\(UUID().uuidString)")
            try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        }
        deinit { try? FileManager.default.removeItem(at: url) }

        func script(_ name: String, _ body: String) throws {
            let file = url.appendingPathComponent(name)
            try ("#!/bin/sh\n" + body + "\n").write(to: file, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: file.path)
        }

        func readyEngine(named name: String = EngineBinary.name, kind: String = "rust") throws {
            let ready = IPCEvent(payload: .ready(EngineInfo(version: "test", pid: 1, workerCap: 1, physicalMemoryGB: 16, engine: kind)))
            try IPCCoder.encodeLine(ready).write(to: url.appendingPathComponent("ready-\(name).json"))
            try script(name, """
            cat "$(dirname "$0")/ready-\(name).json" >&2
            while read -r line; do case "$line" in *shutdown*) exit 0 ;; esac; done
            """)
        }
    }

    private func client(_ folder: Folder, refuse: Set<String> = []) -> EngineClient {
        let launch = EngineClient.LaunchContext(directory: folder.url, environment: [:],
                                                integrityFailure: { refuse.contains($0.lastPathComponent) ? "refused" : nil })
        return EngineClient(launch: launch)
    }

    private func waitForReady(_ client: EngineClient, seconds: Double = 10) async -> EngineInfo? {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if case .ready(let info) = client.state { return info }
            try? await Task.sleep(for: .milliseconds(50))
        }
        return nil
    }

    private func waitForFailure(_ client: EngineClient, seconds: Double = 15) async -> EngineStartFailure? {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if let failure = client.startFailure { return failure }
            try? await Task.sleep(for: .milliseconds(50))
        }
        return nil
    }

    @Test func theBundledEngineStartsAndShowsNoFailure() async throws {
        let folder = try Folder()
        try folder.readyEngine()
        let engine = client(folder)
        engine.start()
        #expect(await waitForReady(engine)?.engine == "rust")
        #expect(engine.startFailure == nil)
        engine.shutdown()
    }

    @Test func aMissingEngineShowsTheCardAndStartsNothingElse() async throws {
        let folder = try Folder()
        try folder.readyEngine(named: "FileIDEngine", kind: "swift")
        let engine = client(folder)
        engine.start()
        #expect(engine.startFailure == EngineStartFailure(.missing))
        guard case .crashed(let reason) = engine.state else { Issue.record("state is \(engine.state)"); return }
        #expect(reason == EngineStartFailure(.missing).summary)
        try await Task.sleep(for: .milliseconds(400))
        #expect(await waitForReady(engine, seconds: 1) == nil, "a Swift engine beside the app is never started")
        #expect(engine.send(.shutdown) == false)
    }

    @Test func anEngineWithTheWrongSignatureIsRefusedWithoutFallingBack() async throws {
        let folder = try Folder()
        try folder.readyEngine(named: "FileIDEngine", kind: "swift")
        try folder.script("fileidd", "sleep 30")
        let engine = client(folder, refuse: ["fileidd"])
        engine.start()
        #expect(engine.startFailure == EngineStartFailure(.signatureMismatch))
        #expect(await waitForReady(engine, seconds: 1) == nil)
    }

    @Test func anEngineThatExitsAtOnceShowsTheCardAfterTwoTries() async throws {
        let folder = try Folder()
        try folder.readyEngine(named: "FileIDEngine", kind: "swift")
        try folder.script("fileidd", "exit 3")
        let engine = client(folder)
        engine.start()
        #expect(await waitForFailure(engine) == EngineStartFailure(.exitsAtStart))
        guard case .crashed = engine.state else { Issue.record("state is \(engine.state)"); return }
        #expect(engine.lastError?.kind != "engine_exited", "the sidebar shows no second, generic card")
        try await Task.sleep(for: .milliseconds(1500))
        guard case .crashed = engine.state else { Issue.record("a Swift engine took over: \(engine.state)"); return }
    }

    @Test func tryAgainStartsTheEngineOnceItIsBackInPlace() async throws {
        let folder = try Folder()
        let engine = client(folder)
        engine.start()
        #expect(engine.startFailure == EngineStartFailure(.missing))
        try folder.readyEngine()
        engine.restart()
        #expect(engine.startFailure == nil)
        #expect(await waitForReady(engine)?.engine == "rust")
        engine.shutdown()
    }

    @Test func tryAgainForgetsTheEarlierExits() async throws {
        let folder = try Folder()
        try folder.script("fileidd", "exit 3")
        let engine = client(folder)
        engine.start()
        #expect(await waitForFailure(engine) == EngineStartFailure(.exitsAtStart))
        try folder.readyEngine()
        engine.restart()
        #expect(await waitForReady(engine) != nil)
        #expect(engine.startFailure == nil)
        engine.shutdown()
    }
}
