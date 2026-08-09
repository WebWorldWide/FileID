// CLIP image-encoder service. Commercial-clean: loads OpenAI/OpenCLIP
// ViT-B/32 (MIT) as ONNX and runs it via ONNX Runtime + CoreML EP — the
// same path `ArcFaceService` uses. Replaces Apple's MobileCLIP-S2 CoreML
// model (research-only license). Input is 224×224 RGB with CLIP mean/std
// normalization (matches the Windows engine's `models/mobileclip.rs`);
// output is an L2-normalized 512-d float vector — unchanged dimension, so
// the `clip_embeddings` schema and all cosine comparisons stay the same.
//
// `@unchecked Sendable`: locks guard internals. `inferenceSem` bounds ANE
// concurrency at 4 (flooding from 14 workers thrashes the ANE).
import Foundation
import CoreGraphics
import FileIDShared
import OnnxRuntimeBindings

public final class MobileCLIPService: @unchecked Sendable {
    public static let shared = MobileCLIPService()

    private let lock = NSLock()
    private let imageLoadLock = NSLock()
    private var env: ORTEnv?
    private var session: ORTSession?
    private var inputName: String?
    private var isImageLoaded = false
    private let inferenceSem = DispatchSemaphore(value: 4)

    // CLIP (OpenAI) preprocessing constants — identical to the Windows engine.
    private static let inputSize = 224
    private static let mean: [Float] = [0.481_454_66, 0.457_827_5, 0.408_210_73]
    private static let std: [Float] = [0.268_629_54, 0.261_302_58, 0.275_777_11]

    private init() {}

    // MARK: - Loading

    /// ViT-B/32 vision encoder ONNX path. Kept under the existing
    /// `mobileclip_image` directory so we don't churn the install layout;
    /// the file is now the OpenCLIP ViT-B/32 vision model.
    public static var defaultImageModelURL: URL {
        FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first!
            .appendingPathComponent("FileID/Models/mobileclip_image", isDirectory: true)
            .appendingPathComponent("clip_vitb32_image.onnx")
    }

    /// Returns true if the ONNX is on disk and successfully loaded. Returns
    /// false if the user hasn't downloaded it yet — callers degrade silently.
    @discardableResult
    public func loadImageEncoder(at url: URL? = nil) -> Bool {
        lock.lock()
        if isImageLoaded, session != nil { lock.unlock(); return true }
        lock.unlock()

        imageLoadLock.lock()
        defer { imageLoadLock.unlock() }
        lock.lock()
        if isImageLoaded, session != nil { lock.unlock(); return true }
        lock.unlock()

        let modelURL = url ?? Self.defaultImageModelURL
        let safe = redactPathForLog(modelURL.path)
        guard FileManager.default.fileExists(atPath: modelURL.path) else {
            JSONLog.shared.warn(ev: "clip_model_missing", path: safe,
                                error: "CLIP ViT-B/32 image ONNX not downloaded; embeddings disabled")
            return false
        }
        do {
            let env = try self.env ?? ORTEnv(loggingLevel: ORTLoggingLevel.warning)
            let opts = try ORTSessionOptions()
            // CoreML EP for ANE acceleration; ORT falls back to CPU if it
            // can't place a node. Mirrors ArcFaceService's posture.
            try? opts.appendCoreMLExecutionProvider(with: ORTCoreMLExecutionProviderOptions())
            let session = try ORTSession(env: env, modelPath: modelURL.path, sessionOptions: opts)
            let name = try session.inputNames().first
            lock.lock()
            self.env = env
            self.session = session
            self.inputName = name
            self.isImageLoaded = true
            lock.unlock()
            JSONLog.shared.info(ev: "clip_model_loaded", path: safe)
            return true
        } catch {
            JSONLog.shared.error(ev: "clip_model_load_failed", path: safe, error: "\(error)")
            return false
        }
    }

    public var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return isImageLoaded && session != nil
    }

    // MARK: - Inference

    /// Returns an L2-normalized 512-d image embedding, or nil if the model
    /// isn't loaded / inference failed.
    public func embedImage(_ cgImage: CGImage) -> [Float]? {
        guard loadImageEncoder() else { return nil }
        lock.lock(); let s = session; let name = inputName; lock.unlock()
        guard let s, let name else { return nil }
        guard let tensor = makeNCHWTensor(cgImage) else { return nil }

        inferenceSem.wait()
        defer { inferenceSem.signal() }

        do {
            let nsData = tensor.withUnsafeBufferPointer { buf -> NSMutableData in
                NSMutableData(bytes: buf.baseAddress, length: buf.count * MemoryLayout<Float>.stride)
            }
            let side = Self.inputSize as NSNumber
            let value = try ORTValue(tensorData: nsData, elementType: .float,
                                     shape: [1, 3, side, side])
            let outputs = try s.run(withInputs: [name: value],
                                    outputNames: Set(try s.outputNames()),
                                    runOptions: nil)
            guard let first = outputs.values.first else { return nil }
            let outData = try first.tensorData() as Data
            let count = outData.count / MemoryLayout<Float>.stride
            guard count > 0 else { return nil }
            var out = [Float](repeating: 0, count: count)
            outData.withUnsafeBytes { raw in
                let src = raw.baseAddress!.assumingMemoryBound(to: Float.self)
                for i in 0..<count { out[i] = src[i] }
            }
            return normalize(out)
        } catch {
            JSONLog.shared.error(ev: "clip_inference_failed", error: "\(error)")
            return nil
        }
    }

    /// Pre-warm the model + ANE pipeline. Call before the worker pool starts.
    public func preWarm() {
        guard loadImageEncoder() else { return }
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let ctx = CGContext(
            data: nil, width: 32, height: 32, bitsPerComponent: 8,
            bytesPerRow: 0, space: cs,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ), let img = ctx.makeImage() else { return }
        let started = CFAbsoluteTimeGetCurrent()
        _ = embedImage(img)
        let ms = (CFAbsoluteTimeGetCurrent() - started) * 1000
        JSONLog.shared.info(ev: "clip_prewarmed", extra: ["ms": AnyCodable(ms)])
    }

    /// Raw little-endian Float32 bytes (length = dims * 4).
    public static func embeddingToBlob(_ vec: [Float]) -> Data {
        vec.withUnsafeBufferPointer { Data(buffer: $0) }
    }

    public static func blobToEmbedding(_ data: Data) -> [Float] {
        let count = data.count / MemoryLayout<Float>.stride
        // S8: guard the empty/corrupt blob — a nil baseAddress force-unwrap
        // would crash the engine.
        guard count > 0 else { return [] }
        return data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> [Float] in
            guard let base = raw.baseAddress?.assumingMemoryBound(to: Float.self) else { return [] }
            return Array(UnsafeBufferPointer(start: base, count: count))
        }
    }

    // MARK: - Internals

    private func normalize(_ v: [Float]) -> [Float] {
        var sum: Float = 0
        for x in v { sum += x * x }
        let norm = sum.squareRoot()
        guard norm > 0 else { return v }
        return v.map { $0 / norm }
    }

    /// Resize to 224×224 RGB and pack CLIP-normalized Float32 NCHW
    /// ([1, 3, 224, 224], C-major). Normalization: (px/255 − mean) / std.
    private func makeNCHWTensor(_ src: CGImage) -> [Float]? {
        let side = Self.inputSize
        let bytesPerRow = side * 4
        var rgba = [UInt8](repeating: 0, count: side * side * 4)
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let ctx = rgba.withUnsafeMutableBufferPointer({ buf -> CGContext? in
            CGContext(data: buf.baseAddress, width: side, height: side,
                      bitsPerComponent: 8, bytesPerRow: bytesPerRow, space: cs,
                      bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
                          | CGBitmapInfo.byteOrder32Big.rawValue)
        }) else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(src, in: CGRect(x: 0, y: 0, width: side, height: side))

        let pixelCount = side * side
        var planes = [Float](repeating: 0, count: pixelCount * 3)
        let mean = Self.mean, std = Self.std
        for i in 0..<pixelCount {
            let r = Float(rgba[i * 4 + 0]) / 255.0
            let g = Float(rgba[i * 4 + 1]) / 255.0
            let b = Float(rgba[i * 4 + 2]) / 255.0
            planes[0 * pixelCount + i] = (r - mean[0]) / std[0]
            planes[1 * pixelCount + i] = (g - mean[1]) / std[1]
            planes[2 * pixelCount + i] = (b - mean[2]) / std[2]
        }
        return planes
    }
}
