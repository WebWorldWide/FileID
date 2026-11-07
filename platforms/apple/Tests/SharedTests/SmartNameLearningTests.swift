import Foundation
import Testing
@testable import FileIDShared

@Suite("Smart name learning")
struct SmartNameLearningTests {
    struct SampleCase: Decodable {
        let base: String
        let applied: String
        let people: [String]?
    }

    struct Expected: Decodable {
        let separator: String?
        let `case`: String?
        let leadingDate: String?
        let trailingDate: String?
        let dateFormat: String?
        let omitPeople: Bool?
        let words: Int?
        let dropWords: [String]?
    }

    struct Proposal: Decodable {
        let style: String
        let maxLength: Int
        let includePeople: Bool?
        let includeEvent: Bool?
        let includeDate: Bool?
        let original: String
        let storedName: String?
        let tags: [String]?
        let personNames: [String]?
        let people: [String]?
        let event: String?
        let date: String?
        let expected: String?
        let reason: String
        let learned: [String]?
    }

    struct Case: Decodable {
        let name: String
        let samples: [SampleCase]
        let learned: Expected
        let proposals: [Proposal]?
    }

    static func learned(_ expected: Expected) -> SmartNameLearning.Learned {
        var learned = SmartNameLearning.Learned()
        learned.separator = expected.separator
        learned.letterCase = expected.case
        learned.leadingDate = expected.leadingDate
        learned.trailingDate = expected.trailingDate
        learned.dateFormat = expected.dateFormat
        learned.omitPeople = expected.omitPeople ?? false
        learned.maxWords = expected.words
        learned.dropWords = expected.dropWords ?? []
        return learned
    }

    @Test func sharedLearningCases() throws {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let url = root.appendingPathComponent("shared/test-corpus/naming/learning-cases.json")
        let cases = try JSONDecoder().decode([Case].self, from: Data(contentsOf: url))
        #expect(cases.count >= 8)
        for item in cases {
            let samples = item.samples.map { SmartNameLearning.Sample(base: $0.base, applied: $0.applied, people: $0.people ?? []) }
            let learned = SmartNameLearning.derive(samples)
            #expect(learned == Self.learned(item.learned), "\(item.name)")
            #expect(SmartNameLearning.derive(samples.reversed()) == learned, "\(item.name) (order)")
            for proposal in item.proposals ?? [] {
                try check(proposal, learned: learned, name: item.name)
            }
        }
    }

    func check(_ proposal: Proposal, learned: SmartNameLearning.Learned, name: String) throws {
        var personWords = Set<SmartNameStyle.Key>()
        for person in proposal.personNames ?? [] { SmartNameStyle.addPersonWords(&personWords, person) }
        let evidence = SmartNameStyle.Evidence(originalStem: proposal.original, storedName: proposal.storedName, tags: proposal.tags ?? [],
                                               personWords: personWords, people: proposal.people ?? [], event: proposal.event, date: proposal.date)
        let style = try #require(SmartNameStyle.Style(rawValue: proposal.style))
        let options = SmartNameStyle.Options(style: style, maxLength: proposal.maxLength, people: proposal.includePeople ?? false,
                                             event: proposal.includeEvent ?? false, date: proposal.includeDate ?? false, learned: learned)
        let outcome = SmartNameStyle.propose(evidence, options)
        #expect(outcome.stem == proposal.expected, "\(name) / \(proposal.original)")
        #expect(outcome.reason == proposal.reason, "\(name) / \(proposal.original)")
        #expect(outcome.learned == (proposal.learned ?? []), "\(name) / \(proposal.original)")
    }

    @Test func reasonsTagAndSplit() {
        #expect(SmartNameLearning.tagged("tags", []) == "tags")
        #expect(SmartNameLearning.tagged("tags", ["case", "separator"]) == "tags|learned:case,separator")
        #expect(SmartNameLearning.splitReason("tags|learned:case,separator") == ("tags", ["case", "separator"]))
        #expect(SmartNameLearning.splitReason("collision") == ("collision", []))
    }
}
