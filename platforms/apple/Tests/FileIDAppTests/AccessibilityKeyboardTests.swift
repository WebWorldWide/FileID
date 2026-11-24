import AppKit
import Foundation
import Testing
@testable import FileID

@Suite("Accessibility and keyboard")
@MainActor
struct AccessibilityKeyboardTests {
    private static func source(_ path: String) throws -> String {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<3 { root.deleteLastPathComponent() }
        return try String(contentsOf: root.appendingPathComponent("app/Sources/FileID/\(path)"), encoding: .utf8)
    }

    @Test func findMenuItemPostsTheFindNotificationOnCommandF() throws {
        let app = try Self.source("FileIDApp.swift")
        let group = try #require(app.range(of: "CommandGroup(after: .textEditing)"))
        let tail = String(app[group.upperBound...].prefix(260))
        #expect(tail.contains("Button(\"Find\")"))
        #expect(tail.contains("name: .fileIDFindInPage"))
        #expect(tail.contains(".keyboardShortcut(\"f\", modifiers: .command)"))
        #expect(Notification.Name.fileIDFindInPage != Notification.Name.fileIDFocusSearch)
    }

    @Test func theDescribePageNeverHidesTheEnginesRawMessageInAHover() throws {
        let view = try Self.source("Views/DeepAnalyzeViews.swift")
        #expect(!view.contains("(ModelFailureText.plain(error), error.message)"))
        #expect(view.components(separatedBy: "(ModelFailureText.plain(error), \"\")").count == 3)
    }

    @Test func findReachesOnlyTheVisiblePage() {
        #expect(FindRouting.accepts(pageActive: true))
        #expect(!FindRouting.accepts(pageActive: false))
    }

    @Test func searchAndMomentsFieldAnswersFind() throws {
        #expect(try Self.source("Views/CatalogWorkbench.swift").contains("respondsToFind: true"))
        #expect(try Self.source("Views/LibraryView.swift").contains("publisher(for: .fileIDFindInPage)"))
    }

    @Test(arguments: [
        "Views/PeopleView.swift",
        "Views/RestructureView.swift",
        "Views/FaceReviewViews.swift",
    ])
    func sheetsWithAPrimaryDoneAlsoCloseOnEscape(file: String) throws {
        let text = try Self.source(file)
        #expect(text.contains(".dsEscapeCancels"))
    }

    @Test func everySheetWithADoneDefaultActionHasAnEscapePath() throws {
        let people = try Self.source("Views/PeopleView.swift")
        #expect(people.components(separatedBy: ".dsEscapeCancels").count - 1 >= 2)
    }

    @Test func peopleCardsAndPhotoTilesAreButtons() throws {
        let people = try Self.source("Views/PeopleView.swift")
        #expect(!people.contains(".onTapGesture"))
        #expect(people.contains("DSBareButtonStyle(radius: Theme.Radius.m)"))
    }

    @Test func takesAndBulkRenamePrimariesAreOnReturn() throws {
        #expect(try Self.source("Views/TakeReview.swift").contains(".keyboardShortcut(pageActive ? .defaultAction : nil)"))
        #expect(try Self.source("Views/BulkRenameSheet.swift").contains(".keyboardShortcut(.defaultAction)"))
    }

    @Test func duplicateCheckboxIsLabelled() throws {
        let cleanup = try Self.source("Views/CleanupView.swift")
        #expect(cleanup.contains(".accessibilityLabel(isSelected ? \"Chosen for Trash\" : \"Choose for Trash\")"))
    }

    @Test func rescanShortcutHasOneOwnerPerScreen() throws {
        #expect(!(try Self.source("Views/Shell/TitleBar.swift")).contains("keyboardShortcut(\"r\""))
    }

    @Test func gridDrawsAGoldRingWhenItHoldsKeyboardFocus() {
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 400, height: 300))
        let grid = TileCollectionView(frame: scroll.bounds)
        scroll.documentView = grid
        let window = NSWindow(contentRect: scroll.frame, styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = scroll
        #expect(window.makeFirstResponder(grid))
        #expect(scroll.layer?.borderWidth == 2)
        #expect(window.makeFirstResponder(nil))
        #expect(scroll.layer?.borderWidth == 0)
    }

    @Test func toastStaysWhileHeldAndGoesAfterRelease() async throws {
        let toasts = ToastCenter.shared
        toasts.show("Moved 3 files", actionLabel: "Undo", duration: 0.1) {}
        toasts.hold()
        try await Task.sleep(for: .milliseconds(300))
        #expect(toasts.current?.text == "Moved 3 files")
        toasts.release()
        try await Task.sleep(for: .milliseconds(300))
        #expect(toasts.current == nil)
    }
}

@MainActor
@Suite struct ListRowVoiceOverTests {
    @Test func aListRowCanBePressedAndOpenedWithVoiceOver() throws {
        let row = ListRowView(frame: NSRect(x: 0, y: 0, width: 600, height: 56))
        var pressed = 0, opened = 0
        row.onPress = { pressed += 1 }
        row.onOpenPreview = { opened += 1 }
        #expect(row.accessibilityPerformPress())
        #expect(pressed == 1)
        let open = try #require(row.accessibilityCustomActions()?.first { $0.name == "Open preview" })
        #expect(open.handler?() == true)
        #expect(opened == 1)
    }
}
