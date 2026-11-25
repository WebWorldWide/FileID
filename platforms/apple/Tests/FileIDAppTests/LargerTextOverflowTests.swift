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

    @Test func describeModelRowWrapsInsteadOfWideningThePage() throws {
        let deep = try appSource("Views/DeepAnalyzeViews.swift")
        let row = try #require(deep.range(of: "Text(\"AI model\").foregroundStyle(Theme.text3)"))
        #expect(deep[row.upperBound...].prefix(200).contains("FlowLayout(spacing: 8)"))
    }
}
