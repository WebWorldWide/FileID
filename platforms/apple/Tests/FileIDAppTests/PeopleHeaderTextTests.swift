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
}
