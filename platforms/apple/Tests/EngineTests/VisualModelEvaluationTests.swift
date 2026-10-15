import Foundation
import Testing
import FileIDShared
import Tokenizers
@testable import FileIDEngine

@Suite("Explicit visual-model evaluation", .serialized)
struct VisualModelEvaluationTests {
    private var evaluationModel: AIModelKind {
        get throws {
            let raw = ProcessInfo.processInfo.environment["FILEID_EVALUATION_MODEL"] ?? AIModelKind.qwen3VL4B.rawValue
            return try #require(AIModelKind(rawValue: raw))
        }
    }

    @Test(.enabled(if: ProcessInfo.processInfo.environment["FILEID_DOWNLOAD_EVALUATION_MODEL"] == "1"))
    func downloadRegisteredModel() async throws {
        let root = try #require(ProcessInfo.processInfo.environment["FILEID_HF_CACHE_ROOT"])
        try #require(root.hasPrefix("/"))
        try await VLMDownloader.shared.fetchRepo(
            repo: try evaluationModel.sourceRepo,
            documentsHF: URL(fileURLWithPath: root, isDirectory: true)
        ) { _, _, _ in }
        let pin = try #require(ModelManifest.vlmPin(forRepo: try evaluationModel.sourceRepo))
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
        try await DeepAnalyze.shared.ensureLoaded(kind: evaluationModel)
        let result = await DeepAnalyze.shared.analyzeMomentSequence(
            imageURLs: indices.map { images[$0] }, times: indices.map { times[$0] }
        )
        print("MOMENT_EVALUATION: \(result.description)")
        _ = try TimelineMomentAnalysis.parse(result.description, frameCount: indices.count)
    }
    @Test(.enabled(if: ProcessInfo.processInfo.environment["FILEID_EVALUATION_GOAL"] != nil))
    func evaluateGoalFrames() async throws {
        _ = try #require(ProcessInfo.processInfo.environment["FILEID_HF_CACHE_ROOT"])
        let root = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["FILEID_EVALUATION_FRAMES"]), isDirectory: true)
        let goal = try #require(ProcessInfo.processInfo.environment["FILEID_EVALUATION_GOAL"])
        let images = (0..<8).map { root.appendingPathComponent("frame-\($0).png") }
        let times = (0..<8).map(Double.init)
        try await DeepAnalyze.shared.ensureLoaded(kind: evaluationModel)
        let result = await DeepAnalyze.shared.analyzeTakeOutcomeSequence(imageURLs: images, times: times, goal: goal)
        print("GOAL_EVALUATION: \(result.description)")
        let parsed = try TakeOutcomeAnalysis.parse(result.description, frameCount: images.count)
        print("GOAL_STATUS: \(parsed.status.rawValue), confidence \(parsed.confidence)")
    }

    @Test(.enabled(if: ProcessInfo.processInfo.environment["FILEID_EVALUATION_TOKENIZER"] == "1"))
    func localTokenizerHonorsConciseTemplateContext() async throws {
        let cache = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["FILEID_HF_CACHE_ROOT"]))
        let model = try evaluationModel
        let directory = cache.appendingPathComponent("models").appendingPathComponent(model.sourceRepo)
        let tokenizer = try await AutoTokenizer.from(modelFolder: directory)
        let messages: [Message] = [["role":"user", "content":"Return the observed event as JSON."]]
        let concise = try tokenizer.applyChatTemplate(messages: messages, tools: nil, additionalContext: DeepAnalyze.templateContext)
        let reasoning = try tokenizer.applyChatTemplate(messages: messages, tools: nil, additionalContext: ["enable_thinking":true])
        let text = tokenizer.decode(tokens: concise)
        #expect(text.contains("<think>\n\n</think>"))
        #expect(concise != reasoning)
    }

}
