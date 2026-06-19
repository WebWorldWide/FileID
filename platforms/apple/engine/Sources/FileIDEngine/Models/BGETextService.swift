// BGE-small document text embedder — byte-faithful with the Windows engine's
// models/bge_text.rs (same WordPiece tokenizer, same 256-token cap, same mean-pool +
// L2-normalize) so a document clusters identically across platforms (lockstep). Runs the
// `Xenova/bge-small-en-v1.5` ONNX through ONNX Runtime (CoreML EP) exactly like
// ArcFaceService. Used by the scan to embed document content into `text_embeddings`, which
// the restructure document pass clusters by.

import Foundation
import OnnxRuntimeBindings
import FileIDShared

// `@unchecked Sendable`: `lock` guards all mutable state; `loadLock` serializes the
// one-time session build; `inferenceSem` bounds concurrent ANE inferences. Mirrors
// MobileCLIPService's posture.
final class BGETextService: @unchecked Sendable {
    static let shared = BGETextService()

    private let lock = NSLock()
    /// Serializes the heavy one-time session build (double-checked against `lock`) so
    /// two racing first-callers don't each construct an ORTEnv+ORTSession and race the
    /// `env` write. Mirrors MobileCLIPService.imageLoadLock.
    private let loadLock = NSLock()
    /// Bounds concurrent CoreML inferences so a doc-heavy scan (up to `workerCap` doc
    /// workers all calling `embed`) can't flood the ANE. Mirrors ArcFaceService /
    /// MobileCLIPService (value: 4).
    private let inferenceSem = DispatchSemaphore(value: 4)
    private var env: ORTEnv?
    private var session: ORTSession?
    private var inputNames: [String] = []
    private var tokenizer: WordPieceTokenizer?

    /// BGE-small hidden size + token cap — must match bge_text.rs (HIDDEN / MAX_SEQ).
    private static let hidden = 384
    private static let maxSeq = 256

    /// Models directory the installer writes `bge_small.onnx` + `vocab.txt` into. The
    /// single source of truth for the path so discovery's "installed?" probe can't
    /// drift from the loader (both go through here).
    static var defaultModelDir: URL {
        ArcFaceService.modelsRoot.appendingPathComponent("bge_text", isDirectory: true)
    }

    /// True once the BGE ONNX is on disk. Discovery keeps embeddingless docs in the
    /// pipeline (for scan-time backfill) only when BGE can actually embed — otherwise
    /// it would re-walk every doc on every scan forever.
    static var isInstalledOnDisk: Bool {
        FileManager.default.fileExists(
            atPath: defaultModelDir.appendingPathComponent("bge_small.onnx").path)
    }

    var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return session != nil && tokenizer != nil
    }

    /// Load `bge_small.onnx` + `vocab.txt` from a models directory. Idempotent-ish:
    /// returns true once ready. Fail-soft — a missing model just leaves the service
    /// not-ready and document embedding is skipped (restructure falls back to filenames).
    @discardableResult
    func load(modelDir: URL) -> Bool {
        lock.lock()
        if session != nil, tokenizer != nil { lock.unlock(); return true }
        lock.unlock()

        // Serialize the heavy build so concurrent first-callers construct the session
        // exactly once (double-checked under `lock`). Without this, racing doc workers
        // each build an ORTEnv+ORTSession and race the `env`/`session` writes. Mirrors
        // MobileCLIPService.loadImageEncoder.
        loadLock.lock()
        defer { loadLock.unlock() }
        lock.lock()
        if session != nil, tokenizer != nil { lock.unlock(); return true }
        lock.unlock()

        let onnx = modelDir.appendingPathComponent("bge_small.onnx")
        let vocab = modelDir.appendingPathComponent("vocab.txt")
        guard FileManager.default.fileExists(atPath: onnx.path),
              let tok = WordPieceTokenizer(vocabFile: vocab) else {
            return false
        }
        do {
            lock.lock()
            let cachedEnv = self.env
            lock.unlock()
            let env = try cachedEnv ?? ORTEnv(loggingLevel: ORTLoggingLevel.warning)
            let opts = try ORTSessionOptions()
            let coreml = ORTCoreMLExecutionProviderOptions()
            coreml.enableOnSubgraphs = true
            coreml.useCPUAndGPU = true
            try opts.appendCoreMLExecutionProvider(with: coreml)
            let session = try ORTSession(env: env, modelPath: onnx.path, sessionOptions: opts)
            let names = try session.inputNames()
            lock.lock()
            self.env = env
            self.session = session
            self.inputNames = names
            self.tokenizer = tok
            lock.unlock()
            JSONLog.shared.info(ev: "bge_text_loaded",
                                extra: ["path": AnyCodable(redactPathForLog(onnx.path)),
                                        "inputs": AnyCodable(names.joined(separator: ","))])
            return true
        } catch {
            JSONLog.shared.error(ev: "bge_text_load_failed",
                                 path: redactPathForLog(onnx.path), error: "\(error)")
            return false
        }
    }

    /// Embed document text into an L2-normalized 384-d vector, or nil if not loaded /
    /// inference failed / the text is empty. Mean-pools the last hidden state over the
    /// attention mask — the canonical BGE-small pooling, matching bge_text.rs.
    func embed(_ text: String) -> [Float]? {
        lock.lock()
        let s = session
        let tok = tokenizer
        let names = inputNames
        lock.unlock()
        guard let s, let tok else { return nil }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return nil }

        let enc = tok.encode(trimmed, maxLen: Self.maxSeq)
        let n = enc.ids.count
        guard n >= 2 else { return nil }
        let shape: [NSNumber] = [1, NSNumber(value: n)]

        func int64Value(_ arr: [Int64]) throws -> ORTValue {
            var a = arr
            let data = a.withUnsafeMutableBufferPointer { buf in
                NSMutableData(bytes: buf.baseAddress, length: buf.count * MemoryLayout<Int64>.stride)
            }
            return try ORTValue(tensorData: data, elementType: .int64, shape: shape)
        }

        do {
            // Bind by name; some exports omit token_type_ids — only feed declared inputs.
            var inputs: [String: ORTValue] = [:]
            for name in names {
                switch name {
                case "input_ids": inputs[name] = try int64Value(enc.ids)
                case "attention_mask": inputs[name] = try int64Value(enc.attentionMask)
                case "token_type_ids": inputs[name] = try int64Value(enc.typeIds)
                default: break
                }
            }
            guard !inputs.isEmpty else { return nil }
            // Bound concurrent ANE inferences (mirrors ArcFace/MobileCLIP). Acquired
            // after the early-return guard so we never hold the slot on a nil path.
            inferenceSem.wait()
            defer { inferenceSem.signal() }
            let outputs = try s.run(withInputs: inputs,
                                    outputNames: Set(try s.outputNames()),
                                    runOptions: nil)
            // First output = last_hidden_state, shape [1, seq, 384] as float32.
            guard let first = outputs.values.first else { return nil }
            let outData = try first.tensorData() as Data
            let count = outData.count / MemoryLayout<Float>.stride
            let seq = count / Self.hidden
            guard seq == n, count == seq * Self.hidden else { return nil }

            var emb = [Float](repeating: 0, count: Self.hidden)
            var total: Float = 0
            outData.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
                let base = raw.bindMemory(to: Float.self)
                for t in 0..<seq {
                    let m = Float(enc.attentionMask[t])
                    if m == 0 { continue }
                    total += m
                    let row = t * Self.hidden
                    for h in 0..<Self.hidden { emb[h] += base[row + h] * m }
                }
            }
            if total > 0 { for i in 0..<Self.hidden { emb[i] /= total } }
            // L2-normalize.
            var norm: Float = 0
            for x in emb { norm += x * x }
            norm = norm.squareRoot()
            if norm > 1e-9 { for i in 0..<Self.hidden { emb[i] /= norm } }
            return emb
        } catch {
            JSONLog.shared.warn(ev: "bge_text_embed_failed", error: "\(error)")
            return nil
        }
    }
}
