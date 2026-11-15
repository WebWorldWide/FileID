import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Sidebar and status after a fall back")
struct HostedGapsTests {
    @Test("no area is lit while Settings is open")
    func settingsLightsNoArea() {
        #expect(Sidebar.highlightedArea(activeTab: .settings) == nil)
        #expect(Sidebar.highlightedArea(activeTab: .library) == .browse)
        #expect(Sidebar.highlightedArea(activeTab: .takes) == .analyze)
        #expect(Sidebar.highlightedArea(activeTab: .restructure) == .act)
    }

    @Test("the generic card is not raised for the fall back to the Swift engine")
    func fallbackRaisesNoCard() {
        #expect(!Sidebar.showsErrorCard("engine_fallback"))
        #expect(Sidebar.showsErrorCard("engine_exited"))
        #expect(Sidebar.showsErrorCard("scan_failed"))
        #expect(Sidebar.showsErrorCard("something_new"))
    }

    private func job(_ state: String) -> CatalogJob {
        CatalogJob(id: state, kind: "catalogIndex", state: state, progress: 0, createdAt: 0, updatedAt: 0)
    }

    @Test("a job list is asked for again only while a job is waiting or running")
    func jobsRefreshWhileActive() {
        #expect(MainWindow.jobsNeedRefresh([job("running")]))
        #expect(MainWindow.jobsNeedRefresh([job("completed"), job("queued")]))
        #expect(!MainWindow.jobsNeedRefresh([job("completed"), job("paused"), job("cancelled")]))
        #expect(!MainWindow.jobsNeedRefresh([]))
    }
}
