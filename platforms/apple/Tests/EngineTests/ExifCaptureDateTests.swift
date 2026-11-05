import Foundation
import Testing
@testable import FileIDEngine

@Suite("EXIF capture date")
struct ExifCaptureDateTests {
    private let now = Date(timeIntervalSince1970: 1_900_000_000)

    @Test func readsOffsetsAndRejectsPlaceholders() {
        #expect(Tagging.exifCaptureDate("2019:07:04 12:00:00", offset: "+00:00", now: now)?.timeIntervalSince1970 == 1_562_241_600)
        #expect(Tagging.exifCaptureDate("2019:07:04 14:00:00", offset: "+02:00", now: now)?.timeIntervalSince1970 == 1_562_241_600)
        #expect(Tagging.exifCaptureDate("2019:07:04 07:00:00", offset: "-05:00", now: now)?.timeIntervalSince1970 == 1_562_241_600)
        #expect(Tagging.exifCaptureDate("2019:07:04 12:00:00", offset: nil, now: now) != nil)
        #expect(Tagging.exifCaptureDate("0000:00:00 00:00:00", offset: nil, now: now) == nil)
        #expect(Tagging.exifCaptureDate("1980:01:01 00:00:00", offset: "+00:00", now: now) == nil)
        #expect(Tagging.exifCaptureDate("2099:01:01 00:00:00", offset: "+00:00", now: now) == nil)
        #expect(Tagging.exifCaptureDate("    :  :     :  :  ", offset: nil, now: now) == nil)
        #expect(Tagging.exifSubsecFraction("5") == 0.5)
        #expect(Tagging.exifSubsecFraction("123") == 0.123)
        #expect(Tagging.exifSubsecFraction(" 07\0") == 0.07)
        #expect(Tagging.exifSubsecFraction("") == 0)
        #expect(Tagging.exifSubsecFraction(nil) == 0)
        #expect(Tagging.exifSubsecFraction("ab") == 0)
    }
}
