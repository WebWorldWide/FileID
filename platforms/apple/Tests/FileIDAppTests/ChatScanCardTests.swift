import Foundation
import Testing
import FileIDShared
@testable import FileID

@MainActor
@Suite("Chat scan card")
struct ChatScanCardTests {
    private func action() -> ChatAction {
        ChatAction(id: UUID().uuidString, kind: "scanFolder", title: "Scan", detail: "", requiresConfirmation: false)
    }

    @Test(.timeLimit(.minutes(1))) func aScanThatNeverStartedIsNotReportedAsComplete() async {
        let runner = ChatActionRunner()
        let a = action()
        runner.markRunning(a, "Starting the scan")
        await runner.watch(a, started: { false }, finished: { true }, mustStart: true, startWait: 0.3) { "Scan complete." }
        guard case .failed(let text) = runner.phase(a) else { Issue.record("expected failed"); return }
        #expect(text.contains("didn’t start"))
    }

    @Test(.timeLimit(.minutes(1))) func aScanTheHelperRefusesFailsTheCardAtOnce() async {
        let runner = ChatActionRunner()
        let a = action()
        runner.markRunning(a, "Starting the scan")
        await runner.watch(a, started: { false }, finished: { true }, interrupted: { "helper stopped" }, mustStart: true, startWait: 30) { "Scan complete." }
        #expect(runner.phase(a) == .failed("helper stopped"))
    }

    @Test(.timeLimit(.minutes(1))) func aScanThatRanReportsItsResult() async {
        let runner = ChatActionRunner()
        let a = action()
        runner.markRunning(a, "Starting the scan")
        await runner.watch(a, started: { true }, finished: { true }, mustStart: true) { "Scan complete." }
        #expect(runner.phase(a) == .done("Scan complete."))
    }
}
