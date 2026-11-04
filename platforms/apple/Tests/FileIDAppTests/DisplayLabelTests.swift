import Testing
@testable import FileID

@Suite("Display labels")
struct DisplayLabelTests {
    @Test func engineTagsReadAsWords() {
        #expect(TagLabel.text("Year_2026") == "2026")
        #expect(TagLabel.text("Camera_Sony") == "Sony")
        #expect(TagLabel.text("grass") == "Grass")
        #expect(TagLabel.text("hot-air-balloon") == "Hot air balloon")
        #expect(TagLabel.text("Kyle at bat") == "Kyle at bat")
    }

    @Test func duplicatesCollapseCaseInsensitively() {
        #expect(TagLabel.unique(["grass", "Grass", "Year_2026", "land"]) == ["Grass", "2026", "Land"])
    }

    @Test func pdfIsUppercase() {
        #expect(FileKindLabel.title("pdf") == "PDF")
        #expect(FileKindLabel.title("image") == "Image")
    }
}
