import Foundation
import Testing
@testable import FileIDShared

@Suite("Smart name styles")
struct SmartNameStyleTests {
    struct Case: Decodable {
        let name: String
        let style: String
        let maxLength: Int
        let includePeople: Bool?
        let includeEvent: Bool?
        let includeDate: Bool?
        let original: String
        let chapter: String?
        let storedName: String?
        let description: String?
        let tags: [String]?
        let personNames: [String]?
        let people: [String]?
        let event: String?
        let date: String?
        let expected: String?
        let reason: String
    }

    @Test func sharedStyleCases() throws {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let cases = try JSONDecoder().decode([Case].self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/naming/style-cases.json")))
        #expect(cases.count >= 10)
        for item in cases {
            var personWords = Set<SmartNameStyle.Key>()
            for name in item.personNames ?? [] { SmartNameStyle.addPersonWords(&personWords, name) }
            let evidence = SmartNameStyle.Evidence(originalStem: item.original, chapter: item.chapter, storedName: item.storedName,
                                                   description: item.description, tags: item.tags ?? [], personWords: personWords,
                                                   people: item.people ?? [], event: item.event, date: item.date)
            let options = SmartNameStyle.Options(style: try #require(SmartNameStyle.Style(rawValue: item.style)), maxLength: item.maxLength,
                                                 people: item.includePeople ?? false, event: item.includeEvent ?? false, date: item.includeDate ?? false)
            let outcome = SmartNameStyle.propose(evidence, options)
            #expect(outcome.stem == item.expected, "\(item.name)")
            #expect(outcome.reason == item.reason, "\(item.name)")
        }
    }

    @Test func personRules() {
        #expect(SmartNameStyle.isUserNamed("Sam"))
        #expect(SmartNameStyle.isUserNamed("Personality"))
        #expect(SmartNameStyle.isUserNamed("Person-"))
        #expect(!SmartNameStyle.isUserNamed("Person 12"))
        #expect(!SmartNameStyle.isUserNamed("unknown_3"))
        #expect(!SmartNameStyle.isUserNamed("Person"))
        #expect(!SmartNameStyle.isUserNamed("  "))
        #expect(SmartNameStyle.personToken(first: " Sam ", legacy: "Samuel") == "sam")
        #expect(SmartNameStyle.personToken(first: nil, legacy: "Mira Smith") == "mira")
        #expect(SmartNameStyle.personToken(first: "J", legacy: nil) == nil)
        #expect(SmartNameStyle.personDisplay(title: "Dr", first: "Ana", legacy: nil) == "Dr Ana")
    }

    @Test func labelsAndNames() {
        #expect(SmartNameStyle.singleLabel(["Birthday Gift", "birthday  gift"]) == "Birthday Gift")
        #expect(SmartNameStyle.singleLabel(["Home run", "Birthday gift"]) == nil)
        #expect(SmartNameStyle.singleLabel(["Chapter 1"]) == nil)
        #expect(SmartNameStyle.splitName("IMG_1.JPG") == ("IMG_1", "JPG"))
        #expect(SmartNameStyle.splitName(".bashrc") == (".bashrc", ""))
        #expect(SmartNameStyle.splitName("name.") == ("name.", ""))
        #expect(SmartNameStyle.rawWords("Sam’s Café-day ДЕНЬ") == ["sam", "café", "day", "день"])
    }
}
