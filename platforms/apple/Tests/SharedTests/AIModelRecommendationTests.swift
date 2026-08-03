import Testing
@testable import FileIDShared

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
}
