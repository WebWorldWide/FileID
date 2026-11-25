import AppKit
import Testing
@testable import FileID

@Suite("Library at whole-drive scale")
struct LibraryScaleTests {
    @Test func endAndCommandDownJumpToTheEndOfTheListingInGridAndList() throws {
        let grid = try appSource("Views/LibraryCollectionGrid.swift")
        let list = try appSource("Views/LibraryListTable.swift")
        for (name, text) in [("grid", grid), ("list", list)] {
            #expect(text.contains("case 119: .end"), "\(name) maps the End key")
            #expect(text.contains("case 115: .home"), "\(name) maps the Home key")
            #expect(text.contains("command ? .end : .down") || text.contains("contains(.command) ? .end : .down"), "\(name) maps Command-Down")
            #expect(text.contains("model.onJumpToEnd()"), "\(name) asks the library to load the rest")
            #expect(text.contains("scrollToEndToken"), "\(name) scrolls to the last row once it is loaded")
        }
    }

    @Test func jumpingToTheEndLoadsEveryRemainingPageBeforeScrolling() throws {
        let library = try appSource("Views/LibraryView.swift")
        let jump = try #require(library.range(of: "private func loadPages(untilEnd: Bool)"))
        let body = String(library[jump.lowerBound...].prefix(4_800))
        #expect(body.contains("} while untilEnd && canLoadMore"))
        #expect(body.contains("LibraryPageSizes.jump"))
        let loop = try #require(body.range(of: "} while untilEnd && canLoadMore"))
        let scroll = try #require(body.range(of: "scrollToEndToken &+= 1"))
        #expect(loop.lowerBound < scroll.lowerBound, "the view lands on the last row only after the last page arrived")
    }

    @Test func aRefreshKeepsEveryLoadedPageInsteadOfCuttingBackToThreeThousandRows() throws {
        let library = try appSource("Views/LibraryView.swift")
        #expect(!library.contains("refreshCap"))
        #expect(library.contains("let want = sameListing && !firstPageOnly ? max(Self.pageSize, rows.count) : Self.pageSize"))
    }

    @Test func rowsLoadedWithoutTagsGetTheirChipsFromTheBottomUp() throws {
        let library = try appSource("Views/LibraryView.swift")
        #expect(library.contains("private func fillTags(for ids: [Int64]"))
        #expect(library.contains("let laterIDs = newRows.dropFirst(Self.tagChunk)"))
    }

    @Test func theVisualHalfOfASearchStartsBesideTheKeywordListing() throws {
        let library = try appSource("Views/LibraryView.swift")
        let start = try #require(library.range(of: "Task { @MainActor in await visualHalf(of: trimmed) }"))
        let keyword = try #require(library.range(of: "let first = await store.filesPageAsync(limit: want"))
        #expect(start.lowerBound < keyword.lowerBound, "the vector half is already running when the keyword query is awaited")
    }

    @MainActor @Test func aVideoTileShowsItsIconAtOnceInsteadOfWaitingOutTheRetry() async throws {
        let missing = URL(fileURLWithPath: "/nonexistent/fileid-test/clip.mp4")
        let service = ThumbnailService.shared
        var missedAt: Date?
        let started = Date()
        let task = Task { @MainActor in
            await service.tileThumbnail(for: missing, size: 224, onMiss: { missedAt = Date() })
        }
        for _ in 0..<80 where missedAt == nil { try await Task.sleep(for: .milliseconds(50)) }
        task.cancel()
        let missed = try #require(missedAt, "the tile was told its first ask came back empty")
        #expect(missed.timeIntervalSince(started) < 4, "well before the five second retry")
    }

    @Test func bothTilesPassTheirIconToTheThumbnailWait() throws {
        for file in ["Views/LibraryCollectionGrid.swift", "Views/LibraryListTable.swift"] {
            #expect(try appSource(file).contains("tileThumbnail(for: row.url, size: 224, onMiss: { show(nil) })"), "\(file)")
        }
    }
}
