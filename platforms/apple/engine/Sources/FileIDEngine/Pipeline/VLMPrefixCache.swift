import Foundation
import MLX
import MLXLMCommon
import MLXVLM

/// Keeps the KV cache of the text that every Deep Analyze prompt starts with
/// (system prompt and the user-turn header), so each file prefills only its image
/// and request. Used only inside `ModelContainer.perform`, which serializes access.
final class VLMPrefixCache: @unchecked Sendable {
    /// Models whose `prepare` continues a warm text cache with new image tokens.
    static func supports(_ model: any LanguageModel) -> Bool { model is Qwen3VL || model is Qwen35 }
    static let minimumPrefixTokens = 48

    private var modelName: String?
    private var prefixTokens: [Int] = []
    private var cache: [KVCache]?
    private var state: LMOutput.State?
    private var previousTokens: [Int] = []
    private(set) var disabled = false

    static func enabled(environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        environment["FILEID_VLM_PREFIX_CACHE"] != "0"
    }

    func reset() {
        modelName = nil
        prefixTokens = []
        cache = nil
        state = nil
        previousTokens = []
    }

    func disable() {
        reset()
        disabled = true
    }

    /// Returns the input to generate from plus the warm cache and state, or the
    /// original input with no cache when no shared text prefix is available.
    func split(
        _ input: LMInput,
        context: ModelContext,
        parameters: GenerateParameters,
        stats: VLMPassStats
    ) throws -> (input: LMInput, cache: [KVCache]?, state: LMOutput.State?) {
        guard !disabled, Self.enabled(), Self.supports(context.model),
              input.image != nil, input.video == nil else { return (input, nil, nil) }
        if modelName != context.configuration.name { reset(); modelName = context.configuration.name }
        let tokens = input.text.tokens.reshaped(-1).asArray(Int.self)
        let mediaIDs = ["<|vision_start|>", "<|image_pad|>"].compactMap { context.tokenizer.convertTokenToId($0) }
        guard let boundary = tokens.firstIndex(where: { mediaIDs.contains($0) }) else { return (input, nil, nil) }

        if cache == nil || prefixTokens.count > boundary || Array(tokens.prefix(prefixTokens.count)) != prefixTokens {
            var common = 0
            let limit = min(boundary, previousTokens.count)
            while common < limit, tokens[common] == previousTokens[common] { common += 1 }
            previousTokens = Array(tokens.prefix(boundary))
            guard common >= Self.minimumPrefixTokens else {
                cache = nil
                return (input, nil, nil)
            }
            let started = ContinuousClock.now
            let fresh = try context.model.newCache(parameters: parameters)
            let prefix = LMInput(tokens: MLXArray(Array(tokens.prefix(common)).map(Int32.init))[.newAxis, 0...])
            var prefixState: LMOutput.State?
            switch try context.model.prepare(prefix, cache: fresh, state: nil, prefill: parameters.prefill) {
            case .logits(let output):
                prefixState = output.state
            case .tokens(let remaining):
                prefixState = context.model(remaining, cache: fresh, state: nil).state
            }
            eval(fresh)
            cache = fresh
            state = prefixState
            prefixTokens = Array(tokens.prefix(common))
            stats.set("prefixBuildMs", VLMTiming.milliseconds(since: started))
        }
        guard let cache else { return (input, nil, nil) }
        let suffix = input.text.tokens.ndim == 1
            ? input.text.tokens[prefixTokens.count...]
            : input.text.tokens[0..., prefixTokens.count...]
        stats.set("prefixReusedTokens", Double(prefixTokens.count))
        return (LMInput(text: .init(tokens: suffix), image: input.image), cache.map { $0.copy() }, state)
    }
}
