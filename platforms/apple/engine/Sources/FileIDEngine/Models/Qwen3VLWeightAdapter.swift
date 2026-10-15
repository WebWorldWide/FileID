import Foundation
import MLX
import MLXLMCommon
import MLXNN
import MLXVLM

final class Qwen3VLWeightAdapter: Module, LanguageModel {
    @ModuleInfo(key: "model") private var model: Qwen3VL

    init(_ configuration: Qwen3VLConfiguration) {
        _model.wrappedValue = Qwen3VL(configuration)
    }

    func prepare(
        _ input: LMInput,
        cache: [any KVCache],
        state: LMOutput.State?,
        prefill: PrefillParameters
    ) throws -> PrepareResult {
        try model.prepare(input, cache: cache, state: state, prefill: prefill)
    }

    func prepare() throws {
        try model.prepare()
    }

    /// Every decode step comes through here; without the forward the protocol
    /// default calls the raw-token entry point, which Qwen3VL aborts on.
    func callAsFunction(_ input: LMInput.Text, cache: [any KVCache]?, state: LMOutput.State?) -> LMOutput {
        model(input, cache: cache, state: state)
    }

    func callAsFunction(_ inputs: MLXArray, cache: [any KVCache]?) -> MLXArray {
        model(inputs, cache: cache)
    }

    var reasoningConfig: ReasoningConfig? { model.reasoningConfig }

    func newCache(parameters: GenerateParameters?) throws -> [any KVCache] {
        try model.newCache(parameters: parameters)
    }

    func sanitize(weights: [String: MLXArray]) -> [String: MLXArray] {
        let adapted = Dictionary(uniqueKeysWithValues: weights.map { key, value in
            (Self.keyForPinnedRuntime(key), value)
        })
        return Dictionary(uniqueKeysWithValues: model.sanitize(weights: adapted).map { key, value in
            (Self.wrappedKey(key), value)
        })
    }

    static func keyForPinnedRuntime(_ key: String) -> String {
        let prefix = "language_model.lm_head."
        guard key.hasPrefix(prefix) else { return key }
        return "lm_head." + key.dropFirst(prefix.count)
    }

    static func wrappedKey(_ key: String) -> String {
        "model." + key
    }
}
