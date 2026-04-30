// CLIP image-encoder service. Loads Apple's MobileCLIP-S2 CoreML
// model from ~/Library/Application Support/FileID/Models/, runs
// 256×256 BGRA buffers through it, returns L2-normalized 512-d
// float vectors.
//
// Double-checked locking on load: without it, N concurrent first
// callers would race in compileAndLoad and thrash the ANE with
// parallel ~100 MB MLModel(contentsOf:) compiles. Tagging pre-warms
// at scan start so the lock is never contended in production.
//
// `@unchecked Sendable`: locks guard internals. The Tagging stage
// caps embed() concurrency to ~2 in-flight via AsyncSemaphore —
// flooding from 14 workers thrashes the ANE.
import Foundation
import CoreML

public final class MobileCLIPService: @unchecked Sendable {
    public static let shared = MobileCLIPService()

    private let lock = NSLock()
    private let imageLoadLock = NSLock()
    private var imageModel: MLModel?
    private var isImageLoaded = false

    // ANE concurrency bound. CoreML serializes ANE access internally, but
    // queueing too many simultaneous predictions causes thrash and latency
    // spikes. Iteration 2 (perf harness) showed ANE underutilized at 2;
    // bumped to 4. CoreML's internal queue still serializes when ANE saturates,
    // but lets the app submit faster so handoff isn't a stall.
    // DispatchSemaphore (sync) is fine here because embedImage is called from
    // GCD dispatch blocks (Tagging.visionQueue) which CAN be blocked briefly.
    private let inferenceSem = DispatchSemaphore(value: 4)

    private init() {}

    // MARK: - Loading

    /// Where v1 (and now v2) downloads MobileCLIP-S2 image weights to.
    /// If the user has already downloaded via v1, we reuse the same files.
    public static var defaultImageModelURL: URL {
        let base = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first!
            .appendingPathComponent("FileID/Models/mobileclip_image", isDirectory: true)
        return base.appendingPathComponent("mobileclip_s2_image.mlpackage")
    }

    /// Returns true if the model file is on disk and (after) successfully loaded.
    /// Returns false if the user hasn't downloaded the model yet — callers
    /// should silently degrade (no embedding) rather than failing the file.
    @discardableResult
    public func loadImageEncoder(at url: URL? = nil) -> Bool {
        // Fast path — already loaded.
        lock.lock()
        if isImageLoaded, imageModel != nil { lock.unlock(); return true }
        lock.unlock()

        // Slow path — serialize. Only one thread compiles; later threads
        // discover the fast path on their next attempt.
        imageLoadLock.lock()
        defer { imageLoadLock.unlock() }
        // Re-check under load lock — another thread may have finished while
        // we were waiting.
        lock.lock()
        if isImageLoaded, imageModel != nil { lock.unlock(); return true }
        lock.unlock()

        let modelURL = url ?? Self.defaultImageModelURL
        let safeModelPath = redactPathForLog(modelURL.path)
        guard FileManager.default.fileExists(atPath: modelURL.path) else {
            JSONLog.shared.warn(ev: "clip_model_missing", path: safeModelPath,
                                error: "MobileCLIP-S2 image weights not downloaded; embeddings disabled")
            return false
        }
        let config = MLModelConfiguration()
        config.computeUnits = .all
        // .mlpackage needs explicit compile → .mlmodelc; the implicit
        // path inside MLModel(contentsOf:) is unreliable under
        // sandboxing because /tmp may not be writable.
        let model: MLModel
        do {
            let compiledURL = try MLModel.compileModel(at: modelURL)
            model = try MLModel(contentsOf: compiledURL, configuration: config)
        } catch {
            JSONLog.shared.error(ev: "clip_model_load_failed", path: safeModelPath,
                                 error: "\(error)")
            return false
        }
        lock.lock()
        imageModel = model
        isImageLoaded = true
        lock.unlock()
        JSONLog.shared.info(ev: "clip_model_loaded", path: safeModelPath)
        return true
    }

    public var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return isImageLoaded && imageModel != nil
    }

    // MARK: - Inference

    /// Returns an L2-normalized 512-d image embedding, or nil if the model
    /// isn't loaded / inference failed. Concurrency is bounded internally by
    /// `inferenceSem` (2 in-flight) — call freely from any context.
    public func embedImage(_ cgImage: CGImage) -> [Float]? {
        guard loadImageEncoder() else { return nil }
        lock.lock(); let model = imageModel; lock.unlock()
        guard let model else { return nil }
        guard let pixelBuffer = cgImageToPixelBuffer(cgImage, size: 256) else { return nil }
        guard let input = try? MLDictionaryFeatureProvider(dictionary: ["image": pixelBuffer]) else {
            return nil
        }

        // Bound ANE concurrency. Without this, 14 workers all calling
        // model.prediction() simultaneously causes throughput to collapse
        // (the v1 Batch 17/18 lesson, repeated).
        inferenceSem.wait()
        defer { inferenceSem.signal() }

        guard let pred = try? model.prediction(from: input),
              let arr = firstMultiArray(in: pred) else { return nil }
        guard arr.count > 0 else { return nil }
        var out = [Float](repeating: 0, count: arr.count)
        for i in 0..<arr.count { out[i] = arr[i].floatValue }
        return normalize(out)
    }

    /// Pre-warm the model + ANE pipeline by running one inference on a tiny
    /// dummy image. Called from `runScan` BEFORE the worker pool starts so
    /// the first 14 concurrent requests don't all race the same first-load
    /// slow path. Safe to call multiple times — fast-paths after first warm.
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
        JSONLog.shared.info(ev: "clip_prewarmed",
                            extra: ["ms": AnyCodable(ms)])
    }

    /// Convert an embedding to the BLOB format the DBWriter inserts:
    /// raw little-endian Float32 bytes (length = dims * 4).
    public static func embeddingToBlob(_ vec: [Float]) -> Data {
        vec.withUnsafeBufferPointer { Data(buffer: $0) }
    }

    /// Inverse of `embeddingToBlob` — used by the read-side query path (M4)
    /// when computing similarity from blobs.
    public static func blobToEmbedding(_ data: Data) -> [Float] {
        let count = data.count / MemoryLayout<Float>.stride
        return data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> [Float] in
            let base = raw.baseAddress!.assumingMemoryBound(to: Float.self)
            return Array(UnsafeBufferPointer(start: base, count: count))
        }
    }

    // MARK: - Internals

    private func firstMultiArray(in provider: MLFeatureProvider) -> MLMultiArray? {
        for name in provider.featureNames {
            if let v = provider.featureValue(for: name)?.multiArrayValue { return v }
        }
        return nil
    }

    private func normalize(_ v: [Float]) -> [Float] {
        var sum: Float = 0
        for x in v { sum += x * x }
        let norm = sum.squareRoot()
        guard norm > 0 else { return v }
        return v.map { $0 / norm }
    }

    private func cgImageToPixelBuffer(_ cgImage: CGImage, size: Int) -> CVPixelBuffer? {
        var pb: CVPixelBuffer?
        let attrs: [CFString: Any] = [
            kCVPixelBufferCGImageCompatibilityKey: true,
            kCVPixelBufferCGBitmapContextCompatibilityKey: true
        ]
        let status = CVPixelBufferCreate(
            nil, size, size, kCVPixelFormatType_32BGRA,
            attrs as CFDictionary, &pb
        )
        guard status == kCVReturnSuccess, let buffer = pb else { return nil }
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        guard let ctx = CGContext(
            data: CVPixelBufferGetBaseAddress(buffer),
            width: size, height: size,
            bitsPerComponent: 8,
            bytesPerRow: CVPixelBufferGetBytesPerRow(buffer),
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
        ) else { return nil }
        ctx.draw(cgImage, in: CGRect(x: 0, y: 0, width: size, height: size))
        return buffer
    }
}
