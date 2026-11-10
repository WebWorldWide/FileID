import Foundation
import CoreGraphics
import CoreText
import Testing
@testable import FileIDEngine

@Suite("Careful text read languages")
struct CarefulTextReadTests {
    @Test func onlyTheLatinValueTurnsOffLanguageDetection() {
        #expect(CarefulTextRead.languages(from: nil) == .automatic)
        #expect(CarefulTextRead.languages(from: "") == .automatic)
        #expect(CarefulTextRead.languages(from: "Latin") == .automatic)
        #expect(CarefulTextRead.languages(from: "latin") == .latin)
    }

    /// Opt-in like the other accurate-tier tests (model compile, Neural Engine).
    @Test(.enabled(if: ProcessInfo.processInfo.environment["FILEID_OCR_TESTS"] == "1"))
    func latinReadsKeepEnglishAndCyrillicAndOnlyDetectionReadsChinese() async throws {
        let ctx = try #require(CGContext(data: nil, width: 1200, height: 500, bitsPerComponent: 8, bytesPerRow: 0,
                                         space: CGColorSpaceCreateDeviceRGB(),
                                         bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        ctx.setFillColor(CGColor(gray: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: 1200, height: 500))
        let lines = [("Quarterly report 2026", "Helvetica", 360.0), ("Вход только для персонала", "Helvetica", 220.0),
                     ("会议记录：周三下午三点", "PingFang SC", 80.0)]
        for (text, font, y) in lines {
            let attributed = NSAttributedString(string: text, attributes: [
                NSAttributedString.Key(kCTFontAttributeName as String): CTFontCreateWithName(font as CFString, 60, nil),
                NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(gray: 0, alpha: 1)])
            ctx.textPosition = CGPoint(x: 60, y: y)
            CTLineDraw(CTLineCreateWithAttributedString(attributed), ctx)
        }
        let image = try #require(ctx.makeImage())
        let latin = try #require(await CarefulTextRead.read(image, languages: .latin))
        #expect(latin.contains("Quarterly report 2026"))
        #expect(latin.contains("Вход только для персонала"))
        #expect(!latin.contains("周三"))
        let automatic = try #require(await CarefulTextRead.read(image, languages: .automatic))
        #expect(automatic.contains("周三下午三点"))
    }
}
