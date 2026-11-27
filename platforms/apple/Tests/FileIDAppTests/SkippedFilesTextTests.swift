import Foundation
import Testing
@testable import FileID

@Suite("Recent scans say what a scan left out")
struct SkippedFilesTextTests {
    @Test("each reason is named with its count, empty files first")
    func reasonsAreNamed() {
        let text = SettingsTab.skippedText(#"{"hidden":2,"empty":24,"unsupported":1,"cloudOnly":3,"folders":4}"#)
        #expect(text == "Left out: 24 empty files, 2 hidden files, 1 file of a kind FileID does not read, 3 files not downloaded from the cloud, 4 hidden, system or tool folders")
    }

    @Test("the files seen and the files in the library are stated, so they add up with what was left out")
    func seenAndCatalogued() {
        #expect(SettingsTab.seenText(#"{"seen":130108,"catalogued":130100,"empty":8}"#) == "Saw 130,108 files; 130,100 are in the library")
        #expect(SettingsTab.seenText(#"{"seen":1,"catalogued":1}"#) == "Saw 1 file; 1 is in the library")
        #expect(SettingsTab.seenText(#"{"empty":3}"#) == nil)
        #expect(SettingsTab.seenText(nil) == nil)
    }

    @Test("a scan that left nothing out, or a catalog from before the record, shows no line")
    func nothingLeftOut() {
        #expect(SettingsTab.skippedText(nil) == nil)
        #expect(SettingsTab.skippedText("{}") == nil)
        #expect(SettingsTab.skippedText("not json") == nil)
    }
}
