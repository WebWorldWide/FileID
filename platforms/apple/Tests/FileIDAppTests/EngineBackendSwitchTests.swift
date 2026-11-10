import Foundation
import Testing
@testable import FileID

@Suite("Engine backend dogfood switch")
struct EngineBackendSwitchTests {
    private func defaults(_ value: String?) -> UserDefaults {
        let suite = "FileIDEngineBackendTests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        if let value { defaults.set(value, forKey: EngineClient.engineBackendKey) }
        return defaults
    }

    @Test func swiftIsTheDefault() {
        #expect(EngineClient.requestedEngineBackend(environment: [:], defaults: defaults(nil)) == "swift")
        #expect(EngineClient.requestedEngineBackend(environment: ["FILEID_ENGINE_BACKEND": ""], defaults: defaults(nil)) == "swift")
        #expect(EngineClient.requestedEngineBackend(environment: [:], defaults: defaults("mlx")) == "swift")
    }

    @Test func rustOnlyWhenAsked() {
        #expect(EngineClient.requestedEngineBackend(environment: [:], defaults: defaults("rust")) == "rust")
        #expect(EngineClient.requestedEngineBackend(environment: ["FILEID_ENGINE_BACKEND": "Rust"], defaults: defaults(nil)) == "rust")
        #expect(EngineClient.requestedEngineBackend(environment: ["FILEID_ENGINE_BACKEND": "swift"], defaults: defaults("rust")) == "swift")
    }

    @Test func swiftNeverStartsTheRustBinary() {
        #expect(EngineClient.engineBinaryNames(backend: "swift") == ["FileIDEngine"])
        #expect(EngineClient.engineBinaryNames(backend: "rust") == ["fileidd", "FileIDEngine"])
    }
}
