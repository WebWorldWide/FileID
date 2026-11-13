import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Engine backend choice")
struct EngineBackendSwitchTests {
    private func defaults(_ value: String?) -> UserDefaults {
        let suite = "FileIDEngineBackendTests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        if let value { defaults.set(value, forKey: EngineBackendPolicy.defaultsKey) }
        return defaults
    }

    private func candidates(_ env: [String: String] = [:], _ value: String? = nil, appStore: Bool = false,
                            rustBundled: Bool = true, demoted: Set<EngineBackend> = []) -> [EngineBackend] {
        EngineBackendPolicy.candidates(environment: env, defaults: defaults(value), appStore: appStore,
                                       rustBundled: rustBundled, demoted: demoted)
    }

    @Test func rustIsTheDefaultWhenBundled() {
        #expect(candidates() == [.rust, .swift])
        #expect(candidates(["FILEID_ENGINE_BACKEND": ""]) == [.rust, .swift])
        #expect(candidates([:], "mlx") == [.rust, .swift])
        #expect(candidates(rustBundled: false) == [.swift])
    }

    @Test func theSettingAndTheEnvironmentStillChooseSwift() {
        #expect(candidates([:], "swift") == [.swift])
        #expect(candidates([:], "Swift") == [.swift])
        #expect(candidates(["FILEID_ENGINE_BACKEND": "swift"], "rust") == [.swift])
        #expect(candidates(["FILEID_ENGINE_BACKEND": "Rust"], "swift") == [.rust, .swift])
        #expect(candidates([:], "rust", rustBundled: false) == [.rust, .swift])
    }

    @Test func appStoreBuildsRunOnlySwift() {
        #expect(candidates(appStore: true) == [.swift])
        #expect(candidates(["FILEID_ENGINE_BACKEND": "rust"], "rust", appStore: true) == [.swift])
        #expect(EngineBackendPolicy.preferred(environment: [:], defaults: defaults("rust"), appStore: true, rustBundled: true) == .swift)
    }

    @Test func aDemotedRustEngineIsSkippedButSwiftNeverIs() {
        #expect(candidates(demoted: [.rust]) == [.swift])
        #expect(candidates([:], "swift", demoted: [.swift]) == [.swift])
    }

    @Test func binaryNames() {
        #expect(EngineBackend.rust.binaryName == "fileidd")
        #expect(EngineBackend.swift.binaryName == "FileIDEngine")
    }

    @Test func fallbackDemotesRustOnALaunchFailureOrACrashLoop() {
        var fallback = EngineFallback()
        let swiftDemoted = fallback.launchFailed(.swift, reason: "x")
        #expect(!swiftDemoted)
        #expect(fallback.demoted.isEmpty && fallback.note == nil)

        let first = fallback.exitedBeforeReady(.rust)
        fallback.reachedReady(.rust)
        let afterReady = fallback.exitedBeforeReady(.rust)
        let second = fallback.exitedBeforeReady(.rust)
        #expect(!first)
        #expect(!afterReady, "a ready in between resets the count")
        #expect(second)
        #expect(fallback.demoted == [.rust])
        #expect(fallback.note?.contains("Swift engine") == true)
        let again = fallback.respawnsExhausted(.rust)
        #expect(!again, "already demoted")

        var signature = EngineFallback()
        let refused = signature.launchFailed(.rust, reason: "its signature doesn't match the app")
        #expect(refused)
        #expect(signature.note?.contains("signature") == true)

        var budget = EngineFallback()
        let rustBudget = budget.respawnsExhausted(.rust)
        let swiftBudget = budget.respawnsExhausted(.swift)
        #expect(rustBudget && !swiftBudget)
    }
}

/// EngineClient itself, against stand-in engines in a scratch folder: the
/// Swift stand-in sends `ready` and runs until `shutdown`; the Rust one
/// crashes at once, is refused by the signing check, or is missing.
@MainActor
@Suite("Engine fallback to Swift", .serialized)
struct EngineFallbackTests {
    private final class Folder {
        let url: URL
        init() throws {
            url = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-engine-fallback-\(UUID().uuidString)")
            try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        }
        deinit { try? FileManager.default.removeItem(at: url) }

        func script(_ name: String, _ body: String) throws {
            let file = url.appendingPathComponent(name)
            try ("#!/bin/sh\n" + body + "\n").write(to: file, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: file.path)
        }

        func swiftStandIn() throws {
            let ready = IPCEvent(payload: .ready(EngineInfo(version: "test", pid: 1, workerCap: 1, physicalMemoryGB: 16, engine: "swift")))
            try IPCCoder.encodeLine(ready).write(to: url.appendingPathComponent("ready.json"))
            try script("FileIDEngine", """
            cat "$(dirname "$0")/ready.json" >&2
            while read -r line; do case "$line" in *shutdown*) exit 0 ;; esac; done
            """)
        }
    }

    private func client(_ folder: Folder, backend: String? = nil, refuse: Set<String> = []) -> EngineClient {
        let suite = "FileIDEngineFallbackTests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        if let backend { defaults.set(backend, forKey: EngineBackendPolicy.defaultsKey) }
        let launch = EngineClient.LaunchContext(directory: folder.url, environment: [:], defaults: defaults,
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

    @Test func aRustEngineThatCrashesWhileStartingFallsBackToSwift() async throws {
        let folder = try Folder()
        try folder.swiftStandIn()
        try folder.script("fileidd", "exit 3")
        let engine = client(folder)
        #expect(engine.preferredBackend == .rust)
        engine.start()
        #expect(engine.runningBackend == .rust)
        let info = await waitForReady(engine)
        #expect(info?.engine == "swift")
        #expect(engine.runningBackend == .swift)
        #expect(engine.fallback.demoted == [.rust])
        #expect(engine.lastError?.kind == "engine_fallback")
        engine.start()
        #expect(engine.runningBackend == .swift, "a restart in the same session stays on Swift")
        _ = await waitForReady(engine)
        engine.shutdown()
    }

    @Test func aRustEngineWithTheWrongSignatureFallsBackToSwift() async throws {
        let folder = try Folder()
        try folder.swiftStandIn()
        try folder.script("fileidd", "sleep 30")
        let engine = client(folder, refuse: ["fileidd"])
        engine.start()
        #expect(engine.runningBackend == .swift)
        #expect(engine.fallback.note?.contains("signature") == true)
        #expect(await waitForReady(engine) != nil)
        engine.shutdown()
    }

    @Test func aMissingRustEngineFallsBackToSwift() async throws {
        let folder = try Folder()
        try folder.swiftStandIn()
        let chosen = client(folder, backend: "rust")
        chosen.start()
        #expect(chosen.runningBackend == .swift)
        #expect(chosen.fallback.demoted == [.rust])
        #expect(await waitForReady(chosen) != nil)
        chosen.shutdown()

        let unchosen = client(folder)
        #expect(unchosen.preferredBackend == .swift, "without fileidd the default is Swift, with no fallback note")
        unchosen.start()
        #expect(unchosen.runningBackend == .swift && unchosen.fallback.note == nil)
        unchosen.shutdown()
    }

    @Test func choosingSwiftNeverStartsTheRustEngine() async throws {
        let folder = try Folder()
        try folder.swiftStandIn()
        try folder.script("fileidd", "exit 3")
        let engine = client(folder, backend: "swift")
        engine.start()
        #expect(engine.runningBackend == .swift)
        #expect(await waitForReady(engine) != nil)
        #expect(engine.fallback.demoted.isEmpty)
        engine.shutdown()
    }
}
