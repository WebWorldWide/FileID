// CLIP text encoder — produces a 512-d embedding from a search query
// in the same space as the image embeddings already stored in
// `clip_embeddings`. Cosine on the two vectors gives semantic search
// — type "sunset at the beach" and rank every photo by how well it
// matches that concept.
//
// Pairs with CLIPTokenizer (BPE → token IDs) and a CoreML .mlpackage
// of OpenAI's CLIP text transformer. The model isn't bundled — the
// user runs scripts/build_clip_text_encoder.py once to convert and
// install. Without the model, isReady returns false and the search
// bar quietly falls back to keyword search.
import Foundation
import CoreML

public final class CLIPTextEncoder: @unchecked Sendable {

    public static let shared = CLIPTextEncoder()

    private let lock = NSLock()
    private var model: MLModel?
    private var loadedURL: URL?

    private init() {}

    public var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return model != nil
    }

    /// Standard install location — beside the other AI models.
    public static var defaultDirectory: URL {
        AppSupportPath.models.appendingPathComponent("clip_text", isDirectory: true)
    }

    public static var defaultModelURL: URL {
        defaultDirectory.appendingPathComponent("clip_text.mlpackage")
    }

    /// Load the CoreML text encoder + the BPE tokenizer's vocabulary.
    /// Returns true iff both pieces are present and loaded successfully.
    @discardableResult
    public func load() -> Bool {
        lock.lock(); defer { lock.unlock() }
        let dir = Self.defaultDirectory
        guard FileManager.default.fileExists(atPath: dir.path) else { return false }
        guard CLIPTokenizer.shared.loadVocabulary(modelDirectory: dir) else {
            NSLog("FileID CLIP text: vocab.json or merges.txt not found in \(dir.path)")
            return false
        }
        let modelURL = Self.defaultModelURL
        guard FileManager.default.fileExists(atPath: modelURL.path) else {
            NSLog("FileID CLIP text: clip_text.mlpackage not found at \(modelURL.path)")
            return false
        }
        do {
            let compiled = try MLModel.compileModel(at: modelURL)
            let config = MLModelConfiguration()
            config.computeUnits = .all
            model = try MLModel(contentsOf: compiled, configuration: config)
            loadedURL = modelURL
            NSLog("FileID CLIP text: loaded from \(modelURL.path)")
            return true
        } catch {
            NSLog("FileID CLIP text load failed: \(error)")
            return false
        }
    }

    /// Embed a free-text query into the CLIP image-embedding space.
    /// Returns nil if the model or tokenizer isn't ready, or if the
    /// inference fails. Result is L2-normalized so cosine search just
    /// uses dot product downstream.
    public func embedText(_ query: String) -> [Float]? {
        guard let tokens = CLIPTokenizer.shared.encode(query) else { return nil }
        lock.lock(); let m = model; lock.unlock()
        guard let m else { return nil }

        // The model expects a 1×77 Int32 input named "input_ids" (or
        // similar — exact name depends on the conversion script).
        // Build an MLMultiArray of shape [1, contextLength].
        let ctx = NSNumber(value: tokens.count)
        guard let arr = try? MLMultiArray(shape: [1, ctx], dataType: .int32) else {
            return nil
        }
        for i in 0..<tokens.count {
            arr[i] = NSNumber(value: tokens[i])
        }
        let inputName = m.modelDescription.inputDescriptionsByName.keys.first ?? "input_ids"
        let input: MLDictionaryFeatureProvider
        do {
            input = try MLDictionaryFeatureProvider(dictionary: [inputName: arr])
        } catch { return nil }

        guard let pred = try? m.prediction(from: input) else { return nil }
        // Output is the CLIP text embedding — usually named "text_embeds"
        // or just the single output. Take the first multiarray and
        // flatten + L2-normalize.
        guard let outName = m.modelDescription.outputDescriptionsByName.keys.first,
              let out = pred.featureValue(for: outName)?.multiArrayValue else {
            return nil
        }
        var vec = [Float](repeating: 0, count: out.count)
        for i in 0..<out.count {
            vec[i] = out[i].floatValue
        }
        // L2 normalize.
        var norm: Float = 0
        for x in vec { norm += x * x }
        let invN = Float(1) / max(.leastNonzeroMagnitude, norm.squareRoot())
        for i in 0..<vec.count { vec[i] *= invN }
        return vec
    }
}
