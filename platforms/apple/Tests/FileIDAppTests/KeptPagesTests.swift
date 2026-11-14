import Foundation
import Testing
@testable import FileID

@Suite("Kept-alive pages")
struct KeptPagesTests {
    private func pages(_ visits: [String], limit: Int = 4, transient: Set<String> = ["settings"]) -> KeptPages<String> {
        var kept = KeptPages<String>(limit: limit, transient: transient)
        for page in visits { kept.visit(page) }
        return kept
    }

    @Test("the first visit mounts only that page")
    func firstVisit() {
        let kept = pages(["library"])
        #expect(kept.recent == ["library"] && kept.active == "library")
    }

    @Test("revisiting moves a page to the front without duplicating it")
    func revisit() {
        let kept = pages(["library", "people", "chat", "library"])
        #expect(kept.recent == ["library", "chat", "people"])
    }

    @Test("the least recently used page is dropped past the limit and the active page always stays")
    func eviction() {
        let kept = pages(["library", "people", "chat", "cleanup", "tools"])
        #expect(kept.recent == ["tools", "cleanup", "chat", "people"])
        #expect(!kept.contains("library") && kept.active == "tools")
        #expect(pages(["a", "b"], limit: 1).recent == ["b"])
        #expect(pages(["a"], limit: 0).recent == ["a"])
    }

    @Test("a transient page is mounted while active and dropped once left")
    func transientPage() {
        var kept = pages(["library", "people", "settings"])
        #expect(kept.recent == ["settings", "people", "library"])
        kept.visit("library")
        #expect(kept.recent == ["library", "people"])
    }

    @Test("visiting the active page changes nothing, and visiting(_:) leaves the original alone")
    func idempotent() {
        let kept = pages(["library", "people"])
        #expect(kept.visiting("people") == kept)
        #expect(kept.visiting("chat").recent == ["chat", "people", "library"])
        #expect(kept.recent == ["people", "library"])
    }

    @MainActor
    @Test("the suggested-name label says when the saved name style differs from the stored suggestion")
    func suggestionLabel() throws {
        let suite = "KeptPagesTests.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        #expect(StyledNames.suggestionLabel(defaults) == "Suggested name")
        defaults.set(false, forKey: NameStyle.learnKey)
        #expect(StyledNames.suggestionLabel(defaults) == "Suggested name")
        defaults.set("dateFirst", forKey: NameStyle.styleKey)
        #expect(StyledNames.suggestionLabel(defaults) == "Suggested name (before your name style)")
        defaults.set(NameStyle.defaultStyle, forKey: NameStyle.styleKey)
        defaults.set(60, forKey: NameStyle.maxLengthKey)
        #expect(StyledNames.suggestionLabel(defaults) == "Suggested name (before your name style)")
    }
}
