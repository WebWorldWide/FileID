import CoreGraphics
import Foundation
import Observation
import Synchronization
import Testing
import FileIDShared
@testable import FileID

@Suite("Content text size")
struct TextSizeTests {
    @Test("Default leaves every designed size untouched")
    func defaultIsIdentity() {
        for size: CGFloat in [10, 10.5, 11, 11.5, 12, 12.5, 13, 13.5, 14, 15, 16, 20] {
            #expect(TextSize.standard.scaled(size) == size)
        }
    }

    @Test("Sizes grow in menu order and land on half points")
    func monotonicHalfPoints() {
        let factors = TextSize.allCases.map(\.factor)
        #expect(factors == factors.sorted())
        #expect(Set(factors).count == factors.count)
        for textSize in TextSize.allCases {
            for size: CGFloat in [11, 11.5, 12, 12.5, 13, 13.5, 14] {
                let scaled = textSize.scaled(size)
                #expect((scaled * 2).rounded() == scaled * 2)
            }
        }
        #expect(TextSize.small.scaled(12) == 11)
        #expect(TextSize.large.scaled(13) == 15)
        #expect(TextSize.larger.scaled(13.5) == 17.5)
    }

    @Test("Stored values round-trip; anything else falls back to Default")
    func storage() {
        for textSize in TextSize.allCases { #expect(TextSize(stored: textSize.rawValue) == textSize) }
        #expect(TextSize(stored: nil) == .standard)
        #expect(TextSize(stored: "") == .standard)
        #expect(TextSize(stored: "huge") == .standard)
        #expect(TextSize.allCases.map(\.title) == ["Small", "Default", "Large", "Larger"])
    }

    @Test("Changing the size notifies views that read it; re-setting the same size does not")
    func observation() {
        let scale = ContentTextScale(size: .standard)
        let fired = Mutex(false)
        withObservationTracking { _ = scale.size } onChange: { fired.withLock { $0 = true } }
        scale.size = .standard
        #expect(!fired.withLock { $0 })
        scale.size = .large
        #expect(fired.withLock { $0 })
        #expect(scale.size == .large)
    }

    @Test("Library tile caption keeps its designed 68 pt at Default and grows with the text")
    func tileCaption() {
        let standard = TileTextMetrics(.standard)
        #expect(standard.captionHeight == 68)
        #expect(standard.name == 17 && standard.chip == 18 && standard.meta == 15 && standard.badge == 17)
        let heights = TextSize.allCases.map { TileTextMetrics($0).captionHeight }
        #expect(heights == heights.sorted())
        for textSize in TextSize.allCases {
            let metrics = TileTextMetrics(textSize)
            #expect(metrics.name >= textSize.scaled(13) * 1.2)
            #expect(metrics.meta >= textSize.scaled(11) * 1.2)
        }
    }
}

@Suite("Scrolling list costs")
struct ListCostTests {
    @Test("One hover tracker per duplicate group finds the thumbnail under the pointer")
    func copyStripHitTest() {
        #expect(CopyStrip.thumbnailIndex(at: CGPoint(x: 0, y: 0), count: 3) == 0)
        #expect(CopyStrip.thumbnailIndex(at: CGPoint(x: 131.5, y: 131.5), count: 3) == 0)
        #expect(CopyStrip.thumbnailIndex(at: CGPoint(x: 132, y: 10), count: 3) == nil)
        #expect(CopyStrip.thumbnailIndex(at: CGPoint(x: 143.9, y: 10), count: 3) == nil)
        #expect(CopyStrip.thumbnailIndex(at: CGPoint(x: 144, y: 10), count: 3) == 1)
        #expect(CopyStrip.thumbnailIndex(at: CGPoint(x: 2 * 144 + 60, y: 60), count: 3) == 2)
        #expect(CopyStrip.thumbnailIndex(at: CGPoint(x: 3 * 144 + 10, y: 60), count: 3) == nil)
        #expect(CopyStrip.thumbnailIndex(at: CGPoint(x: 10, y: 132), count: 3) == nil)
        #expect(CopyStrip.thumbnailIndex(at: CGPoint(x: -1, y: 10), count: 3) == nil)
        #expect(CopyStrip.thumbnailIndex(at: CGPoint(x: 10, y: 10), count: 0) == nil)
    }

    @Test("Moments result rows carry their display strings")
    func momentRows() {
        let frame = CatalogHit(fileID: 1, path: "/Videos/Trip/beach.mov", kind: "sampledFrame", text: "waves", startSeconds: 65)
        let page = CatalogHit(fileID: 2, path: "/Docs/report.pdf", kind: "documentText", text: "", page: 3)
        let rows = [frame, page].enumerated().map { MomentResultRow(index: $0.offset, hit: $0.element) }
        #expect(rows.map(\.id) == [0, 1])
        #expect(rows[0].name == "beach.mov")
        #expect(rows[0].sampled)
        #expect(rows[0].meta.hasSuffix("From a video frame (not reviewed)"))
        #expect(rows[0].meta.contains(" · "))
        #expect(rows[0].text == "waves")
        #expect(rows[1].name == "report.pdf")
        #expect(!rows[1].sampled)
        #expect(rows[1].meta == "Page 3 · Text in document")
        #expect(rows == [frame, page].enumerated().map { MomentResultRow(index: $0.offset, hit: $0.element) })
    }
}
