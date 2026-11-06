import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Smart-name style presentation")
struct NameStyleTests {
    private func hit(_ id: Int64, _ name: String, _ text: String, _ reason: String) -> CatalogHit {
        CatalogHit(fileID: id, path: "/photos/\(name)", kind: "proposedName", text: text, evidenceID: reason)
    }

    @Test func reasonLabelsCoverEveryEngineReason() throws {
        let expected = [
            "chapter": "From your chapter", "proposedName": "From the AI name", "description": "From the description",
            "tags": "From tags", "event": "From your event", "unchanged": "Already well named",
            "budget": "Too long for the length limit", "reserved": "Not allowed as a file name",
            "collision": "No free name in this folder", "noEvidence": "Nothing to name it from yet",
        ]
        for (reason, label) in expected { #expect(NameStyle.reasonLabel(reason) == label) }
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let data = try Data(contentsOf: root.appendingPathComponent("shared/test-corpus/naming/style-cases.json"))
        let cases = try #require(JSONSerialization.jsonObject(with: data) as? [[String: Any]])
        for reason in cases.compactMap({ $0["reason"] as? String }) { #expect(expected[reason] != nil, "\(reason)") }
    }

    @Test func defaultsAndRequest() {
        #expect(NameStyle.defaultStyle == "short" && NameStyle.defaultMaxLength == 40)
        #expect(NameStyle.defaultIncludePeople && NameStyle.defaultIncludeEvent && !NameStyle.defaultIncludeDate)
        let naming = NameStyle.naming(style: "descriptive", maxLength: 500, includePeople: true, includeEvent: false, includeDate: true)
        #expect(naming == CatalogNaming(style: "descriptive", maxLength: 120, includePeople: true, includeEvent: false, includeDate: true))
        #expect(NameStyle.naming(style: "loud", maxLength: 1, includePeople: false, includeEvent: false, includeDate: false)
                == CatalogNaming(style: "short", maxLength: 4))
        let request = NameStyle.request(id: "r1", fileIDs: [3, 4], naming: naming)
        #expect(request.action == "proposeNames" && request.fileIDs == [3, 4] && request.naming == naming)
    }

    @Test func dateFirstDisablesAddDate() {
        #expect(!NameStyle.dateToggleEnabled(style: "dateFirst"))
        #expect(NameStyle.dateToggleEnabled(style: "short") && NameStyle.dateToggleEnabled(style: "descriptive"))
        #expect(!NameStyle.naming(style: "dateFirst", maxLength: 40, includePeople: true, includeEvent: true, includeDate: true).includeDate)
    }

    @Test func fileIDsPreferTheSelectionAndCapAtOneThousand() {
        #expect(NameStyle.fileIDs(selection: [5, 5, 0, 6], fallback: [9]) == [5, 6])
        #expect(NameStyle.fileIDs(selection: [], fallback: [9, 8]) == [9, 8])
        #expect(NameStyle.fileIDs(selection: Array(1...1_500), fallback: []).count == 1_000)
    }

    @Test func onlyRowsWithTextCanBeCheckedAndUntickedRowsStayUnticked() {
        let hits = [hit(1, "IMG_0001.jpg", "sam-baseball-hit.jpg", "proposedName"), hit(2, "Beach.jpg", "", "unchanged"),
                    CatalogHit(fileID: 3, path: "/x", kind: "keyword", text: "nope")]
        var rows = NameStyle.rows(from: hits, previous: [])
        #expect(rows.map(\.fileID) == [1, 2])
        #expect(rows[0].canCheck && rows[0].checked)
        #expect(!rows[1].canCheck && !rows[1].checked)
        rows[0].checked = false
        let again = NameStyle.rows(from: hits, previous: rows)
        #expect(!again[0].checked)
    }

    @Test func renamesSendTheEditedTextForCheckedRowsOnly() {
        var rows = NameStyle.rows(from: [hit(1, "IMG_0001.jpg", "sam-baseball-hit.jpg", "proposedName"),
                                         hit(2, "IMG_0002.jpg", "beach.jpg", "tags"),
                                         hit(3, "IMG_0003.jpg", "", "budget"),
                                         hit(4, "IMG_0004.jpg", "cake.jpg", "description"),
                                         hit(5, "IMG_0005.jpg", "dog.jpg", "tags")], previous: [])
        rows[0].edited = "  sam-home-run.jpg "
        rows[1].checked = false
        rows[2].checked = true
        rows[3].edited = "bad/name.jpg"
        rows[4].edited = "IMG_0005.jpg"
        #expect(NameStyle.renames(rows) == [CatalogWrites.NamedRename(fileID: 1, newName: "sam-home-run.jpg")])
    }

    @Test func voiceOverLabelReadsNameTargetAndReason() {
        let rows = NameStyle.rows(from: [hit(1, "IMG_0001.jpg", "sam-baseball-hit.jpg", "proposedName"),
                                         hit(2, "Beach.jpg", "", "unchanged")], previous: [])
        #expect(NameStyle.accessibilityLabel(rows[0]) == "IMG_0001.jpg, rename to sam-baseball-hit.jpg, from the AI name")
        #expect(NameStyle.accessibilityLabel(rows[1]) == "Beach.jpg, keeps its name, already well named")
    }

    private func pair() -> [CatalogHit] {
        [hit(11, "DSC_1.JPG", "beach-sunset_2.JPG", "proposedName|group:11"),
         hit(10, "DSC_1.ARW", "beach-sunset_2.ARW", "proposedName|group:11"),
         hit(12, "DSC_1.ARW.xmp", "beach-sunset_2.ARW.xmp", "proposedName|group:11"),
         hit(14, "DSC_3.ARW", "lone-hill.ARW", "proposedName")]
    }

    @Test func companionsArriveTogetherLeaderFirst() {
        let rows = NameStyle.rows(from: pair(), previous: [])
        #expect(rows.map(\.fileID) == [11, 10, 12, 14])
        #expect(rows.map(\.reason) == ["proposedName", "proposedName", "proposedName", "proposedName"])
        #expect(rows[0].isLeader && rows[1].isFollower && rows[2].isFollower && rows[3].group == nil)
        #expect(rows.map(\.suffix) == [".JPG", ".ARW", ".ARW.xmp", ""])
        #expect(NameStyle.companionChip(rows[0], in: rows) == "+ ARW, XMP")
        #expect(NameStyle.companionChip(rows[1], in: rows) == nil && NameStyle.companionChip(rows[3], in: rows) == nil)
        #expect(NameStyle.reasonLabel("unscannedCompanion") == "Its partner file isn't scanned yet")
        #expect(NameStyle.accessibilityLabel(rows[1], in: rows)
                == "DSC_1.ARW, rename to beach-sunset_2.ARW, from the AI name, together with DSC_1.JPG, DSC_1.ARW.xmp")
    }

    @Test func tickingTheLeaderTicksTheWholeGroupAndStaysUnticked() {
        var rows = NameStyle.rows(from: pair(), previous: [])
        NameStyle.setChecked(&rows, fileID: 11, false)
        #expect(rows.map(\.checked) == [false, false, false, true])
        #expect(NameStyle.rows(from: pair(), previous: rows).map(\.checked) == [false, false, false, true])
        NameStyle.setChecked(&rows, fileID: 11, true)
        #expect(rows.allSatisfy { $0.checked })
    }

    @Test func editingTheLeaderGivesEveryCompanionTheSameStem() {
        var rows = NameStyle.rows(from: pair(), previous: [])
        NameStyle.edit(&rows, fileID: 11, " sunset-walk.JPG ")
        #expect(rows.map(\.edited) == [" sunset-walk.JPG ", "sunset-walk.ARW", "sunset-walk.ARW.xmp", "lone-hill.ARW"])
        NameStyle.edit(&rows, fileID: 10, "rogue.ARW")
        #expect(rows[1].edited == "sunset-walk.ARW")
        #expect(NameStyle.renames(rows) == [
            CatalogWrites.NamedRename(fileID: 11, newName: "sunset-walk.JPG", group: 11),
            CatalogWrites.NamedRename(fileID: 10, newName: "sunset-walk.ARW", group: 11),
            CatalogWrites.NamedRename(fileID: 12, newName: "sunset-walk.ARW.xmp", group: 11),
            CatalogWrites.NamedRename(fileID: 14, newName: "lone-hill.ARW"),
        ])
        NameStyle.edit(&rows, fileID: 11, "bad/name.JPG")
        #expect(NameStyle.renames(rows) == [CatalogWrites.NamedRename(fileID: 14, newName: "lone-hill.ARW")])
    }

    @Test func applyBatchesNeverSplitAGroup() {
        let renames = [CatalogWrites.NamedRename(fileID: 1, newName: "a"),
                       CatalogWrites.NamedRename(fileID: 2, newName: "b", group: 2),
                       CatalogWrites.NamedRename(fileID: 3, newName: "b", group: 2),
                       CatalogWrites.NamedRename(fileID: 4, newName: "c", group: 4),
                       CatalogWrites.NamedRename(fileID: 5, newName: "c", group: 4),
                       CatalogWrites.NamedRename(fileID: 6, newName: "c", group: 4)]
        #expect(CatalogWrites.groupedChunks(renames, size: 2).map { $0.map(\.fileID) } == [[1], [2, 3], [4, 5, 6]])
        #expect(CatalogWrites.groupedChunks(renames, size: 250).map { $0.map(\.fileID) } == [[1, 2, 3, 4, 5, 6]])
    }
}
