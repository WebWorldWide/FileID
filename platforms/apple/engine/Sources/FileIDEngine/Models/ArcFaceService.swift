// ArcFace face embedder — Buffalo-L (iResNet50) or Buffalo-S
// (MobileFace) ONNX, run via ONNX Runtime with the CoreML execution
// provider (ANE acceleration on Apple Silicon).
//
// Why ONNX instead of CoreML: matches Immich's posture exactly. We
// pull the original Buffalo ONNX from the upstream Immich HuggingFace
// repo at runtime; we never redistribute the InsightFace pre-trained
// weights. Same legal posture, no on-device conversion step.
//
// Preprocessing — formerly baked into the CoreML graph via ImageType
// scale/bias — now happens here in Swift: resize face crop to 112×112
// RGB, normalize as (pixel − 127.5) / 127.5, pack as Float32 NCHW.
//
// Double-checked locking on load (avoid concurrent compile-and-load
// races from the worker pool); DispatchSemaphore caps in-flight
// predictions at 4 to keep the ANE from thrashing.
import Foundation
import CoreGraphics
import Accelerate
import FileIDShared
import OnnxRuntimeBindings

public final class ArcFaceService: @unchecked Sendable {
    public static let shared = ArcFaceService()

    private let lock = NSLock()
    private let loadLock = NSLock()
    private var env: ORTEnv?
    private var session: ORTSession?
    private var inputName: String?
    private var loadedKind: FaceEmbedderKind?
    private let inferenceSem = DispatchSemaphore(value: 4)

    private init() {}

    // MARK: - Paths

    /// Application Support directory where face embedder ONNX files live.
    /// Same parent directory as MobileCLIP so the user finds them in one
    /// place from Settings → Open Models folder.
    public static var modelsRoot: URL {
        FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first!
            .appendingPathComponent("FileID/Models", isDirectory: true)
    }

    public static func modelURL(for kind: FaceEmbedderKind) -> URL {
        modelsRoot.appendingPathComponent(kind.modelFileName)
    }

    public static func isInstalled(_ kind: FaceEmbedderKind) -> Bool {
        FileManager.default.fileExists(atPath: modelURL(for: kind).path)
    }

    // MARK: - Loading

    /// Load (or swap to) the requested embedder. Returns true on success;
    /// false if the .onnx isn't on disk yet (caller should fall through
    /// gracefully — face detection still works without an embedder, the
    /// row just doesn't get an arcface_embedding).
    @discardableResult
    public func load(_ kind: FaceEmbedderKind) -> Bool {
        // Fast path — already loaded with the right kind.
        lock.lock()
        if let loaded = loadedKind, loaded == kind, session != nil {
            lock.unlock(); return true
        }
        lock.unlock()

        loadLock.lock()
        defer { loadLock.unlock() }
        // Re-check under load lock — another thread may have finished.
        lock.lock()
        if let loaded = loadedKind, loaded == kind, session != nil {
            lock.unlock(); return true
        }
        lock.unlock()

        let url = Self.modelURL(for: kind)
        guard FileManager.default.fileExists(atPath: url.path) else {
            JSONLog.shared.warn(ev: "arcface_model_missing",
                                path: redactPathForLog(url.path),
                                error: "ArcFace .onnx not present; face embedding skipped")
            return false
        }

        do {
            // ORTEnv is process-wide; reuse across model swaps.
            let env: ORTEnv
            if let existing = self.env {
                env = existing
            } else {
                env = try ORTEnv(loggingLevel: ORTLoggingLevel.warning)
            }
            let opts = try ORTSessionOptions()
            // CoreML EP — enables ANE/GPU acceleration on Apple Silicon.
            // MLProgram = post-iOS15/macOS12 program format (faster init,
            // better op coverage than the legacy NeuralNetwork format).
            let coremlOpts = ORTCoreMLExecutionProviderOptions()
            coremlOpts.enableOnSubgraphs = true
            coremlOpts.useCPUAndGPU = true
            try opts.appendCoreMLExecutionProvider(with: coremlOpts)
            let session = try ORTSession(env: env, modelPath: url.path, sessionOptions: opts)
            // Discover input name — Buffalo ONNX uses "input.1" after
            // PyTorch tracing renames the original; mobileface may differ.
            let inputs = try session.inputNames()
            guard let firstInput = inputs.first else {
                JSONLog.shared.error(ev: "arcface_model_load_failed",
                                     path: redactPathForLog(url.path),
                                     error: "ONNX session reports no inputs")
                return false
            }
            lock.lock()
            self.env = env
            self.session = session
            self.inputName = firstInput
            self.loadedKind = kind
            lock.unlock()
            JSONLog.shared.info(ev: "arcface_model_loaded",
                                extra: ["kind": AnyCodable(kind.rawValue),
                                        "path": AnyCodable(redactPathForLog(url.path)),
                                        "input": AnyCodable(firstInput)])
            return true
        } catch {
            JSONLog.shared.error(ev: "arcface_model_load_failed",
                                 path: redactPathForLog(url.path), error: "\(error)")
            return false
        }
    }

    public var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return session != nil
    }

    public var currentKind: FaceEmbedderKind? {
        lock.lock(); defer { lock.unlock() }
        return loadedKind
    }

    // MARK: - Inference

    /// Returns an L2-normalized 512-d embedding for the supplied face
    /// crop. Returns nil if the model isn't loaded or inference failed.
    public func embed(_ crop: CGImage) -> [Float]? {
        lock.lock()
        let s = session
        let name = inputName
        lock.unlock()
        guard let s, let name else { return nil }
        guard let tensor = makeNCHWTensor(crop, side: 112) else { return nil }

        inferenceSem.wait()
        defer { inferenceSem.signal() }

        do {
            // Hand ORT a heap-allocated NSMutableData seeded with a copy
            // of the tensor bytes. The previous shape — `NSMutableData(
            // bytes: &tensor, length: …)` over a stack-allocated [Float]
            // — relied on ORTValue retaining the buffer for the lifetime
            // of the call. ORT's Swift bindings don't document copy-vs-
            // alias semantics, so we copy explicitly. ~150 KB extra per
            // face inference; immeasurable next to the ANE work.
            let nsData = tensor.withUnsafeBufferPointer { buf -> NSMutableData in
                NSMutableData(bytes: buf.baseAddress, length: buf.count * MemoryLayout<Float>.stride)
            }
            let shape: [NSNumber] = [1, 3, 112, 112]
            let value = try ORTValue(tensorData: nsData,
                                     elementType: .float,
                                     shape: shape)
            let outputs = try s.run(withInputs: [name: value],
                                    outputNames: Set(try s.outputNames()),
                                    runOptions: nil)
            guard let first = outputs.values.first else { return nil }
            let outData = try first.tensorData() as Data
            let count = outData.count / MemoryLayout<Float>.stride
            var floats = [Float](repeating: 0, count: count)
            outData.withUnsafeBytes { raw in
                let src = raw.baseAddress!.assumingMemoryBound(to: Float.self)
                for i in 0..<count { floats[i] = src[i] }
            }
            return l2Normalize(floats)
        } catch {
            JSONLog.shared.error(ev: "arcface_inference_failed", error: "\(error)")
            return nil
        }
    }

    /// Pre-warm the model + ANE pipeline. Call before the worker pool
    /// starts so the first 14 concurrent requests don't all race the
    /// same first-load path.
    public func preWarm(_ kind: FaceEmbedderKind) {
        guard load(kind) else { return }
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let ctx = CGContext(
            data: nil, width: 32, height: 32, bitsPerComponent: 8,
            bytesPerRow: 0, space: cs,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ), let img = ctx.makeImage() else { return }
        let started = CFAbsoluteTimeGetCurrent()
        _ = embed(img)
        let ms = (CFAbsoluteTimeGetCurrent() - started) * 1000
        JSONLog.shared.info(ev: "arcface_prewarmed",
                            extra: ["ms": AnyCodable(ms),
                                    "kind": AnyCodable(kind.rawValue)])
    }

    /// Encode a 512-d float32 embedding as a raw little-endian blob for
    /// the DB. Symmetric with `MobileCLIPService.embeddingToBlob`.
    public static func embeddingToBlob(_ vec: [Float]) -> Data {
        vec.withUnsafeBufferPointer { Data(buffer: $0) }
    }

    public static func blobToEmbedding(_ data: Data) -> [Float] {
        let count = data.count / MemoryLayout<Float>.stride
        return data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> [Float] in
            let base = raw.baseAddress!.assumingMemoryBound(to: Float.self)
            return Array(UnsafeBufferPointer(start: base, count: count))
        }
    }

    // MARK: - Preprocessing

    /// Resize the face crop to side×side RGB and pack as a Float32 NCHW
    /// tensor with `(pixel − 127.5) / 127.5` normalization per channel.
    /// Output layout: [1, 3, side, side] flattened row-major (C-major).
    private func makeNCHWTensor(_ src: CGImage, side: Int) -> [Float]? {
        // Resize via CGContext into an RGBA8 buffer (no alpha — premul
        // skipped via noneSkipLast so colour stays untouched).
        let bytesPerPixel = 4
        let bytesPerRow = side * bytesPerPixel
        var rgba = [UInt8](repeating: 0, count: side * side * bytesPerPixel)
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let ctx = rgba.withUnsafeMutableBufferPointer({ buf -> CGContext? in
            CGContext(
                data: buf.baseAddress, width: side, height: side,
                bitsPerComponent: 8, bytesPerRow: bytesPerRow, space: cs,
                bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
                    | CGBitmapInfo.byteOrder32Big.rawValue
            )
        }) else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(src, in: CGRect(x: 0, y: 0, width: side, height: side))

        // Re-read the rendered pixels (CGContext writes into our buffer).
        // Re-grab via the same closure pattern would re-allocate; just
        // reuse rgba which was filled by ctx.draw.
        let pixelCount = side * side
        var planes = [Float](repeating: 0, count: pixelCount * 3)
        // Channel order: 0=R, 1=G, 2=B (RGBX in source). Plane stride =
        // pixelCount; spatial stride = 1.
        let bias: Float = -1.0
        let scale: Float = 1.0 / 127.5
        for i in 0..<pixelCount {
            let r = Float(rgba[i * 4 + 0])
            let g = Float(rgba[i * 4 + 1])
            let b = Float(rgba[i * 4 + 2])
            planes[0 * pixelCount + i] = r * scale + bias
            planes[1 * pixelCount + i] = g * scale + bias
            planes[2 * pixelCount + i] = b * scale + bias
        }
        return planes
    }

    // MARK: - Internals

    private func l2Normalize(_ v: [Float]) -> [Float] {
        var sum: Float = 0
        for x in v { sum += x * x }
        let norm = sum.squareRoot()
        guard norm > 0 else { return v }
        return v.map { $0 / norm }
    }
}
