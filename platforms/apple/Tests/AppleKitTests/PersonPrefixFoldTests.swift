import Testing
@testable import FileIDVLMCore

@Suite("Person-name prefix folds accents")
struct PersonPrefixFoldTests {
    @Test func aPersonNameWithAccentsIsPrefixedFoldedNotDropped() {
        #expect(DeepAnalyze.applyPersonPrefix("beach-sunset", faceNames: ["José García"]) == "jose-beach-sunset")
        #expect(DeepAnalyze.applyPersonPrefix("beach-sunset", faceNames: ["Zoe\u{308} Lund"]) == "zoe-beach-sunset")
        #expect(DeepAnalyze.applyPersonPrefix("beach-sunset", faceNames: ["Łukasz", "Søren", "Ærin"]) == "aerin-lukasz-soren-beach-sunset")
        #expect(DeepAnalyze.applyPersonPrefix("jose-at-beach", faceNames: ["José"]) == "jose-at-beach")
        #expect(DeepAnalyze.applyPersonPrefix("beach", faceNames: ["Müller", "Muller"]) == "muller-beach")
        #expect(DeepAnalyze.applyPersonPrefix("beach", faceNames: ["東京"]) == "beach")
    }
}
