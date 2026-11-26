import Testing
import FileIDModelInfo
@testable import FileID

@Suite("Starting description model")
struct StartingModelTests {
    @Test func aSavedChoiceWins() {
        let kind = DeepAnalyzeSettings.startingModel(
            persistedRawValue: AIModelKind.gemma3_4B.rawValue, ramGB: 30.9, freeDiskBytes: Int64.max, isInstalled: { _ in false })
        #expect(kind == .gemma3_4B)
    }

    @Test func aDownloadedRecommendationIsPickedEvenWhenDiskIsTight() {
        let kind = DeepAnalyzeSettings.startingModel(
            persistedRawValue: nil, ramGB: 16, freeDiskBytes: 0, isInstalled: { $0 == .qwen3VL8B })
        #expect(kind == .qwen3VL8B)
    }

    @Test func withNothingDownloadedTheDiskDecides() {
        let kind = DeepAnalyzeSettings.startingModel(
            persistedRawValue: "", ramGB: 16, freeDiskBytes: 0, isInstalled: { _ in false })
        #expect(kind == AIModelKind.safeDefaultFor(ramGB: 16, freeDiskBytes: 0))
    }
}
