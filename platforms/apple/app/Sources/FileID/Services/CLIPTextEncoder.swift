// CLIP text encoder — query-time semantic search. Tokenize a string via
// CLIPTokenizer (OpenAI BPE), run the OpenCLIP ViT-B/32 text ONNX through
// ONNX Runtime, L2-normalize, return 512 floats in the same space as the
// image embeddings in `clip_embeddings`. Cosine over the two gives search.
//
// Commercial-clean: ViT-B/32 (MIT) ONNX via ORT replaces Apple's MobileCLIP-S2
// CoreML text model (research-only). Input contract MUST match the Windows
// engine's `models/clip_text.rs` exactly — input_ids as int64, shape [1, 77],
// zero-padded, truncated to 77 — or the text embedding lands in a different
// space than the ViT-B/32 image embeddings and search breaks.
import Foundation
import FileIDShared
import OnnxRuntimeBindings

public final class CLIPTextEncoder: @unchecked Sendable {

    public static let shared = CLIPTextEncoder()

    private let lock = NSLock()
    private var env: ORTEnv?
    private var session: ORTSession?
    private var inputName: String?

    private static let contextLen = 77

    private init() {}

    public var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return session != nil
    }

    /// Standard install location — beside the other AI models.
    public static var defaultDirectory: URL {
        AppSupportPath.models.appendingPathComponent("clip_text", isDirectory: true)
    }

    public static var defaultModelURL: URL {
        defaultDirectory.appendingPathComponent("clip_text.onnx")
    }

    /// Load the ViT-B/32 text ONNX + the BPE tokenizer's vocabulary. Returns
    /// true iff both are present and loaded successfully.
    @discardableResult
    public func load() -> Bool {
        lock.lock(); defer { lock.unlock() }
        if session != nil { return true }
        let dir = Self.defaultDirectory
        guard FileManager.default.fileExists(atPath: dir.path) else { return false }
        guard CLIPTokenizer.shared.loadVocabulary(modelDirectory: dir) else {
            NSLog("FileID CLIP text: vocab.json or merges.txt not found in %@", redactPathForLog(dir.path))
            return false
        }
        let modelURL = Self.defaultModelURL
        guard FileManager.default.fileExists(atPath: modelURL.path) else {
            NSLog("FileID CLIP text: clip_text.onnx not found at %@", redactPathForLog(modelURL.path))
            return false
        }
        do {
            let env = try self.env ?? ORTEnv(loggingLevel: ORTLoggingLevel.warning)
            let opts = try ORTSessionOptions()
            try? opts.appendCoreMLExecutionProvider(with: ORTCoreMLExecutionProviderOptions())
            let session = try ORTSession(env: env, modelPath: modelURL.path, sessionOptions: opts)
            self.env = env
            self.session = session
            self.inputName = try session.inputNames().first
            NSLog("FileID CLIP text: loaded ViT-B/32 ONNX from %@", redactPathForLog(modelURL.path))
            return true
        } catch {
            NSLog("FileID CLIP text load failed: %@", "\(error)")
            return false
        }
    }

    /// Embed a free-text query into the CLIP image-embedding space.
    /// L2-normalized; nil if the model/tokenizer isn't ready or inference fails.
    public func embedText(_ query: String) -> [Float]? {
        guard load() else { return nil }
        guard let tokens = CLIPTokenizer.shared.encode(query) else { return nil }
        lock.lock(); let s = session; let name = inputName; lock.unlock()
        guard let s, let name else { return nil }

        // int64 input_ids, [1, 77], zero-padded — matches clip_text.rs.
        var ids = [Int64](repeating: 0, count: Self.contextLen)
        for (i, t) in tokens.prefix(Self.contextLen).enumerated() { ids[i] = Int64(t) }

        do {
            let nsData = ids.withUnsafeBufferPointer { buf in
                NSMutableData(bytes: buf.baseAddress, length: buf.count * MemoryLayout<Int64>.stride)
            }
            let value = try ORTValue(tensorData: nsData, elementType: .int64,
                                     shape: [1, NSNumber(value: Self.contextLen)])
            let outputs = try s.run(withInputs: [name: value],
                                    outputNames: Set(try s.outputNames()),
                                    runOptions: nil)
            guard let first = outputs.values.first else { return nil }
            let data = try first.tensorData() as Data
            let count = data.count / MemoryLayout<Float>.stride
            guard count > 0 else { return nil }
            var vec = [Float](repeating: 0, count: count)
            data.withUnsafeBytes { raw in
                let src = raw.baseAddress!.assumingMemoryBound(to: Float.self)
                for i in 0..<count { vec[i] = src[i] }
            }
            var norm: Float = 0
            for x in vec { norm += x * x }
            let invN = Float(1) / max(.leastNonzeroMagnitude, norm.squareRoot())
            for i in 0..<count { vec[i] *= invN }
            return vec
        } catch {
            NSLog("FileID CLIP text inference failed: %@", "\(error)")
            return nil
        }
    }
}
