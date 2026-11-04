import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Engine reset recovery")
struct EngineResetRecoveryTests {
    private func progress(_ phase: ScanPhase) -> ScanProgress {
        ScanProgress(sessionID: "s", phase: phase, total: 400, discovered: 400, processed: 120, failed: 3,
                     filesPerSecond: 42, etaSeconds: 7, residentMB: 300, availableMB: 9000)
    }

    @Test func liveScanProgressSettlesToFailedWhenTheEngineDies() {
        for phase in [ScanPhase.discovering, .tagging, .postScan] {
            let settled = progress(phase).settledAfterEngineExit()
            #expect(settled.phase == .failed)
            #expect(settled.processed == 120 && settled.total == 400 && settled.failed == 3)
            #expect(settled.etaSeconds == nil && settled.filesPerSecond == 0)
        }
        for phase in [ScanPhase.idle, .completed, .cancelled, .failed] {
            #expect(progress(phase).settledAfterEngineExit().phase == phase)
        }
    }

    @MainActor
    @Test func toolsSessionReleasesARequestTheDeadEngineCannotAnswer() {
        let session = ToolsSession()
        session.pending = "req-1"
        session.pendingAction = "preview"
        session.pendingEngineGeneration = 4

        session.abandonPendingIfEngineRestarted(generation: 4)
        #expect(session.pending == "req-1")

        session.abandonPendingIfEngineRestarted(generation: 5)
        #expect(session.pending.isEmpty)
        #expect(session.pendingAction.isEmpty)
        #expect(session.message.contains("stopped"))
    }

    @MainActor
    @Test func toolsSessionKeepsIdleStateWhenNothingIsPending() {
        let session = ToolsSession()
        let before = session.message
        session.abandonPendingIfEngineRestarted(generation: 9)
        #expect(session.message == before)
    }

    @Test func folderJobRejectionsEndTheChatCardWait() {
        #expect(ChatActionRunner.folderJobInterruption(engineRestarted: true, newError: nil) != nil)
        #expect(ChatActionRunner.folderJobInterruption(engineRestarted: false, newError: nil) == nil)
        for kind in ["restructure_busy", "plan_restructure_failed", "apply_restructure", "undo_restructure", "db_unavailable"] {
            let reason = ChatActionRunner.folderJobInterruption(engineRestarted: false, newError: EngineError(kind: kind, message: "Apply failed: NSCocoaErrorDomain 4"))
            #expect(reason != nil)
            #expect(reason?.contains("NSCocoaErrorDomain") == false)
        }
        #expect(ChatActionRunner.folderJobInterruption(engineRestarted: false, newError: EngineError(kind: "face_clustering_failed", message: "x")) == nil)
    }

    @MainActor
    @Test(.timeLimit(.minutes(1))) func chatCardWaitStopsWhenTheJobCanNoLongerFinish() async {
        let runner = ChatActionRunner()
        let action = ChatAction(id: UUID().uuidString, kind: "suggestFolderLayout", title: "Organize", detail: "", requiresConfirmation: false)
        runner.markRunning(action, "Planning a tidier layout")
        var polls = 0
        await runner.watch(action, started: { true }, finished: { false }, interrupted: {
            polls += 1
            return polls >= 2 ? "engine restarted" : nil
        }) { "done" }
        #expect(runner.phase(action) == .failed("engine restarted"))
    }
}
