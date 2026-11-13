import Foundation
import MLX
import MLXLMCommon
import MLXVLM

/// Keeps the KV state of the last Chat prompt. The next prompt continues from
/// the part it shares (the system prompt, earlier turns, or the whole evidence
/// block for the plain re-ask), trimmed in place, so nothing shared is
/// prefilled twice. Models whose caches cannot be trimmed start fresh. Used only
/// inside `ModelContainer.perform`, which serializes access.
final class VLMTextPromptCache: @unchecked Sendable {
    private var modelName: String?
    private var tokens: [Int] = []
    private var cache: [KVCache]?
    private var state: LMOutput.State?
    private(set) var disabled = false

    func reset() {
        modelName = nil
        tokens = []
        cache = nil
        state = nil
    }

    func disable() {
        reset()
        disabled = true
    }

    private func usable(_ input: LMInput, _ context: ModelContext) -> Bool {
        !disabled && VLMPrefixCache.enabled() && VLMPrefixCache.supports(context.model) && input.image == nil && input.video == nil
    }

    struct Warm {
        let input: LMInput
        let cache: [KVCache]?
        let state: LMOutput.State?
    }

    /// What is left to feed and the cache and model state to continue from.
    /// Without a reusable cache, everything but the last prompt token is
    /// prefilled here so the model state (Qwen3-VL's rope deltas) can be kept.
    /// Hand the cache back with `store` once the reply is done.
    func take(_ input: LMInput, context: ModelContext, parameters: GenerateParameters, stats: VLMPassStats) throws -> Warm {
        let all = input.text.tokens.reshaped(-1).asArray(Int.self)
        let kept = cache, keptState = state, keptTokens = tokens
        cache = nil; state = nil; tokens = []
        guard usable(input, context), all.count > 1 else { return Warm(input: input, cache: nil, state: nil) }
        if modelName != context.configuration.name { modelName = context.configuration.name; return try fresh(input, all, context, parameters) }
        var common = 0
        let limit = min(all.count - 1, keptTokens.count)
        while common < limit, all[common] == keptTokens[common] { common += 1 }
        guard let kept, common >= VLMPrefixCache.minimumPrefixTokens, kept.allSatisfy({ $0.isTrimmable && $0.offset >= common }) else {
            return try fresh(input, all, context, parameters)
        }
        for layer in kept where layer.offset > common { _ = layer.trim(layer.offset - common) }
        stats.set("prefixReusedTokens", Double(common))
        return Warm(input: tail(input, from: common), cache: kept, state: keptState)
    }

    private func fresh(_ input: LMInput, _ all: [Int], _ context: ModelContext, _ parameters: GenerateParameters) throws -> Warm {
        let filled = try context.model.newCache(parameters: parameters)
        guard filled.allSatisfy(\.isTrimmable) else { return Warm(input: input, cache: nil, state: nil) }
        let prefix = LMInput(tokens: MLXArray(all.dropLast().map(Int32.init))[.newAxis, 0...])
        var prefixState: LMOutput.State?
        switch try context.model.prepare(prefix, cache: filled, state: nil, prefill: parameters.prefill) {
        case .logits(let output): prefixState = output.state
        case .tokens(let remaining): prefixState = context.model(remaining, cache: filled, state: nil).state
        }
        eval(filled)
        return Warm(input: tail(input, from: all.count - 1), cache: filled, state: prefixState)
    }

    private func tail(_ input: LMInput, from start: Int) -> LMInput {
        LMInput(text: .init(tokens: input.text.tokens.ndim == 1 ? input.text.tokens[start...] : input.text.tokens[0..., start...]))
    }

    /// Keeps the cache, trimmed back to the prompt it was filled from, and the model state.
    func store(_ warm: Warm, prompt input: LMInput, context: ModelContext) {
        guard let filled = warm.cache, usable(input, context), filled.allSatisfy(\.isTrimmable) else { return }
        let prompt = input.text.tokens.reshaped(-1).asArray(Int.self)
        guard filled.allSatisfy({ $0.offset >= prompt.count }) else { return }
        for layer in filled where layer.offset > prompt.count { _ = layer.trim(layer.offset - prompt.count) }
        modelName = context.configuration.name
        tokens = prompt
        cache = filled
        state = warm.state
    }
}
