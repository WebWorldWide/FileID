import Testing
@testable import FileIDVLMCore

@Suite("Text-only filename fallback")
struct DocumentKeywordsFilenameTests {
    @Test func accentedWordsAreFoldedNotSplitIntoFragments() {
        let name = DocumentKeywords.groundedFilename(from: "Zürich Flughafen Ticket confirmation. Résumé attached.")
        #expect(name != nil)
        #expect(name?.contains("zurich") == true)
        #expect(name?.contains("z-rich") == false)
        #expect(name?.contains("r-sum") == false)
    }

    @Test func wordsThatCannotBecomePlainLettersAreLeftOutRatherThanGarbled() {
        let name = DocumentKeywords.groundedFilename(from: "東京 Invoice March Statement")
        #expect(name?.unicodeScalars.allSatisfy { $0.isASCII } == true)
    }
}
