import Foundation
import Testing
@testable import FileID

@Suite("Recent scans say what a scan left out")
struct SkippedFilesTextTests {
    @Test("each reason is named with its count, empty files first")
    func reasonsAreNamed() {
        let text = SettingsTab.skippedText(#"{"hidden":2,"empty":24,"unsupported":1,"cloudOnly":3}"#)
        #expect(text == "Left out: 24 empty files, 2 hidden files or folders, 1 file of a kind FileID does not read, 3 files not downloaded from the cloud")
    }

    @Test("a scan that left nothing out, or a catalog from before the record, shows no line")
    func nothingLeftOut() {
        #expect(SettingsTab.skippedText(nil) == nil)
        #expect(SettingsTab.skippedText("{}") == nil)
        #expect(SettingsTab.skippedText("not json") == nil)
    }
}
