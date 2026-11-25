import AppKit
import Testing
@testable import FileID

@Suite("Library at whole-drive scale")
struct LibraryScaleTests {
    @Test func theVisualHalfOfASearchStartsBesideTheKeywordListing() throws {
        let library = try appSource("Views/LibraryView.swift")
        let start = try #require(library.range(of: "Task { @MainActor in await visualHalf(of: trimmed) }"))
        let keyword = try #require(library.range(of: "let first = await store.filesPageAsync(limit: want"))
        #expect(start.lowerBound < keyword.lowerBound, "the vector half is already running when the keyword query is awaited")
    }
}
