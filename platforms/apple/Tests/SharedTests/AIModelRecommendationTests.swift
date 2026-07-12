import Testing
@testable import FileIDShared

@Suite("AI model hardware recommendation")
struct AIModelRecommendationTests {
    @Test func eightGBSelectsRunnableGemma() {
        let choice = AIModelKind.safeDefaultFor(ramGB: 8)
        #expect(choice == .gemma3_4B)
        #expect(choice.fits(ramGB: 8))
    }

    @Test func sixteenGBSelectsQwen() {
        #expect(AIModelKind.safeDefaultFor(ramGB: 16) == .qwen2VL7B)
    }

    @Test func nominalThirtyTwoGBSelectsMistral() {
        #expect(AIModelKind.safeDefaultFor(ramGB: 30.9) == .mistralSmall32)
    }

    @Test func diskPressureDowngradesBeforeDownloading() {
        let qwenOnly = AIModelKind.qwen2VL7B.requiredFreeBytes + 1
        #expect(qwenOnly < AIModelKind.mistralSmall32.requiredFreeBytes)
        #expect(AIModelKind.safeDefaultFor(ramGB: 30.9,
                                          freeDiskBytes: qwenOnly) == .qwen2VL7B)
    }

    @Test func onboardingPreservesAnExplicitExistingChoice() {
        let selected = AIModelKind.onboardingSelection(
            persistedRawValue: AIModelKind.gemma3_4B.rawValue,
            ramGB: 30.9,
            freeDiskBytes: Int64.max)
        #expect(selected == .gemma3_4B)
    }
}
