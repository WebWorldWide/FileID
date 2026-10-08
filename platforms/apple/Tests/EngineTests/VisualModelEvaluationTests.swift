import Foundation
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Explicit visual-model evaluation", .serialized)
struct VisualModelEvaluationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["FILEID_DOWNLOAD_EVALUATION_MODEL"] == "1"))
    func downloadRegisteredModel() async throws {
        let root = try #require(ProcessInfo.processInfo.environment["FILEID_HF_CACHE_ROOT"])
        try #require(root.hasPrefix("/"))
        try await VLMDownloader.shared.fetchRepo(
            repo: AIModelKind.qwen3VL4B.sourceRepo,
            documentsHF: URL(fileURLWithPath: root, isDirectory: true)
        ) { _, _, _ in }
        let pin = try #require(ModelManifest.vlmPin(forRepo: AIModelKind.qwen3VL4B.sourceRepo))
        let sentinel = ModelCachePaths.huggingFaceModels
            .appendingPathComponent(pin.repo)
            .appendingPathComponent(".fileid-verified-\(pin.revision)")
        #expect(FileManager.default.fileExists(atPath: sentinel.path))
    }

    @Test(.enabled(if: ProcessInfo.processInfo.environment["FILEID_EVALUATION_FRAMES"] != nil))
    func evaluateMomentFrames() async throws {
        _ = try #require(ProcessInfo.processInfo.environment["FILEID_HF_CACHE_ROOT"])
        let directory = URL(fileURLWithPath: try #require(
            ProcessInfo.processInfo.environment["FILEID_EVALUATION_FRAMES"]
        ), isDirectory: true)
        let images = (0..<8).map { directory.appendingPathComponent("frame-\($0).png") }
        for image in images {
            try #require(FileManager.default.fileExists(atPath: image.path))
        }
        let metadata = try JSONDecoder().decode([VideoFrameMetadata].self, from:
            Data(contentsOf: directory.appendingPathComponent("metadata.json")))
        try #require(metadata.count == images.count)
        let times = metadata.map(\.seconds)
        let duration = try #require(metadata.first?.duration)
        try #require(duration.isFinite && duration > 0)
        let window = TimelineMomentAnalysis.Window(start: 0, end: duration, times: times)
        let indices = try TimelineMomentAnalysis.distinctFrameIndices(times: times, window: window)
        try await DeepAnalyze.shared.ensureLoaded(kind: .qwen3VL4B)
        let result = await DeepAnalyze.shared.analyzeMomentSequence(
            imageURLs: indices.map { images[$0] }, times: indices.map { times[$0] }
        )
        print("MOMENT_EVALUATION: \(result.description)")
        _ = try TimelineMomentAnalysis.parse(result.description, frameCount: indices.count)
    }
}
