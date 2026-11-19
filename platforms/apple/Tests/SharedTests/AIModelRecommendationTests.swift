import Foundation
import Testing
@testable import FileIDShared
@testable import FileIDModelInfo

@Suite("AI model hardware recommendation")
struct AIModelRecommendationTests {
    @Test func eightGBSelectsRunnableQwen3() {
        let choice = AIModelKind.safeDefaultFor(ramGB: 8)
        #expect(choice == .qwen3VL4B)
        #expect(choice.fits(ramGB: 8))
    }

    @Test func sixteenGBSelectsQwen3EightB() {
        #expect(AIModelKind.safeDefaultFor(ramGB: 16) == .qwen3VL8B)
    }

    @Test func nominalThirtyTwoGBSelectsMistral() {
        #expect(AIModelKind.safeDefaultFor(ramGB: 30.9) == .mistralSmall32)
    }

    @Test func diskPressureDowngradesBeforeDownloading() {
        let qwenOnly = AIModelKind.qwen2VL7B.requiredFreeBytes + 1
        #expect(qwenOnly < AIModelKind.mistralSmall32.requiredFreeBytes)
        #expect(AIModelKind.safeDefaultFor(ramGB: 30.9,
                                          freeDiskBytes: qwenOnly) == .qwen3VL4B)
    }

    @Test func onboardingPreservesAnExplicitExistingChoice() {
        let selected = AIModelKind.onboardingSelection(
            persistedRawValue: AIModelKind.gemma3_4B.rawValue,
            ramGB: 30.9,
            freeDiskBytes: Int64.max)
        #expect(selected == .gemma3_4B)
    }

    @Test func fitIsTheEnginesAdmissionBudgetNotAFractionOfMemory() {
        #expect(AIModelKind.qwen3VL4B.fits(ramGB: 8))
        #expect(!AIModelKind.qwen3VL8B.fits(ramGB: 8))
        #expect(AIModelKind.qwen3VL8B.fits(ramGB: 16))
        #expect(AIModelKind.gemma3_12B.fits(ramGB: 16))
        #expect(!AIModelKind.mistralSmall32.fits(ramGB: 16))
        #expect(AIModelKind.mistralSmall32.fits(ramGB: 30.9))
    }

    @Test func aBusyMacGetsTheModelThatLoadsNow() {
        #expect(AIModelKind.safeDefaultFor(ramGB: 16, availableMB: 13_000) == .qwen3VL8B)
        #expect(AIModelKind.safeDefaultFor(ramGB: 16, availableMB: 9_500) == .qwen3VL4B)
        #expect(AIModelKind.safeDefaultFor(ramGB: 16, availableMB: 4_500) == .qwen3VL2B)
        #expect(AIModelKind.safeDefaultFor(ramGB: 8, availableMB: 7_000) == .qwen3VL4B)
        #expect(AIModelKind.safeDefaultFor(ramGB: 8, availableMB: 4_000) == .qwen3VL2B)
        #expect(AIModelKind.safeDefaultFor(ramGB: 8, availableMB: 1_000) == .qwen3VL2B)
    }

    @Test func theTwoBillionModelIsOfferedOnEightGBMacs() {
        #expect(AIModelKind.recommendedFor(ramGB: 8).contains(.qwen3VL2B))
        #expect(AIModelKind.recommendedFor(ramGB: 16).contains(.qwen3VL2B))
    }

    @Test func recommendationFollowsTheSharedFixture() throws {
        struct Candidate: Decodable { let id: String; let requestedMB: UInt64 }
        struct Case: Decodable { let totalMB: UInt64; let availableMB: UInt64; let ladder: [Candidate]; let expected: String? }
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let cases = try JSONDecoder().decode([Case].self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/model-recommendation.json")))
        #expect(!cases.isEmpty)
        for entry in cases {
            let ladder = entry.ladder.map { (id: $0.id, requestedMB: $0.requestedMB) }
            #expect(ModelMemoryRule.recommend(ladder: ladder, totalMB: entry.totalMB, availableMB: entry.availableMB) == entry.expected)
        }
    }
}
