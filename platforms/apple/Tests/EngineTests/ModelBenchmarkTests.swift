import Testing
import Foundation
import CoreImage
import FileIDShared
@testable import FileIDEngine

@Suite("MLX model benchmark")
struct ModelBenchmarkTests {
    @Test func medianHandlesOddEvenAndNonFiniteRuns() {
        #expect(ModelBenchmarkRunner.median([3, 1, 2]) == 2)
        #expect(ModelBenchmarkRunner.median([4, 1, 3, 2]) == 2.5)
        #expect(ModelBenchmarkRunner.median([.nan, 5]) == 5)
        #expect(ModelBenchmarkRunner.median([]) == nil)
    }

    @Test func jsonValidityAcceptsFencedObjectsOnly() {
        #expect(ModelBenchmarkRunner.isValidJSONObject(#"{"title":"Beach","tags":["sea","sun","family"]}"#))
        #expect(ModelBenchmarkRunner.isValidJSONObject("```json\n{\"a\":1}\n```"))
        #expect(ModelBenchmarkRunner.isValidJSONObject("Sure: {\"a\": [1, 2]}"))
        #expect(!ModelBenchmarkRunner.isValidJSONObject("[1, 2, 3]"))
        #expect(!ModelBenchmarkRunner.isValidJSONObject("{title: beach}"))
        #expect(!ModelBenchmarkRunner.isValidJSONObject("no json here"))
    }

    @Test func probeMatchesWholeWordsOnly() {
        #expect(ModelBenchmarkRunner.answerMatches("Red.", expected: "red"))
        #expect(ModelBenchmarkRunner.answerMatches("The circle is red", expected: "red"))
        #expect(!ModelBenchmarkRunner.answerMatches("reddish", expected: "red"))
        #expect(!ModelBenchmarkRunner.answerMatches("blue", expected: "red"))
    }

    @Test func workloadsCoverVisionOnlyForVisionModels() {
        #expect(ModelBenchmarkRunner.workloads(vision: false).allSatisfy { !$0.withImage })
        let vision = ModelBenchmarkRunner.workloads(vision: true)
        #expect(vision.contains { $0.phase == "vision" })
        #expect(vision.contains { $0.expectedAnswer == "red" })
    }

    @Test func syntheticCardIsRedAtTheCentreAndDeterministic() throws {
        let image = ModelBenchmarkRunner.syntheticImage()
        let context = CIContext(options: [.workingColorSpace: NSNull()])
        var pixel = [UInt8](repeating: 0, count: 4)
        context.render(image, toBitmap: &pixel, rowBytes: 4, bounds: CGRect(x: 224, y: 248, width: 1, height: 1),
                       format: .RGBA8, colorSpace: nil)
        #expect(pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60)
        var corner = [UInt8](repeating: 0, count: 4)
        context.render(image, toBitmap: &corner, rowBytes: 4, bounds: CGRect(x: 4, y: 440, width: 1, height: 1),
                       format: .RGBA8, colorSpace: nil)
        #expect(corner[2] > corner[0])
    }

    @Test func fixedTileArchitecturesKeepTheirOwnImageSize() {
        #expect(!DeepAnalyze.acceptsResize(architecture: "idefics3"))
        #expect(!DeepAnalyze.acceptsResize(architecture: "smolvlm"))
        #expect(DeepAnalyze.acceptsResize(architecture: "qwen2_5_vl"))
        #expect(DeepAnalyze.acceptsResize(architecture: "gemma4"))
        #expect(DeepAnalyze.acceptsResize(architecture: nil))
    }

    @Test func mlxCacheCapScalesWithModelAndHonoursOverride() {
        #expect(DeepAnalyze.mlxCacheLimitMB(for: .qwen3VL4B, environment: [:]) == 256)
        #expect(DeepAnalyze.mlxCacheLimitMB(for: .qwen3VL8B, environment: [:]) == 512)
        #expect(DeepAnalyze.mlxCacheLimitMB(for: .qwen3VL4B, environment: ["FILEID_MLX_CACHE_MB": "0"]) == 0)
        #expect(DeepAnalyze.mlxCacheLimitMB(for: .qwen3VL4B, environment: ["FILEID_MLX_CACHE_MB": "x"]) == 256)
    }

    @Test func fingerprintNamesChipMemoryAndOS() {
        let fingerprint = ModelBenchmarkRunner.hardwareFingerprint()
        #expect(fingerprint.contains(" GB"))
        #expect(fingerprint.contains("macOS "))
    }

    @Test func uninstalledModelIsRejectedWithoutDownloading() async throws {
        let phases = PhaseLog()
        await #expect(throws: ModelBenchmarkRunner.Failure.self) {
            _ = try await ModelBenchmarkRunner.run(kind: .qwen3VL4B, isInstalled: { _ in false }) { phase, _, _ in
                phases.append(phase)
            }
        }
        #expect(phases.values.isEmpty)
    }

    @Test func storeKeepsOnlyThisMachinesResults() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let file = root.appendingPathComponent("model-benchmarks.json")

        let here = ModelBenchmark(measuredAt: "2026-10-15T00:00:00Z", hardware: ModelBenchmarkRunner.hardwareFingerprint(),
                                  loadSeconds: 4.2, promptTokensPerSecond: 410, generatedTokensPerSecond: 38)
        ModelBenchmarkStore.save(here, for: "qwen3VL4B", at: file)
        #expect(ModelBenchmarkStore.all(at: file)["qwen3VL4B"] == here)

        let elsewhere = ModelBenchmark(measuredAt: "2026-10-15T00:00:00Z", hardware: "Other Mac", loadSeconds: 1)
        let data = try JSONEncoder().encode(["qwen3VL8B": elsewhere, "qwen3VL4B": here])
        try data.write(to: file)
        #expect(ModelBenchmarkStore.all(at: file).keys.sorted() == ["qwen3VL4B"])
    }
}

private final class PhaseLog: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [String] = []
    func append(_ phase: String) { lock.lock(); stored.append(phase); lock.unlock() }
    var values: [String] { lock.lock(); defer { lock.unlock() }; return stored }
}

@Suite("Idle model release")
struct IdleModelReleaseTests {
    @Test func idleTimeoutDefaultsToFiveMinutesAndHonoursOverride() {
        #expect(DeepAnalyze.idleUnloadSeconds(environment: [:]) == 300)
        #expect(DeepAnalyze.idleUnloadSeconds(environment: ["FILEID_MODEL_IDLE_SECONDS": "0"]) == 0)
        #expect(DeepAnalyze.idleUnloadSeconds(environment: ["FILEID_MODEL_IDLE_SECONDS": "45"]) == 45)
        #expect(DeepAnalyze.idleUnloadSeconds(environment: ["FILEID_MODEL_IDLE_SECONDS": "-1"]) == 300)
    }

    @Test func peakMemoryIsDecimalGigabytes() {
        #expect(ModelBenchmarkRunner.decimalGB(3_600_000_000) == 3.6)
        #expect(ModelBenchmarkRunner.decimalGB(1_073_741_824) == 1.07)
    }
}
