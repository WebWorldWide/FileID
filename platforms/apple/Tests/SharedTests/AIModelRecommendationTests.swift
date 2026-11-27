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

    @Test func theHundredBillionModelTopsTheLadderOnlyWhereItFits() {
        #expect(AIModelKind.safeDefaultFor(ramGB: 64) == .mistralSmall32)
        #expect(AIModelKind.safeDefaultFor(ramGB: 96) == .qwen35_122B)
        #expect(AIModelKind.safeDefaultFor(ramGB: 128) == .qwen35_122B)
        #expect(AIModelKind.safeDefaultFor(ramGB: 192) == .qwen35_122B)
        #expect(!AIModelKind.qwen35_122B.fits(ramGB: 64))
        #expect(AIModelKind.qwen35_122B.fits(ramGB: 96))
        #expect(AIModelKind.recommendedFor(ramGB: 64).first == .mistralSmall32)
        #expect(AIModelKind.recommendedFor(ramGB: 96).first == .qwen35_122B)
    }

    @Test func aBusyBigMacFallsBackToTheModelThatLoadsNow() {
        #expect(AIModelKind.safeDefaultFor(ramGB: 128, availableMB: 100_000) == .qwen35_122B)
        #expect(AIModelKind.safeDefaultFor(ramGB: 128, availableMB: 40_000) == .mistralSmall32)
        #expect(AIModelKind.safeDefaultFor(ramGB: 96, availableMB: 50_000) == .mistralSmall32)
        #expect(AIModelKind.safeDefaultFor(ramGB: 96, availableMB: 15_000) == .qwen3VL8B)
    }

    @Test func theBigModelIsATrackedFavouriteOfTheCatalog() throws {
        let entry = try #require(AIModelKind.qwen35_122B.catalogEntry)
        #expect(entry.role == .vision && entry.architecture == "qwen3_5_moe" && entry.license == "apache-2.0")
        #expect(entry.bases.first?.repo == "Qwen/Qwen3.5-122B-A10B")
        #expect(AIModelKind.qwen35_122B.sourceRepo == entry.repo)
        #expect(AIModelKind.qwen35_122B.licensePolicyKey == "Apache-2.0")
        #expect(AIModelKind.qwen35_122B.licenseTermsURL == nil)
    }

    @Test func diskPressureDowngradesBeforeDownloading() {
        let qwenOnly = AIModelKind.qwen2VL7B.requiredFreeBytes + 1
        #expect(qwenOnly < AIModelKind.mistralSmall32.requiredFreeBytes)
        #expect(AIModelKind.safeDefaultFor(ramGB: 30.9,
                                          freeDiskBytes: qwenOnly) == .qwen3VL4B)
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
