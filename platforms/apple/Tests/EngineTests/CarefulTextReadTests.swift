import Foundation
import CoreGraphics
import CoreText
import Testing
@testable import FileIDEngine

@Suite("Careful text read languages")
struct CarefulTextReadTests {
    typealias Line = CarefulTextRead.Line

    @Test func coverageCountsTheShareInsideOtherBoxes() {
        let box = CGRect(x: 0.1, y: 0.1, width: 0.4, height: 0.1)
        #expect(CarefulTextRead.coverage(of: box, by: []) == 0)
        #expect(CarefulTextRead.coverage(of: box, by: [CGRect(x: 0, y: 0, width: 1, height: 1)]) == 1)
        #expect(CarefulTextRead.coverage(of: box, by: [CGRect(x: 0.1, y: 0.1, width: 0.2, height: 0.1)]) == 0.5)
    }

    @Test func unreadRegionsAreUncoveredTextAndUnsureLatinLines() {
        let read = Line(text: "Exit", confidence: 1, box: CGRect(x: 0.1, y: 0.3, width: 0.1, height: 0.04))
        let invented = Line(text: "al aka Eati", confidence: 0.3, box: CGRect(x: 0.1, y: 0.4, width: 0.3, height: 0.05))
        let chinese = CGRect(x: 0.1, y: 0.6, width: 0.4, height: 0.06)
        let distant = CGRect(x: 0.7, y: 0.8, width: 0.05, height: 0.01)
        let regions = CarefulTextRead.unreadRegions(textBoxes: [read.box, chinese, distant], latin: [read, invented])
        #expect(regions == [chinese, invented.box])
        #expect(CarefulTextRead.unreadRegions(textBoxes: [read.box, distant], latin: [read]).isEmpty)
    }

    @Test func fallbackRegionWidensAlongTheLineAndStaysInTheImage() throws {
        #expect(CarefulTextRead.fallbackRegion([]) == nil)
        let region = try #require(CarefulTextRead.fallbackRegion([CGRect(x: 0.4, y: 0.5, width: 0.1, height: 0.05)]))
        #expect(abs(region.minX - 0.2) < 1e-9 && abs(region.maxX - 0.7) < 1e-9)
        #expect(abs(region.minY - 0.475) < 1e-9 && abs(region.maxY - 0.575) < 1e-9)
        let edge = try #require(CarefulTextRead.fallbackRegion([CGRect(x: 0, y: 0.97, width: 0.9, height: 0.03)]))
        #expect(edge.minX == 0 && edge.maxX == 1 && edge.maxY == 1)
    }

    @Test func pixelRectFlipsToTopLeftOrigin() {
        let rect = CarefulTextRead.pixelRect(CGRect(x: 0.25, y: 0.5, width: 0.5, height: 0.25), width: 400, height: 200)
        #expect(rect == CGRect(x: 100, y: 50, width: 200, height: 50))
    }

    @Test func mergeAddsNewLinesAndReplacesInventedOnes() {
        let exit = Line(text: "Exit", confidence: 1, box: CGRect(x: 0.1, y: 0.3, width: 0.1, height: 0.04))
        let invented = Line(text: "al aka Eati", confidence: 0.3, box: CGRect(x: 0.1, y: 0.4, width: 0.3, height: 0.05))
        let arabic = Line(text: "المخرج على اليسار", confidence: 1, box: CGRect(x: 0.09, y: 0.39, width: 0.32, height: 0.06))
        let exitAgain = Line(text: "Exit", confidence: 1, box: exit.box)
        let merged = CarefulTextRead.merge(latin: [exit, invented], fallback: [arabic, exitAgain])
        #expect(merged.map(\.text) == ["Exit", "المخرج على اليسار"])
        let unsureKept = CarefulTextRead.merge(latin: [exit, invented], fallback: [])
        #expect(unsureKept.map(\.text) == ["Exit", "al aka Eati"])
        let latinGuess = Line(text: "al aka East", confidence: 1, box: invented.box)
        #expect(CarefulTextRead.merge(latin: [exit, invented], fallback: [latinGuess]).map(\.text) == ["Exit", "al aka Eati"])
    }

    @Test func otherScriptsAreThoseOutsideLatinGreekAndCyrillic() {
        for text in ["Exit", "Добро пожаловать", "Ελλάδα", "Tiếng Việt", "piñata 2026"] {
            #expect(!CarefulTextRead.hasOtherScript(text), "\(text)")
        }
        for text in ["会议记录", "ありがとう", "감사합니다", "ممنوع التدخين", "श्री कृष्ण", "שלום", "สวัสดี"] {
            #expect(CarefulTextRead.hasOtherScript(text), "\(text)")
        }
    }

    /// Opt-in like the other accurate-tier tests (model compile, Neural Engine).
    @Test(.enabled(if: ProcessInfo.processInfo.environment["FILEID_OCR_TESTS"] == "1"))
    func latinFirstStillReadsChineseBesideEnglish() async throws {
        let ctx = try #require(CGContext(data: nil, width: 1200, height: 400, bitsPerComponent: 8, bytesPerRow: 0,
                                         space: CGColorSpaceCreateDeviceRGB(),
                                         bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        ctx.setFillColor(CGColor(gray: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: 1200, height: 400))
        for (text, font, y) in [("Quarterly report 2026", "Helvetica", 260.0), ("会议记录：周三下午三点", "PingFang SC", 100.0)] {
            let attributed = NSAttributedString(string: text, attributes: [
                NSAttributedString.Key(kCTFontAttributeName as String): CTFontCreateWithName(font as CFString, 64, nil),
                NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(gray: 0, alpha: 1)])
            ctx.textPosition = CGPoint(x: 60, y: y)
            CTLineDraw(CTLineCreateWithAttributedString(attributed), ctx)
        }
        let image = try #require(ctx.makeImage())
        let text = try #require(await CarefulTextRead.read(image, languages: .latinFirst))
        #expect(text.contains("Quarterly report 2026"))
        #expect(text.contains("周三下午三点"))
    }
}
