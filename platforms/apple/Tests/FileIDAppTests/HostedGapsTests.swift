import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Sidebar and status after a fall back")
struct HostedGapsTests {
    @Test("Settings keeps the area the person was last in lit; every other page lights its own")
    func settingsKeepsTheLastArea() {
        #expect(Sidebar.areaShown(activeTab: .settings, lastUsed: .act) == .act)
        #expect(Sidebar.areaShown(activeTab: .settings, lastUsed: .browse) == .browse)
        #expect(Sidebar.areaShown(activeTab: .library, lastUsed: .act) == .browse)
        #expect(Sidebar.areaShown(activeTab: .takes, lastUsed: .browse) == .analyze)
        #expect(Sidebar.areaShown(activeTab: .restructure, lastUsed: .browse) == .act)
    }

    @Test("the last area survives a relaunch")
    func lastAreaIsStored() throws {
        let suite = "fileid.lastarea.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        #expect(MainWindow.Area.lastUsed(defaults: defaults) == .browse)
        defaults.set(MainWindow.Area.act.rawValue, forKey: MainWindow.Area.lastUsedKey)
        #expect(MainWindow.Area.lastUsed(defaults: defaults) == .act)
    }

    @Test("the generic card is raised for everything but a stopped folder plan")
    func genericCardKinds() {
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
