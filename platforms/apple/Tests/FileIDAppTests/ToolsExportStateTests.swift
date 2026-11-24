import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("File Tools export state")
struct ToolsExportStateTests {
    @Test func aFailedExportThatWroteNothingIsNotMarkedExported() {
        let failed = ToolOutput(fileID: 1, sourcePath: "/a", outputPath: "/b", state: "failed", message: "no space")
        let pending = ToolOutput(fileID: 1, sourcePath: "/a", outputPath: "/b", state: "pending")
        #expect(!ToolsSession.exportCounts(status: "error", outputs: []))
        #expect(!ToolsSession.exportCounts(status: "error", outputs: [failed]))
        #expect(!ToolsSession.exportCounts(status: "error", outputs: [pending]))
    }

    @Test func anExportThatWorkedOrLeftAFinishedFileIsMarkedExported() {
        let done = ToolOutput(fileID: 1, sourcePath: "/a", outputPath: "/b", state: "completed")
        #expect(ToolsSession.exportCounts(status: "ok", outputs: [done]))
        #expect(ToolsSession.exportCounts(status: "error", outputs: [done]))
    }
}
