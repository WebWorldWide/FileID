import Foundation
import MLX
import MLXLMCommon

/// Greedy decoding for Deep Analyze replies: stops as soon as the reply is
/// complete and can verify n-gram drafts copied from the prompt and the reply so
/// far (prompt-lookup decoding), which needs no second model. Greedy acceptance
/// keeps the output identical to plain greedy decoding.
enum VLMGreedyDecoder {
    struct Outcome {
        var tokens: [Int] = []
        var text = ""
        var promptTokens = 0
        var promptSeconds: Double = 0
        var generateSeconds: Double = 0
        var modelCalls = 0
        var draftedTokens = 0
        var acceptedTokens = 0
        var stoppedEarly = false
    }

    static func promptLookupEnabled(environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        environment["FILEID_VLM_PROMPT_LOOKUP"] == "1"
    }

    static func earlyStopEnabled(environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        environment["FILEID_VLM_EARLY_STOP"] != "0"
    }

    static func draftLength(environment: [String: String] = ProcessInfo.processInfo.environment) -> Int {
        if let raw = environment["FILEID_VLM_LOOKUP_TOKENS"], let n = Int(raw), (1...16).contains(n) { return n }
        return 6
    }

    static let maximumNGram = 3

    static func stopTokenIDs(_ context: ModelContext) -> Set<Int> {
        var ids = context.configuration.eosTokenIds
        if let eos = context.tokenizer.eosTokenId { ids.insert(eos) }
        if let unknown = context.tokenizer.unknownTokenId { ids.insert(unknown) }
        for token in context.configuration.extraEOSTokens.union(["<|im_end|>", "<|endoftext|>"]) {
            if let id = context.tokenizer.convertTokenToId(token) { ids.insert(id) }
        }
        return ids
    }

    /// Tokens following the most recent earlier occurrence of the longest
    /// suffix (up to `maximumNGram` tokens) of `history`, searched in
    /// `sources` and in `history` itself.
    static func draft(history: [Int], sources: [[Int]], length: Int) -> [Int] {
        guard length > 0, !history.isEmpty else { return [] }
        for n in stride(from: min(maximumNGram, history.count), through: 1, by: -1) {
            let suffix = Array(history[(history.count - n)...])
            if let found = lastFollowing(suffix, in: history, before: history.count - n, length: length) { return found }
            for source in sources.reversed() {
                if let found = lastFollowing(suffix, in: source, before: source.count, length: length) { return found }
            }
        }
        return []
    }

    private static func lastFollowing(_ pattern: [Int], in tokens: [Int], before end: Int, length: Int) -> [Int]? {
        let n = pattern.count
        guard end >= n else { return nil }
        var start = end - n
        while start >= 0 {
            if tokens[start] == pattern[0], Array(tokens[start..<(start + n)]) == pattern {
                let from = start + n
                let to = min(tokens.count, from + length)
                if from < to { return Array(tokens[from..<to]) }
            }
            start -= 1
        }
        return nil
    }

    static func run(
        input: LMInput,
        cache providedCache: [KVCache]?,
        state providedState: LMOutput.State?,
        context: ModelContext,
        parameters: GenerateParameters,
        lookupSources: [[Int]],
        derivedSource: ((String) -> [Int]?)? = nil,
        promptLookup: Bool,
        isComplete: (String) -> Bool,
        onText: ((String) async -> Void)?
    ) async throws -> Outcome {
        let model = context.model
        let cache = try providedCache ?? model.newCache(parameters: parameters)
        let stops = stopTokenIDs(context)
        let maxTokens = parameters.maxTokens ?? 320
        let draftLength = promptLookup ? draftLength() : 0
        var outcome = Outcome()
        outcome.promptTokens = input.text.tokens.size
        var state = providedState
        let promptStart = Date()

        let prefill: LMOutput
        switch try model.prepare(input, cache: cache, state: state, prefill: parameters.prefill) {
        case .logits(let output):
            prefill = output
        case .tokens(let remaining):
            prefill = model(remaining[text: .newAxis], cache: cache, state: state)
        }
        state = prefill.state ?? state
        var pending = argMax(prefill.logits[0..., -1, 0...], axis: -1)
        var fedDraft: [Int] = []
        asyncEval([pending] + cache.flatMap { $0.state })
        var emittedText = ""
        var generateStart = Date()
        var history: [Int] = []
        var sources = lookupSources
        var derived = derivedSource == nil || !promptLookup

        func emit(_ token: Int) -> Bool {
            if stops.contains(token) { return false }
            outcome.tokens.append(token)
            history.append(token)
            return true
        }

        decoding: while true {
            var lookahead: MLXArray?
            if draftLength == 0 {
                // Queue the next step on the unevaluated token so the GPU never waits
                // for the CPU to build the graph, as TokenIterator does.
                let output = model(LMInput.Text(tokens: pending.reshaped([1, 1])), cache: cache, state: state)
                state = output.state ?? state
                let next = argMax(output.logits[0..., -1, 0...], axis: -1)
                asyncEval([next] + cache.flatMap { $0.state })
                lookahead = next
            }
            let predictions = pending.asArray(Int.self)
            if outcome.modelCalls == 0 {
                outcome.promptSeconds = Date().timeIntervalSince(promptStart)
                generateStart = Date()
            }
            outcome.modelCalls += 1
            var accepted = 0
            while accepted < fedDraft.count, predictions[accepted] == fedDraft[accepted] { accepted += 1 }
            outcome.acceptedTokens += accepted
            let rejected = fedDraft.count - accepted
            if rejected > 0 { trimPromptCache(cache, numTokens: rejected) }
            var finished = false
            for token in fedDraft.prefix(accepted) + [predictions[accepted]] {
                if !emit(token) || outcome.tokens.count >= maxTokens { finished = true; break }
            }
            let text = context.tokenizer.decode(tokenIds: outcome.tokens)
            if let onText, text.count > emittedText.count, text.hasPrefix(emittedText) {
                await onText(String(text.dropFirst(emittedText.count)))
            }
            emittedText = text
            if finished { break decoding }
            if !derived, let extra = derivedSource?(text) {
                sources.append(extra)
                derived = true
            }
            if isComplete(text) { outcome.stoppedEarly = true; break decoding }
            try Task.checkCancellation()
            if let lookahead {
                pending = lookahead
                continue
            }

            let last = outcome.tokens[outcome.tokens.count - 1]
            fedDraft = draftLength > 0
                ? Array(draft(history: history, sources: sources, length: min(draftLength, maxTokens - outcome.tokens.count)))
                : []
            outcome.draftedTokens += fedDraft.count
            let fed = MLXArray(([last] + fedDraft).map(Int32.init))[.newAxis, 0...]
            let output = model(LMInput.Text(tokens: fed), cache: cache, state: state)
            state = output.state ?? state
            pending = argMax(output.logits[0], axis: -1)
            asyncEval([pending] + cache.flatMap { $0.state })
        }
        Stream.defaultStream.synchronize()
        outcome.text = emittedText
        outcome.generateSeconds = Date().timeIntervalSince(generateStart)
        return outcome
    }
}
