import Foundation
import CoreImage
import MLX
import MLXLMCommon
import MLXVLM

// MARK: - DeepAnalyzeService

// Qwen2.5-VL 3B via Apple MLX, ~1–2 s/image on M1. Used post-scan and per-image
// from MediaPreviewOverlay; the loaded ModelContainer is cached between calls.
//
// Actor (not @MainActor) because `VLMModelFactory.loadContainer` blocks on
// large GPU allocations — running that on the main thread freezes the UI and
// was contributing to scan-time lockups when Deep Analyze was invoked.

actor DeepAnalyzeService {
    static let shared = DeepAnalyzeService()

    enum State: Sendable {
        case notLoaded
        case loading(progress: Double, message: String)
        case ready
        case failed(String)
    }

    private(set) var state: State = .notLoaded
    private var container: ModelContainer?
    private var loadedKind: AIModelKind?

    private let generateParams = MLXLMCommon.GenerateParameters(
        maxTokens: 320,
        temperature: 0.3,
        topP: 0.9
    )

    private init() {}

    // MARK: - Active model selection

    // Resolves AIModelKind to an mlx-swift-examples ModelConfiguration. The
    // pinned 2.29.1 VLMRegistry exposes 6 architectures we surface to the
    // user. Returns nil for non-VLM kinds.
    nonisolated static func vlmConfig(for kind: AIModelKind) -> ModelConfiguration? {
        switch kind {
        case .qwen2VL2B:    return VLMRegistry.qwen2_5VL3BInstruct4Bit
        case .qwen3VL4B:    return VLMRegistry.qwen3VL4BInstruct4Bit
        case .gemma3_4B:    return VLMRegistry.gemma3_4B_qat_4bit
        case .gemma3_12B:   return VLMRegistry.gemma3_12B_qat_4bit
        case .smolvlm:      return VLMRegistry.smolvlminstruct4bit
        case .paligemma3B:  return VLMRegistry.paligemma3bMix448_8bit
        case .mobileCLIPImage, .mobileCLIPText: return nil
        }
    }

    // The model the user picked in Settings (or the default).
    nonisolated static var activeKind: AIModelKind {
        if let raw = UserDefaults.standard.string(forKey: "deepAnalyzeActiveModel"),
           let k = AIModelKind(rawValue: raw),
           k.isVLM {
            return k
        }
        return .qwen2VL2B
    }

    // GPU cache budget per model. Heavy models need more headroom; the small
    // ones can run lean to leave room for Vision + thumbnail caches.
    nonisolated static func gpuCacheBudgetMB(for kind: AIModelKind) -> Int {
        switch kind {
        case .gemma3_12B:               return 8_192      // ~7 GB weights + headroom
        case .qwen3VL4B, .gemma3_4B,
             .paligemma3B, .qwen2VL2B:  return 3_072
        case .smolvlm:                  return 1_024
        case .mobileCLIPImage,
             .mobileCLIPText:           return 0
        }
    }

    // MARK: - Loading

    func ensureLoaded(
        progress: @MainActor @Sendable @escaping (_ fraction: Double, _ message: String) -> Void
    ) async throws {
        let wantedKind = Self.activeKind
        // Already loaded the right model? Done.
        if container != nil, loadedKind == wantedKind {
            state = .ready
            return
        }
        // Loaded a different model — drop it before swapping. ~3-7 GB of GPU
        // weights need to leave the cache before the next model can land.
        if container != nil, loadedKind != wantedKind {
            container = nil
            loadedKind = nil
            MLX.GPU.set(cacheLimit: 0)
            MLX.GPU.clearCache()
        }

        guard let config = Self.vlmConfig(for: wantedKind) else {
            state = .failed("Selected model is not a VLM.")
            throw NSError(domain: "DeepAnalyzeService", code: 1)
        }

        let displayName = wantedKind.descriptor.displayName
        state = .loading(progress: 0, message: "Preparing \(displayName)…")
        MLX.GPU.set(cacheLimit: Self.gpuCacheBudgetMB(for: wantedKind) * 1024 * 1024)

        do {
            let loaded = try await VLMModelFactory.shared.loadContainer(
                configuration: config
            ) { p in
                let frac = p.fractionCompleted
                let msg  = "Downloading \(displayName) (\(Int(frac * 100))%)"
                Task { @MainActor in progress(frac, msg) }
            }
            container = loaded
            loadedKind = wantedKind
            state = .ready
        } catch {
            state = .failed(error.localizedDescription)
            throw error
        }
    }

    func setupModelFromRegistry(
        progress: @MainActor @Sendable @escaping (Double, String) -> Void
    ) async throws {
        try await ensureLoaded(progress: progress)
    }

    // MARK: - Inference

    func analyze(imageURL: URL) async -> String {
        do {
            try await ensureLoaded { _, _ in }
        } catch {
            return "Deep Analyze unavailable: \(error.localizedDescription). Download the model in Settings → AI Models."
        }

        guard let container else {
            return "Deep Analyze model not loaded."
        }
        // 768 px keeps decode under ~5 MB while preserving detail the 448-input
        // model can use — full decode of a 100 MP RAW would be ~400 MB.
        // autoreleasepool drains CG scratch so a long pass doesn't accumulate.
        let ciImage: CIImage? = autoreleasepool {
            let visionProcessor = VisionProcessor()
            guard let cgImage = visionProcessor.loadImage(from: imageURL, maxPixelSize: 768) else {
                return nil
            }
            return CIImage(cgImage: cgImage)
        }
        guard let ciImage else {
            return "Cannot load image at \(imageURL.lastPathComponent)."
        }

        let collector = TokenCollector()
        let params = generateParams
        do {
            try await container.perform { (context: ModelContext) -> Void in
                let chat: [Chat.Message] = [
                    .system("You are a concise image-understanding assistant. Given an image, reply with: (1) a 1–2 sentence description, then (2) 3–5 short tags prefixed with #. Avoid speculation about people's identities."),
                    .user("Describe this image.", images: [.ciImage(ciImage)], videos: [])
                ]

                var userInput = UserInput(chat: chat)
                userInput.processing.resize = .init(width: 448, height: 448)

                let lmInput = try await context.processor.prepare(input: userInput)
                let stream = try MLXLMCommon.generate(
                    input: lmInput, parameters: params, context: context
                )

                for await item in stream {
                    if let chunk = item.chunk { collector.append(chunk) }
                }
            }
        } catch {
            return "Inference failed: \(error.localizedDescription)"
        }
        return collector.snapshot().trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // Drain MLX GPU scratch between chunks. Keeps Qwen's weights resident.
    func trimCaches() {
        MLX.GPU.clearCache()
    }

    // Release weights for the loaded model. Re-loading costs ~10 s — only
    // call at end of pass or when switching models.
    func unload() {
        container = nil
        loadedKind = nil
        state = .notLoaded
        MLX.GPU.set(cacheLimit: 0)
        MLX.GPU.clearCache()
    }
}

// Sendable accumulator for streamed tokens; MLX's `perform` closure is
// @Sendable so a plain `var` capture fails Swift 6 concurrency checks.
private final class TokenCollector: @unchecked Sendable {
    private var buffer = ""
    private let lock = NSLock()
    func append(_ s: String) { lock.lock(); buffer += s; lock.unlock() }
    func snapshot() -> String { lock.lock(); defer { lock.unlock() }; return buffer }
}

// MARK: - Status helpers

extension DeepAnalyzeService {
    // Treat the presence of config.json in the MLX hub cache as "installed".
    // Defaults to the legacy Qwen kind so existing call sites stay valid.
    nonisolated static func isInstalledOnDisk(_ kind: AIModelKind = .qwen2VL2B) -> Bool {
        guard let dir = modelCacheDirectory(for: kind) else { return false }
        return FileManager.default.fileExists(atPath: dir.appendingPathComponent("config.json").path)
    }

    nonisolated static func modelCacheDirectory(for kind: AIModelKind = .qwen2VL2B) -> URL? {
        AIModelDescriptor.vlmCacheURL(forRepo: kind.descriptor.sourceRepo)
    }

    nonisolated static func removeOnDisk(_ kind: AIModelKind = .qwen2VL2B) {
        guard let dir = modelCacheDirectory(for: kind) else { return }
        try? FileManager.default.removeItem(at: dir)
    }
}
