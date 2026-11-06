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
}
