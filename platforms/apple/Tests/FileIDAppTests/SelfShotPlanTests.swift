#if DEBUG
import Testing
import Foundation
@testable import FileID

@Suite("Self-capture plan")
struct SelfShotPlanTests {
    private let all = MainWindow.Tab.allCases.map(\.rawValue)

    @Test func off_without_dir() {
        #expect(SelfShotPlan.from(environment: [:], available: all) == nil)
        #expect(SelfShotPlan.from(environment: [SelfShotPlan.dirKey: ""], available: all) == nil)
    }

    @Test func slugs() {
        #expect(SelfShotPlan.slug("Search & Moments") == "search-moments")
        #expect(SelfShotPlan.slug("Deep Analyze") == "deep-analyze")
        #expect(SelfShotPlan.slug("Settings") == "settings")
    }

    @Test func default_is_every_page_including_settings() {
        let plan = SelfShotPlan.from(environment: [SelfShotPlan.dirKey: "/tmp/x"], available: all)
        #expect(plan?.pages == all)
        #expect(plan?.pages.contains("Settings") == true)
        #expect(plan?.settle == 2)
    }

    @Test func pages_list_accepts_slugs_drops_unknown_and_duplicates() {
        let pages = SelfShotPlan.parsePages("settings, search-moments,Nope,SETTINGS,library", available: all)
        #expect(pages == ["Settings", "Search & Moments", "Library"])
    }

    @Test func settle_folder_and_size() {
        let env = [SelfShotPlan.dirKey: "/tmp/x", "FILEID_SELF_SHOT_SETTLE": "0.5",
                   "FILEID_SELF_SHOT_FOLDER": "/tmp/lib", "FILEID_WIN_SIZE": "1440x900"]
        let plan = SelfShotPlan.from(environment: env, available: all)
        #expect(plan?.settle == 0.5)
        #expect(plan?.folder?.path == "/tmp/lib")
        #expect(plan?.windowSize == CGSize(width: 1440, height: 900))
        #expect(SelfShotPlan.parseSize("bad") == nil)
        #expect(SelfShotPlan.parseSize("10x10") == nil)
    }

    @Test func views_parse_page_and_options() {
        let views = SelfShotView.parse("docs=Library?kind=doc;found=Library?query=invoice%20total&kind=pdf;lower=Settings?scroll=bottom;bad=Nowhere;noequals", available: all)
        #expect(views.map(\.name) == ["docs", "found", "lower"])
        #expect(views[0].kind == "doc" && views[0].query == nil)
        #expect(views[1].query == "invoice total" && views[1].kind == "pdf")
        #expect(views[2].page == "Settings" && views[2].scrollToBottom)
        #expect(SelfShotView.parse(nil, available: all).isEmpty)
    }

    @Test func views_parse_preview_file() {
        let views = SelfShotView.parse("big=Library?query=huge&preview=huge%206500.jpg", available: all)
        #expect(views.first?.preview == "huge 6500.jpg" && views.first?.query == "huge")
    }
}
#endif
