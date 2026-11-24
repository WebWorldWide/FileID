import Foundation
import Testing
import FileIDShared
@testable import FileID

@MainActor
@Suite("Describe run errors")
struct DeepRunErrorTests {
    @Test func aFileThatChangedDuringTheRunDoesNotEndTheRun() {
        let engine = EngineClient()
        engine.handleEvent(IPCEvent(payload: .deepAnalyzeProgress(DeepAnalyzeProgress(processed: 1, total: 10, etaSeconds: nil, currentPath: nil, modelKind: "m"))))
        #expect(engine.deepAnalyzeInFlight)
        engine.handleEvent(IPCEvent(payload: .error(EngineError(kind: "deep_source_changed", message: "A file changed."))))
        #expect(engine.deepAnalyzeInFlight)
        #expect(engine.deepAnalyzeProgress != nil)
        engine.handleEvent(IPCEvent(payload: .error(EngineError(kind: "deep_analyze_failed", message: "Stopped."))))
        #expect(!engine.deepAnalyzeInFlight)
    }
}
