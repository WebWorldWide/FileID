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
