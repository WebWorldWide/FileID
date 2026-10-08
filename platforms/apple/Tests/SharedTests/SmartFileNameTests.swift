import Testing
@testable import FileIDShared

@Suite("Concise evidence names")
struct SmartFileNameTests {
    @Test func confirmedIdentityAndEvent() {
        #expect(SmartFileName.stem("alex-baseball-hit", confirmedSubjects: ["Alex"]) == "Alex - Baseball Hit")
        #expect(SmartFileName.stem("birthday-gift-opening") == "Birthday Gift Opening")
        #expect(SmartFileName.stem("roof-repair-estimate") == "Roof Repair Estimate")
    }
    @Test func abstainsAndHonorsLimits() {
        #expect(SmartFileName.stem("photo") == nil)
        #expect(SmartFileName.stem("untitled") == nil)
        #expect(SmartFileName.stem("roof-repair-estimate", originalStem: "Roof Repair Estimate") == nil)
        #expect((SmartFileName.stem(String(repeating: "longword-", count: 40))?.count ?? 1000) <= 60)
        #expect(SmartFileName.stem("baseball-hit", confirmedSubjects: ["Alex"], style: .slug) == "alex-baseball-hit")
        #expect(SmartFileName.stem("a-photo-of-baseball-hit") == "Baseball Hit")
    }
    @Test func personPrefixesPreserveSubjectsAndEventWords() {
        #expect(SmartFileName.stem("jones-beach-sunset", confirmedSubjects: ["Alex Jones"]) == "Alex Jones - Jones Beach Sunset")
        #expect(SmartFileName.stem("alex-jones-baseball-hit", confirmedSubjects: ["Alex Jones"]) == "Alex Jones - Baseball Hit")
        #expect(SmartFileName.stem("alex-and-mira-birthday-gift", confirmedSubjects: ["Alex", "Mira"]) == "Alex & Mira - Birthday Gift")
        #expect(SmartFileName.stem("alex-&-mira-birthday-gift", confirmedSubjects: ["Mira", "Alex"]) == "Alex & Mira - Birthday Gift")
        #expect(SmartFileName.stem("alex-and-sunrise", confirmedSubjects: ["Alex"]) == "Alex - And Sunrise")
        #expect(SmartFileName.stem("alex-mira-baseball-hit", confirmedSubjects: ["Alex Jones", "Mira Smith"]) == "Alex Jones & Mira Smith - Baseball Hit")
    }

}
