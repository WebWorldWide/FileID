// ArcFace face embedder. Loads either iResNet50 (Buffalo-L) or MobileFace
// (Buffalo-S) from a CoreML .mlpackage. Both produce L2-normalized 512-d
// float32 embeddings from 112×112 RGB face crops with ImageNet-style
// signed normalization (pixel - 127.5) / 127.5.
//
// Same load + concurrency shape as MobileCLIPService:
//  - Double-checked locking so 14 concurrent first callers don't race the
//    initial MLModel(contentsOf:) compile.
//  - `.computeUnits = .all` so CoreML routes to ANE when shapes allow.
//  - DispatchSemaphore caps in-flight predictions at 4 so the ANE doesn't
//    thrash with many parallel embed() calls (the v1 Batch 17/18 lesson).
//  - 112×112 input is hardcoded — InsightFace's ArcFace family was
//    trained on this exact resolution; larger inputs waste cycles.
import Foundation
import CoreML
import FileIDShared

public final class ArcFaceService: @unchecked Sendable {
    public static let shared = ArcFaceService()

    private let lock = NSLock()
    private let loadLock = NSLock()
    private var model: MLModel?
    private var loadedKind: FaceEmbedderKind?
    private let inferenceSem = DispatchSemaphore(value: 4)

    private init() {}

    // MARK: - Paths

    /// Application Support directory where face embedder .mlpackages live.
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
    /// false if the .mlpackage isn't on disk yet (caller should fall
    /// through gracefully — face detection still works without an
    /// embedder, the row just doesn't get an arcface_embedding).
    @discardableResult
    public func load(_ kind: FaceEmbedderKind) -> Bool {
        // Fast path — already loaded with the right kind.
        lock.lock()
        if let loaded = loadedKind, loaded == kind, model != nil {
            lock.unlock(); return true
        }
        lock.unlock()

        loadLock.lock()
        defer { loadLock.unlock() }
        // Re-check under load lock — another thread may have finished.
        lock.lock()
        if let loaded = loadedKind, loaded == kind, model != nil {
            lock.unlock(); return true
        }
        lock.unlock()

        let url = Self.modelURL(for: kind)
        guard FileManager.default.fileExists(atPath: url.path) else {
            JSONLog.shared.warn(ev: "arcface_model_missing",
                                path: url.path,
                                error: "ArcFace .mlpackage not present; face embedding skipped")
            return false
        }
        let config = MLModelConfiguration()
        config.computeUnits = .all
        let loaded: MLModel
        do {
            let compiled = try MLModel.compileModel(at: url)
            loaded = try MLModel(contentsOf: compiled, configuration: config)
        } catch {
            JSONLog.shared.error(ev: "arcface_model_load_failed",
                                 path: url.path, error: "\(error)")
            return false
        }
        lock.lock()
        self.model = loaded
        self.loadedKind = kind
        lock.unlock()
        JSONLog.shared.info(ev: "arcface_model_loaded",
                            extra: ["kind": AnyCodable(kind.rawValue),
                                    "path": AnyCodable(url.path)])
        return true
    }

    public var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return model != nil
    }

    public var currentKind: FaceEmbedderKind? {
        lock.lock(); defer { lock.unlock() }
        return loadedKind
    }

    // MARK: - Inference

    /// Returns an L2-normalized 512-d embedding for the supplied face
    /// crop. Returns nil if the model isn't loaded or inference failed.
    public func embed(_ crop: CGImage) -> [Float]? {
        lock.lock(); let m = model; lock.unlock()
        guard let m else { return nil }
        guard let pixelBuffer = cgImageToPixelBuffer(crop, size: 112) else { return nil }
        guard let input = try? MLDictionaryFeatureProvider(dictionary: ["input": pixelBuffer]) else {
            return nil
        }

        inferenceSem.wait()
        defer { inferenceSem.signal() }

        guard let pred = try? m.prediction(from: input),
              let arr = firstMultiArray(in: pred), arr.count > 0 else { return nil }
        var out = [Float](repeating: 0, count: arr.count)
        for i in 0..<arr.count { out[i] = arr[i].floatValue }
        return l2Normalize(out)
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

    // MARK: - Internals

    private func firstMultiArray(in provider: MLFeatureProvider) -> MLMultiArray? {
        for name in provider.featureNames {
            if let v = provider.featureValue(for: name)?.multiArrayValue { return v }
        }
        return nil
    }

    private func l2Normalize(_ v: [Float]) -> [Float] {
        var sum: Float = 0
        for x in v { sum += x * x }
        let norm = sum.squareRoot()
        guard norm > 0 else { return v }
        return v.map { $0 / norm }
    }

    /// 112×112 BGRA pixel buffer for the CoreML model. Note: the
    /// conversion script must bake `(pixel - 127.5) / 127.5` normalization
    /// into the model graph (via coremltools `ImageType` with bias/scale).
    /// If conversion was done without that, callers will see consistently
    /// wrong embeddings — fix in conversion, NOT here, so that we keep the
    /// pixel buffer path identical to MobileCLIP.
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
            bitmapInfo: CGImageAlphaInfo.noneSkipFirst.rawValue
                | CGBitmapInfo.byteOrder32Little.rawValue
        ) else { return nil }
        ctx.draw(cgImage, in: CGRect(x: 0, y: 0, width: size, height: size))
        return buffer
    }
}
