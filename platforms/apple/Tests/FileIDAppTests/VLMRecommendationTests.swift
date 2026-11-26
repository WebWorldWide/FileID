import Testing
import FileIDShared
@testable import FileID

@Suite("Description model recommendation")
struct VLMRecommendationTests {
    private func advice(availableMB: Int64, recommended: String?) -> VLMAdvice {
        VLMAdvice(ModelCatalog(requestID: "advice-1", physicalMemoryGB: 16, availableMemoryMB: availableMB, vramMB: 0,
                               recommendedID: recommended, models: []))
    }

    @Test("both surfaces show the engine's pick for the memory free right now")
    func followsTheEnginePick() {
        let live = VLMRecommendation(ramGB: 16, advice: advice(availableMB: 5_600, recommended: "qwen3_vl_2b"), freeDiskBytes: nil)
        #expect(live.kind == .qwen3VL2B)
        #expect(live.bestForThisMac == .qwen3VL8B)
    }

    @Test("a pick below the best model says why, with the free memory")
    func saysWhyWhenItDiffers() {
        let live = VLMRecommendation(ramGB: 16, advice: advice(availableMB: 6_246, recommended: "qwen3_vl_4b"), freeDiskBytes: nil)
        #expect(live.reason == "Qwen3-VL 8B fits this Mac; Qwen3-VL 4B is recommended right now because only 6.1 GB is free.")
    }

    @Test("no reason when the engine agrees with the best model, or has not answered")
    func quietOtherwise() {
        #expect(VLMRecommendation(ramGB: 16, advice: advice(availableMB: 13_000, recommended: "qwen3_vl_8b"), freeDiskBytes: nil).reason == nil)
        let unanswered = VLMRecommendation(ramGB: 16, advice: nil, freeDiskBytes: nil)
        #expect(unanswered.kind == .qwen3VL8B)
        #expect(unanswered.reason == nil)
    }
}

@Suite("Description model before any choice is saved")
struct DescriptionModelAdoptionTests {
    private func advice(availableMB: Int64, recommended: String) -> VLMAdvice {
        VLMAdvice(ModelCatalog(requestID: "advice-1", physicalMemoryGB: 16, availableMemoryMB: availableMB, vramMB: 0,
                               recommendedID: recommended, models: []))
    }

    @Test("the active model drops to the engine's pick when little memory is free")
    func followsLowMemory() {
        let kind = DeepAnalyzeSettings.adoptedKind(
            current: .qwen3VL8B, persistedRawValue: nil, advice: advice(availableMB: 6_246, recommended: "qwen3_vl_4b"),
            ramGB: 16, freeDiskBytes: nil, isInstalled: { _ in false })
        #expect(kind == .qwen3VL4B)
    }

    @Test("a saved choice, a downloaded model, or no answer from the engine leaves the model alone")
    func leavesOtherCasesAlone() {
        let low = advice(availableMB: 6_246, recommended: "qwen3_vl_4b")
        #expect(DeepAnalyzeSettings.adoptedKind(current: .qwen3VL8B, persistedRawValue: "qwen3_vl_8b", advice: low, ramGB: 16, freeDiskBytes: nil, isInstalled: { _ in false }) == nil)
        #expect(DeepAnalyzeSettings.adoptedKind(current: .qwen3VL8B, persistedRawValue: nil, advice: low, ramGB: 16, freeDiskBytes: nil, isInstalled: { _ in true }) == nil)
        #expect(DeepAnalyzeSettings.adoptedKind(current: .qwen3VL8B, persistedRawValue: nil, advice: nil, ramGB: 16, freeDiskBytes: nil, isInstalled: { _ in false }) == nil)
    }
}

@Suite("Describe run summary")
struct DescribeRunSummaryTests {
    @Test("a run on the chosen model reads as before")
    func sameModel() {
        #expect(DescribeRunSummary.toast(processed: 8, ran: "qwen3_vl_8b", chosen: .qwen3VL8B) == "Descriptions ready for 8 files")
    }

    @Test("a run that fell back names the model that ran and why")
    func fallback() {
        let toast = DescribeRunSummary.toast(processed: 8, ran: "qwen3_vl_4b", chosen: .qwen3VL8B)
        #expect(toast == "Descriptions ready for 8 files, written with \(AIModelKind.qwen3VL4B.displayName), because not enough memory was free for \(AIModelKind.qwen3VL8B.displayName)")
    }
}
