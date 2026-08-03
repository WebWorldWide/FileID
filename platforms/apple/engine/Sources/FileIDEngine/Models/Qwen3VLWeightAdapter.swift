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
        windowSize: Int?
    ) throws -> PrepareResult {
        try model.prepare(input, cache: cache, windowSize: windowSize)
    }

    func callAsFunction(_ inputs: MLXArray, cache: [any KVCache]?) -> MLXArray {
        model(inputs, cache: cache)
    }

    func newCache(parameters: GenerateParameters?) -> [any KVCache] {
        model.newCache(parameters: parameters)
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
