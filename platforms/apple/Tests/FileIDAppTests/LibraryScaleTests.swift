import AppKit
import FileIDShared
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

    @Test func aRefreshRequeriesOnlyTheHeadOfAListingLoadedToTheEnd() throws {
        let library = try appSource("Views/LibraryView.swift")
        #expect(!library.contains("max(Self.pageSize, rows.count)"))
        #expect(library.contains("LibraryReload.plan(loaded: rows.count"))
        #expect(library.contains("private func recheckTail("))
        let small = LibraryReload.plan(loaded: 1_200, sameListing: true, firstPageOnly: false, searching: false, complete: true)
        #expect(small == LibraryReload.Plan(want: 1_200, keepsTail: false), "a short listing is re-read whole")
        let far = LibraryReload.plan(loaded: 129_979, sameListing: true, firstPageOnly: false, searching: false, complete: true)
        #expect(far == LibraryReload.Plan(want: LibraryReload.head, keepsTail: true), "after End a refresh asks for one head, not 130,000 rows")
        let partial = LibraryReload.plan(loaded: 20_000, sameListing: true, firstPageOnly: false, searching: false, complete: false)
        #expect(partial == LibraryReload.Plan(want: LibraryReload.head, keepsTail: false), "a half-loaded listing pages on from the head")
        let search = LibraryReload.plan(loaded: 20_000, sameListing: true, firstPageOnly: false, searching: true, complete: true)
        #expect(!search.keepsTail && search.want == LibraryReload.head)
        let other = LibraryReload.plan(loaded: 129_979, sameListing: false, firstPageOnly: false, searching: false, complete: true)
        #expect(other.want == LibraryPageSizes.first)
    }

    @Test func aRefreshedHeadSplicesOntoTheRowsLoadedBeyondItAndDropsWhatLeftTheListing() {
        func row(_ id: Int64, size: Int64 = 1) -> FileRow {
            FileRow(id: id, pathText: "/x/\(id).jpg", sizeBytes: size, createdAt: nil, modifiedAt: nil, scannedAt: Date(timeIntervalSince1970: 0), kind: "image", extension: "jpg", phash: nil,
                    aesthetic: nil, hasFaces: false, hasText: false, cameraModel: nil, locationLat: nil, locationLon: nil, failed: false, errorMessage: nil)
        }
        let loaded = (1...10).map { row(Int64($0)) }
        let head = [row(0), row(1, size: 9), row(2), row(3)]
        let spliced = LibraryReload.splice(head: head, onto: loaded)
        #expect(spliced?.map(\.id) == [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10])
        #expect(spliced?[1].sizeBytes == 9)
        #expect(LibraryReload.splice(head: [row(99)], onto: loaded) == nil, "a head that left the loaded rows entirely cannot be spliced")
        let checked = Set((4...10).map(Int64.init))
        let applied = LibraryReload.applying([5: row(5, size: 7), 6: row(6)], checked: checked, to: spliced ?? [])
        #expect(applied.map(\.id) == [0, 1, 2, 3, 5, 6])
        #expect(applied.first { $0.id == 5 }?.sizeBytes == 7)
    }

    @Test func theListingSaysHowManyRowsWaitBelowTheLoadedOnes() throws {
        #expect(LibraryView.moreBelowNote(loaded: 2_300, total: 129_979) == "2,300 of 129,979 loaded · Load all")
        #expect(LibraryView.moreBelowNote(loaded: 129_979, total: 129_979) == nil)
        #expect(LibraryView.moreBelowNote(loaded: 600, total: nil) == "More files below · Load all")
        let library = try appSource("Views/LibraryView.swift")
        #expect(library.contains("else if canLoadMore, similarSeed == nil, let note = Self.moreBelowNote("))
        #expect(library.contains("Button { jumpToEnd() } label: { Text(note) }"))
        #expect(library.contains(".background(Theme.surfaceBase, in: RoundedRectangle(cornerRadius: Theme.Radius.s))"), "the pill sits over tile metadata, so its face is opaque")
    }

    @Test func rowsLoadedWithoutTagsGetTheirChipsFromTheBottomUp() throws {
        let library = try appSource("Views/LibraryView.swift")
        #expect(library.contains("private func fillTags(for ids: [Int64]"))
        #expect(library.contains("let laterIDs = newRows[min(Self.tagChunk, headCount)..<headCount]"))
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

    @Test func theLibraryToolbarHasARowLayoutForEveryKindChipAtLargerText() throws {
        let library = try appSource("Views/LibraryView.swift")
        let header = try #require(library.range(of: "private var header: some View"))
        let body = String(library[header.lowerBound...].prefix(4_000))
        #expect(body.contains("filterField\n                        Spacer(minLength: 8)"), "a last layout puts the kind chips on their own row")
        #expect(body.contains("ScrollView(.horizontal, showsIndicators: false) { kindPicker }"), "and scrolls them when even that row is too wide")
    }
}
