// CLIP text encoder for query-time semantic search: BPE-tokenize, run the OpenCLIP ViT-B/32 text ONNX via ONNX Runtime, L2-normalize into the image embedding space.
// The input contract must match the Windows engine's models/clip_text.rs (int64 input_ids, [1, 77], zero-padded, truncated), or search breaks.
// ViT-B/32 (MIT) replaces Apple's research-only MobileCLIP-S2 text model.
import Foundation
import FileIDShared
import OnnxRuntimeBindings

public final class CLIPTextEncoder: @unchecked Sendable {

    public static let shared = CLIPTextEncoder()

    // `lock` guards only the env/session/inputName slots and is held for microseconds. `loadLock` serializes the multi-second session build.
    // Holding one lock across the build froze the UI, which reads `session` from the MainActor.
    private let lock = NSLock()
    private let loadLock = NSLock()
    private var env: ORTEnv?
    private var session: ORTSession?
    private var inputName: String?

    private static let contextLen = 77
    private let log: @Sendable (String) -> Void

    /// The engine passes its own logger: NSLog writes to stderr, which is the
    /// engine's IPC event stream.
    public init(log: @escaping @Sendable (String) -> Void = { NSLog("%@", $0) }) {
        self.log = log
    }

    public var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return session != nil
    }

    /// Model file present on disk even though the ORT session may still
    /// be compiling — lets the UI tell "install CLIP" from "wait for load".
    public var isInstalled: Bool {
        FileManager.default.fileExists(atPath: CLIPEmbeddingSpace.textModelURL.path)
    }

    /// Loads the ViT-B/32 text ONNX and BPE vocabulary; true iff both loaded.
    @discardableResult
    public func load() -> Bool {
        lock.lock(); let already = (session != nil); lock.unlock()
        if already { return true }

        // Serialize loads without holding `lock` during the build, so isReady and embedText stay responsive.
        loadLock.lock(); defer { loadLock.unlock() }
        lock.lock(); let nowLoaded = (session != nil); lock.unlock()
        if nowLoaded { return true }

        let dir = CLIPEmbeddingSpace.textModelDirectory
        let modelURL = CLIPEmbeddingSpace.textModelURL
        guard FileManager.default.fileExists(atPath: modelURL.path) else {
            log("FileID CLIP text: clip_text.onnx not found at \(redactPathForLog(modelURL.path))")
            return false
        }
        let started = CFAbsoluteTimeGetCurrent()
        do {
            let built = try ModelArtifactVerification.load(verifiedBy: {
                CLIPEmbeddingSpace.verifyArtifact(at: modelURL, id: "clip_vitb32_text")
                    && CLIPEmbeddingSpace.verifyArtifact(at: dir.appendingPathComponent("vocab.json"), id: "clip_bpe_vocab")
                    && CLIPEmbeddingSpace.verifyArtifact(at: dir.appendingPathComponent("merges.txt"), id: "clip_bpe_merges")
            }) { () throws -> (ORTEnv, ORTSession, String?)? in
                guard CLIPTokenizer.shared.loadVocabulary(modelDirectory: dir) else { return nil }
                lock.lock(); let existingEnv = self.env; lock.unlock()
                let env = try existingEnv ?? ORTPrivacy.makeEnv(loggingLevel: ORTLoggingLevel.warning)
                // CPU only: the CoreML provider splits this graph into ~110 partitions, which took ~3.5 s per launch and ran no faster.
                let session = try ORTSession(env: env, modelPath: modelURL.path, sessionOptions: try ORTSessionOptions())
                return (env, session, try session.inputNames().first)
            }
            guard let verified = built else {
                log("FileID CLIP text: artifact verification failed; semantic search unavailable")
                return false
            }
            guard let loaded = verified else {
                log("FileID CLIP text: vocab.json or merges.txt not found in \(redactPathForLog(dir.path))")
                return false
            }
            lock.lock()
            self.env = loaded.0
            self.session = loaded.1
            self.inputName = loaded.2
            lock.unlock()
            log("FileID CLIP text: loaded ViT-B/32 ONNX from \(redactPathForLog(modelURL.path)) in \(Int((CFAbsoluteTimeGetCurrent() - started) * 1000)) ms")
            return true
        } catch {
            log("FileID CLIP text load failed: \(error)")
            return false
        }
    }

    /// Tears down the ORT session so search stops querying a model the user just uninstalled. `isReady` becomes false and `embedText` returns nil until `load()` rebuilds.
    /// Takes `loadLock` so it can't race a build in progress.
    public func unload() {
        loadLock.lock(); defer { loadLock.unlock() }
        lock.lock()
        session = nil
        inputName = nil
        lock.unlock()
        // `env` is cheap to retain and reused if the user reinstalls CLIP.
    }

    /// Embeds a free-text query into the CLIP image-embedding space. L2-normalized; nil if not ready or inference fails.
    public func embedText(_ query: String) -> [Float]? {
        guard load() else { return nil }
        guard let tokens = CLIPTokenizer.shared.encode(query) else { return nil }
        lock.lock(); let s = session; let name = inputName; lock.unlock()
        guard let s, let name else { return nil }

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
            log("FileID CLIP text inference failed: \(error)")
            return nil
        }
    }
}
