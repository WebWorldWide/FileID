import AppKit
import SwiftUI
import Testing
@testable import FileID

@Suite struct LargerTextOverflowTests {
    @Test func everyScrollViewNamesItsAxis() throws {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let views = root.appendingPathComponent("platforms/apple/app/Sources/FileID")
        let files = try #require(FileManager.default.enumerator(at: views, includingPropertiesForKeys: nil))
        var bare: [String] = []
        for case let url as URL in files where url.pathExtension == "swift" {
            let text = try String(contentsOf: url, encoding: .utf8)
            if text.range(of: #"(?<![A-Za-z])ScrollView \{"#, options: .regularExpression) != nil { bare.append(url.lastPathComponent) }
        }
        #expect(bare.isEmpty, "A ScrollView with no axis scrolls sideways too and is offered no width, so one row that does not wrap widens the whole page past the window: \(bare)")
    }

    @Test func duplicatesControlsWrapWhenTheyDoNotFitBesideTheSummary() throws {
        let cleanup = try appSource("Views/CleanupView.swift")
        let header = try #require(cleanup.range(of: "private func header(visible"))
        let body = String(cleanup[header.lowerBound...].prefix(6500))
        #expect(body.contains("FlowLayout(spacing: 8) { controls }"))
        #expect(!body.contains("HStack(alignment: .center, spacing: 12) { controls }"))
    }

    @Test func suggestedMergeRowKeepsItsMergeButtonWholeAtLargerText() throws {
        let people = try appSource("Views/PeopleView.swift")
        let row = try #require(people.range(of: "private func row(for cand: MergeSuggestionPair)"))
        let body = String(people[row.lowerBound...].prefix(2200))
        #expect(body.contains("                .fixedSize()\n            }\n            .padding(10)"))
        let card = try #require(people.range(of: "private func miniCard("))
        #expect(people[card.lowerBound...].prefix(1200).contains(".frame(minWidth: 120, maxWidth: 170, alignment: .leading)"))
    }

    @Test func describeModelRowWrapsInsteadOfWideningThePage() throws {
        let deep = try appSource("Views/DeepAnalyzeViews.swift")
        let row = try #require(deep.range(of: "Text(\"AI model\").foregroundStyle(Theme.text3)"))
        #expect(deep[row.upperBound...].prefix(200).contains("FlowLayout(spacing: 8)"))
    }

    @Test func panelHeaderRuleSpansThePanelWhenTheHeaderStacks() throws {
        let design = try appSource("Theme/DesignSystem.swift")
        let header = try #require(design.range(of: "struct PanelHeader<Trailing"))
        let body = String(design[header.lowerBound...].prefix(1100))
        let frame = try #require(body.range(of: ".frame(maxWidth: .infinity, alignment: .leading)"))
        let rule = try #require(body.range(of: ".overlay(alignment: .bottom) { Hairline() }"))
        #expect(frame.lowerBound < rule.lowerBound)
    }

    @Test func chatSourcesHeaderFollowsTheConversationHeaderHeight() throws {
        let chat = try appSource("Views/ChatWorkbench.swift")
        #expect(chat.contains("ChatPaneHeaderHeightKey"))
        #expect(chat.contains(".frame(height: max(Theme.Metrics.paneHeaderHeight, pageHeaderHeight), alignment: .top)"))
        #expect(!chat.contains("TagChip(text: \"Names\").hidden()"))
    }

    @MainActor
    @Test func anOptionRowPutsItsLabelAboveTheControlsOnlyWhenBothDoNotFit() {
        func height(width: CGFloat) -> CGFloat {
            let view = OptionRowLayout(labelColumn: 100) {
                Text("Label")
                ScrollView(.horizontal) { Color.clear.frame(width: 200, height: 20) }
                Text("A note").fixedSize(horizontal: false, vertical: true)
            }
            return NSHostingController(rootView: view).sizeThatFits(in: CGSize(width: width, height: 1000)).height
        }
        let beside = height(width: 400)
        let stacked = height(width: 250)
        #expect(stacked > beside + 8)
    }

    @Test func theDescribeButtonLooksDisabledWhenAnOfflineFileDisablesIt() throws {
        let deep = try appSource("Views/DeepAnalyzeViews.swift")
        let button = try #require(deep.range(of: "struct DeepAnalyzeButton"))
        let body = String(deep[button.lowerBound...].prefix(3200))
        #expect(body.contains("@Environment(\\.isEnabled) private var enabled"))
        #expect(body.contains("fits && enabled ? Theme.gold"))
        let library = try appSource("Views/LibraryView.swift")
        #expect(library.contains("descriptions are kept.\")\n                        .multilineTextAlignment(.center).padding(.horizontal, 28)"))
    }
}
