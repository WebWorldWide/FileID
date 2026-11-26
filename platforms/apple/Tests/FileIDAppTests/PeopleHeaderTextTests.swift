import Testing
@testable import FileID

@Suite("People header text")
struct PeopleHeaderTextTests {
    @Test func headerSaysGroupingIsRunningWhilePeopleAlreadyExist() {
        #expect(PeopleHeaderText.countLine(people: 3, unnamed: 2, faces: 40, grouping: true) == "3 people · 2 still unnamed · still grouping faces")
        #expect(PeopleHeaderText.countLine(people: 3, unnamed: 0, faces: 40, grouping: true) == "3 people · all named · still grouping faces")
        #expect(PeopleHeaderText.countLine(people: 3, unnamed: 0, faces: 40, grouping: false) == "3 people · all named")
    }

    @Test func headerSaysFacesAreWaitingOrBeingGroupedBeforeAnyPersonExists() {
        #expect(PeopleHeaderText.countLine(people: 0, unnamed: 0, faces: 12, grouping: true) == "12 faces · grouping…")
        #expect(PeopleHeaderText.countLine(people: 0, unnamed: 0, faces: 12, grouping: false) == "12 faces detected · not grouped yet")
        #expect(PeopleHeaderText.countLine(people: 0, unnamed: 0, faces: 0, grouping: true) == "")
    }

    @Test func theEmptyStateDoesNotTellYouToPressGroupFacesWhileGroupingRuns() {
        #expect(!PeopleHeaderText.ungroupedMessage(grouping: true).contains("Press"))
        #expect(PeopleHeaderText.ungroupedMessage(grouping: false).contains("Press Group faces"))
    }

    @Test func theModelPanelSaysGroupedPeopleStayWhenOnlyNewFacesNeedTheModel() {
        #expect(PeopleHeaderText.modelPanelTitle(hasPeople: true).contains("new faces"))
        #expect(PeopleHeaderText.modelPanelTitle(hasPeople: true).contains("stay"))
        #expect(PeopleHeaderText.modelPanelTitle(hasPeople: false).contains("Download"))
    }

    @Test func groupedPeopleStayVisibleWhenTheFaceModelIsMissing() {
        #expect(PeopleContent.resolve(modelInstalled: false, people: 1_420, faces: 89_000) == .people(modelMissing: true))
        #expect(PeopleContent.resolve(modelInstalled: true, people: 3, faces: 40) == .people(modelMissing: false))
        #expect(PeopleContent.resolve(modelInstalled: false, people: 0, faces: 40) == .modelMissing)
        #expect(PeopleContent.resolve(modelInstalled: true, people: 0, faces: 40) == .ungrouped)
        #expect(PeopleContent.resolve(modelInstalled: true, people: 0, faces: 0) == .empty)
    }
}
