import Testing
import FileIDShared
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

    @Test func homePathsAreAbbreviated() {
        let home = UserHome.path
        #expect(PathText.abbreviated(home + "/Pictures/Trip") == "~/Pictures/Trip")
        #expect(PathText.abbreviated(home) == "~")
        #expect(PathText.abbreviated("/Volumes/Adlon/Camera") == "/Volumes/Adlon/Camera")
        #expect(PathText.abbreviated(home + "other/x") == home + "other/x", "a sibling sharing the prefix is not home")
    }
}

@Suite("Path shortenings")
struct PathShorteningTests {
    @Test func cutsAtFolderBoundaries() {
        #expect(PathText.shortenings("/Volumes/Adlon/Camera/Eckerts 10-4-26/Photos") == [
            "/Volumes/Adlon/Camera/Eckerts 10-4-26/Photos",
            "…/Camera/Eckerts 10-4-26/Photos",
            "…/Eckerts 10-4-26/Photos",
            "…/Photos"])
    }

    @Test func shortPathsHaveNothingToDrop() {
        #expect(PathText.shortenings("/Volumes/Adlon") == ["/Volumes/Adlon", "…/Adlon"])
        #expect(PathText.shortenings("/Photos") == ["/Photos"])
        #expect(PathText.shortenings("~") == ["~"])
    }
}

@Suite("Folder tails")
struct PathTailTests {
    @Test func keepsTheDistinguishingPart() {
        #expect(PathText.tail("/Users/me/Pictures/Trips/Lisbon 2022/Day 1") == "Lisbon 2022/Day 1")
        #expect(PathText.tail("/Users/me/Downloads") == "me/Downloads")
        #expect(PathText.tail("/Downloads") == "Downloads")
        #expect(PathText.tail("/") == "/")
        #expect(PathText.tail("/a/b/c", parts: 1) == "c")
    }
}
