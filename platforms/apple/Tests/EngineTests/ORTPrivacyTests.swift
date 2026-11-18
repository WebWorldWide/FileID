import Foundation
import OnnxRuntimeBindings
import Testing
@testable import FileIDORTSupport

@Suite("ONNX Runtime telemetry stays off")
struct ORTPrivacyTests {
    private static let appleRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()

    private static func source(_ relative: String) throws -> String {
        try String(contentsOf: appleRoot.appendingPathComponent(relative), encoding: .utf8)
    }

    @Test func environmentCreationDisablesTelemetry() throws {
        _ = try ORTPrivacy.makeEnv(loggingLevel: .warning)
        #expect(ProcessInfo.processInfo.environment[ORTPrivacy.disableTelemetryVariable] == "1")
        #expect(ORTPrivacy.disableTelemetryEvents(), "the C API's DisableTelemetryEvents must resolve and succeed")
    }

    @Test func everyEnvironmentGoesThroughORTPrivacy() throws {
        let manager = FileManager.default
        var offenders: [String] = []
        for folder in ["app/Sources", "engine/Sources", "shared/Sources", "adapters/Sources"] {
            let root = Self.appleRoot.appendingPathComponent(folder)
            let files = manager.enumerator(at: root, includingPropertiesForKeys: nil)?
                .compactMap { $0 as? URL }.filter { $0.pathExtension == "swift" } ?? []
            for file in files where file.lastPathComponent != "ORTPrivacy.swift" {
                if try String(contentsOf: file, encoding: .utf8).contains("ORTEnv(") {
                    offenders.append(file.lastPathComponent)
                }
            }
        }
        #expect(offenders.isEmpty, "create ORT environments with ORTPrivacy.makeEnv: \(offenders)")
    }

    @Test func processesDisableTelemetryBeforeLoadingORT() throws {
        let engine = try Self.source("engine/Sources/FileIDEngine/FileIDEngineMain.swift")
        let main = try #require(engine.range(of: "static func main() async {"))
        let firstLine = engine[main.upperBound...].drop(while: \.isWhitespace).prefix(while: { $0 != "\n" })
        #expect(firstLine == "ORTPrivacy.disableTelemetryBeforeLoad()")
    }
}
