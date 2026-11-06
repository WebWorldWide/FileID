import Foundation
import Testing
@testable import FileIDShared

@Suite("Smart name companions")
struct SmartNameCompanionsTests {
    typealias C = SmartNameCompanions

    @Test func groupsRawJpegLivePhotosAndSidecars() {
        let folder = [
            "DSC_0012.ARW", "DSC_0012.JPG", "DSC_0012.xmp", "DSC_0012.ARW.xmp", ".DSC_0012.JPG.fileid-tags.json",
            "DSC_0012.txt", "IMG_0001.HEIC", "IMG_0001.MOV", "IMG_0001.AAE", "DSC_0013.ARW", "notes.xmp", "report.pdf",
            "report.docx",
        ]
        let pair = C.members("DSC_0012.JPG", listing: folder)
        #expect(pair.map(\.name) == ["DSC_0012.JPG", "DSC_0012.ARW", "DSC_0012.ARW.xmp", "DSC_0012.xmp"])
        #expect(pair[2].suffix == ".ARW.xmp")
        #expect(C.members("DSC_0012.xmp", listing: folder).map(\.name) == pair.map(\.name))
        #expect(C.members("IMG_0001.MOV", listing: folder).map(\.name) == ["IMG_0001.HEIC", "IMG_0001.MOV", "IMG_0001.AAE"])
        #expect(C.members("DSC_0013.ARW", listing: folder).isEmpty)
        #expect(C.members("notes.xmp", listing: ["notes.xmp", "notes.aae"]).isEmpty)
        #expect(C.members("report.pdf", listing: folder).isEmpty)
        #expect(C.members("a.jpg", listing: ["A.CR3"]).map(\.name) == ["a.jpg", "A.CR3"])
        #expect(C.role("\u{212A}DC") == nil)
    }

    @Test func sharedNamesBumpTogetherOrNotAtAll() {
        let members: [(suffix: String, own: SmartNameStyle.Key)] = [(".JPG", SmartNameStyle.key("dsc_1.jpg")), (".ARW", SmartNameStyle.key("dsc_1.arw"))]
        #expect(C.sharedNames(stem: "beach", members: members, taken: [], claimed: []) == ["beach.JPG", "beach.ARW"])
        #expect(C.sharedNames(stem: "beach", members: members, taken: [SmartNameStyle.key("beach.arw")], claimed: []) == ["beach_2.JPG", "beach_2.ARW"])
        let full = Set((1...99).map { SmartNameStyle.key($0 == 1 ? "beach.arw" : "beach_\($0).arw") })
        #expect(C.sharedNames(stem: "beach", members: members, taken: full, claimed: []) == nil)
        let twins: [(suffix: String, own: SmartNameStyle.Key)] = [(".JPG", SmartNameStyle.key("a.jpg")), (".jpg", SmartNameStyle.key("a.jpg"))]
        #expect(C.sharedNames(stem: "beach", members: twins, taken: [], claimed: []) == nil)
    }

    @Test func sidecarsFollowOnlyACompleteGroup() {
        let folder = [
            "IMG_1.JPG", "IMG_1.ARW", "IMG_1.xmp", "IMG_1.ARW.XMP", "IMG_1.aae", ".IMG_1.xmp", "IMG_1.txt.xmp", "img_1.jpg.xmp",
            "IMG_2.HEIC", "IMG_2.AAE", "IMG_3.JPG", "IMG_3.xmp",
        ]
        let flat = { (found: [(owner: Int, name: String, suffix: String)]) in found.map { "\($0.owner) \($0.name) \($0.suffix)" } }
        #expect(flat(C.followingSidecars(planned: ["IMG_1.JPG", "IMG_1.ARW", "IMG_2.HEIC"], listing: folder))
                == ["0 IMG_1.xmp .xmp", "0 IMG_1.aae .aae", "0 IMG_1.ARW.XMP .ARW.XMP", "2 IMG_2.AAE .AAE"])
        #expect(C.followingSidecars(planned: ["IMG_1.JPG"], listing: folder).isEmpty)
        #expect(flat(C.followingSidecars(planned: ["IMG_3.JPG"], listing: folder)) == ["0 IMG_3.xmp .xmp"])
        #expect(C.followingSidecars(planned: ["IMG_3.JPG", "IMG_3.xmp"], listing: folder).isEmpty)
        #expect(C.followingSidecars(planned: ["IMG_3.xmp"], listing: folder).isEmpty)
    }

    @Test func evidenceRankAndReasonTags() {
        #expect(C.sourceRank("chapter", style: .short) < C.sourceRank("proposedName", style: .short))
        #expect(C.sourceRank("proposedName", style: .short) < C.sourceRank("description", style: .short))
        #expect(C.sourceRank("description", style: .descriptive) < C.sourceRank("proposedName", style: .descriptive))
        #expect(C.sourceRank("event", style: .dateFirst) < C.sourceRank("noEvidence", style: .dateFirst))
        #expect(C.groupReason("tags", leader: 7) == "tags|group:7")
        #expect(C.parse("tags|group:7") == ("tags", 7))
        #expect(C.parse("collision") == ("collision", nil))
        #expect(C.parse(nil) == ("noEvidence", nil))
    }
}
