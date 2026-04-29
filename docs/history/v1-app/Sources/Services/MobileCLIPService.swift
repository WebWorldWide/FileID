import Foundation
import CoreML
import Vision
import CoreImage

// MARK: - MobileCLIPService

// Apple MobileCLIP S2: 512-d image/text embeddings for zero-shot classification.
// Weights are downloaded on demand; missing weights degrade to a quiet no-op.
final class MobileCLIPService: @unchecked Sendable {
    static let shared = MobileCLIPService()

    // "a photo of …" prefix matches CLIP's training distribution; bare labels
    // score measurably worse.
    static let labelVocabulary: [String] = [
        "a photo of a person", "a portrait photograph", "a group of people",
        "a landscape photograph", "a mountain view", "a beach scene",
        "a forest", "a field of flowers", "a river or lake",
        "a city skyline", "a street scene", "a sunset", "a sunrise",
        "a photo taken at night", "the night sky", "the moon",
        "a close-up of flowers", "a garden",
        "a pet dog", "a pet cat", "a bird", "wildlife", "a farm animal",
        "food on a plate", "a plate of pasta", "a sandwich", "a dessert",
        "a cocktail", "a cup of coffee",
        "a screenshot of a computer", "a screenshot of a phone",
        "a document scan", "a receipt", "an invoice", "a chart or graph",
        "a handwritten note",
        "a car", "an airplane", "a boat", "a bicycle", "a train",
        "an interior room", "a living room", "a kitchen", "a bedroom",
        "an office desk",
        "a baby", "a wedding", "a birthday party", "a concert",
        "a sports game", "indoor sports", "outdoor sports",
        "abstract art", "a painting", "a sculpture", "a meme"
    ]

    private let lock = NSLock()
    private var imageModel: MLModel?
    private var textModel:  MLModel?
    private var textEmbeddings: [String: [Float]] = [:]
    private var isImageLoaded = false
    private var isTextLoaded  = false
    private var lastLoadAttempt: Date?

    // Separate locks gating the slow MLModel(contentsOf:) call. Without these,
    // the previous design only locked the *flag assignment* — meaning N
    // simultaneous callers would all enter compileAndLoad and run
    // MLModel(contentsOf:) in parallel. On a 16 GB Mac with 14 scan workers,
    // 14× concurrent ~100 MB model loads + Metal/ANE compilation = thrashing
    // and the 21 → 0.2 files/s collapse Batch 17's eager preload triggered.
    // These locks ensure exactly one load runs at a time per encoder; later
    // callers see `is*Loaded == true` on the fast path.
    private let imageLoadLock = NSLock()
    private let textLoadLock  = NSLock()

    private init() {}

    // MARK: - Loading

    @discardableResult
    func loadImageEncoder() -> Bool {
        // Fast path — already loaded.
        lock.lock()
        if isImageLoaded, imageModel != nil { lock.unlock(); return true }
        lock.unlock()

        // Slow path — serialize. Only one thread compiles; later threads
        // discover the fast path on their next attempt.
        imageLoadLock.lock()
        defer { imageLoadLock.unlock() }
        // Re-check inside the load lock — another thread may have finished
        // while we were waiting for the lock.
        lock.lock()
        if isImageLoaded, imageModel != nil { lock.unlock(); return true }
        lock.unlock()

        guard let url = locateModel(kind: .mobileCLIPImage) else { return false }
        return compileAndLoad(url: url, assignTo: .image)
    }

    @discardableResult
    func loadTextEncoder() -> Bool {
        // Fast path — already loaded.
        lock.lock()
        if isTextLoaded, textModel != nil { lock.unlock(); return true }
        lock.unlock()

        // Slow path — serialize.
        textLoadLock.lock()
        defer { textLoadLock.unlock() }
        lock.lock()
        if isTextLoaded, textModel != nil { lock.unlock(); return true }
        lock.unlock()

        guard let url = locateModel(kind: .mobileCLIPText) else { return false }
        let ok = compileAndLoad(url: url, assignTo: .text)
        if ok { precomputeTextEmbeddings() }
        return ok
    }

    enum EncoderSlot { case image, text }

    private func compileAndLoad(url: URL, assignTo slot: EncoderSlot) -> Bool {
        let config = MLModelConfiguration()
        config.computeUnits = .all
        guard let model = try? MLModel(contentsOf: url, configuration: config) else {
            return false
        }
        lock.lock()
        switch slot {
        case .image: imageModel = model; isImageLoaded = true
        case .text:  textModel  = model; isTextLoaded  = true
        }
        lock.unlock()
        return true
    }

    private func locateModel(kind: AIModelKind) -> URL? {
        let downloaded = kind.descriptor.primaryFileURL
        if FileManager.default.fileExists(atPath: downloaded.path) {
            return downloaded
        }
        let bundleName = kind == .mobileCLIPImage ? "mobileclip_s2_image" : "mobileclip_s2_text"
        return Bundle.main.url(forResource: bundleName, withExtension: "mlpackage")
    }

    // MARK: - Inference

    func embedImage(_ cgImage: CGImage) -> [Float]? {
        guard loadImageEncoder() else { return nil }
        lock.lock(); let model = imageModel; lock.unlock()
        guard let model else { return nil }

        guard let pixelBuffer = cgImageToPixelBuffer(cgImage, size: 256) else { return nil }
        guard let input = try? MLDictionaryFeatureProvider(dictionary: ["image": pixelBuffer]) else {
            return nil
        }
        guard let pred = try? model.prediction(from: input),
              let arr  = firstMultiArray(in: pred) else { return nil }

        // A malformed CoreML model can hand back a zero-length MultiArray —
        // returning [Float]() here would silently disable zero-shot CLIP for
        // every downstream call. Treat as load failure so the pipeline knows
        // the embedding is unusable.
        guard arr.count > 0 else { return nil }
        var out = [Float](repeating: 0, count: arr.count)
        for i in 0..<arr.count { out[i] = arr[i].floatValue }
        return normalize(out)
    }

    // Top-K cosine match vs. cached text embeddings — runs the image encoder.
    // Prefer `classify(usingEmbedding:)` if you've already embedded the image
    // for `clipEmbedding` storage; this overload exists for one-shot callers.
    func classify(_ cgImage: CGImage, topK: Int = 5) -> [(String, Float)] {
        guard let imgVec = embedImage(cgImage) else { return [] }
        return classify(usingEmbedding: imgVec, topK: topK)
    }

    // Same as `classify(_:topK:)` but reuses a precomputed image embedding.
    // MediaProcessor's scan path embeds once per file for the persisted
    // `clipEmbedding` field — calling this overload avoids running the
    // ~100–200 ms image encoder a second time just to score labels.
    func classify(usingEmbedding imgVec: [Float], topK: Int = 5) -> [(String, Float)] {
        _ = loadTextEncoder()

        lock.lock()
        let embeddings = textEmbeddings
        lock.unlock()
        guard !embeddings.isEmpty else { return [] }

        // Bounded ascending-sorted array: O(N log K) vs O(N log N) full sort.
        var top: [(String, Float)] = []
        top.reserveCapacity(topK + 1)
        for (label, txtVec) in embeddings {
            let score = cosine(imgVec, txtVec)
            if top.count < topK {
                let idx = top.firstIndex(where: { $0.1 > score }) ?? top.count
                top.insert((label, score), at: idx)
            } else if score > top[0].1 {
                top.removeFirst()
                let idx = top.firstIndex(where: { $0.1 > score }) ?? top.count
                top.insert((label, score), at: idx)
            }
        }
        return top.reversed()
    }

    // MARK: - Text embedding cache

    private func precomputeTextEmbeddings() {
        lock.lock()
        guard textEmbeddings.isEmpty, let model = textModel else { lock.unlock(); return }
        lock.unlock()

        var out: [String: [Float]] = [:]
        for label in Self.labelVocabulary {
            guard let vec = runTextEncoder(label, model: model) else { continue }
            out[label] = vec
        }
        lock.lock()
        textEmbeddings = out
        lock.unlock()
    }

    private func runTextEncoder(_ text: String, model: MLModel) -> [Float]? {
        // TODO(docs/NEXT.md): port Apple's tokenizer. Without it, models that
        // expect token IDs return nil and zero-shot gracefully disables itself.
        guard let input = try? MLDictionaryFeatureProvider(dictionary: ["text": text]) else {
            return nil
        }
        guard let pred = try? model.prediction(from: input),
              let arr = firstMultiArray(in: pred) else { return nil }
        guard arr.count > 0 else { return nil }
        var out = [Float](repeating: 0, count: arr.count)
        for i in 0..<arr.count { out[i] = arr[i].floatValue }
        return normalize(out)
    }

    private func firstMultiArray(in provider: MLFeatureProvider) -> MLMultiArray? {
        for name in provider.featureNames {
            if let v = provider.featureValue(for: name)?.multiArrayValue { return v }
        }
        return nil
    }

    // MARK: - Math

    private func normalize(_ v: [Float]) -> [Float] {
        var sum: Float = 0
        for x in v { sum += x * x }
        let norm = sum.squareRoot()
        guard norm > 0 else { return v }
        return v.map { $0 / norm }
    }

    private func cosine(_ a: [Float], _ b: [Float]) -> Float {
        let n = min(a.count, b.count)
        var dot: Float = 0
        for i in 0..<n { dot += a[i] * b[i] }
        return dot
    }

    // MARK: - CGImage → CVPixelBuffer

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

extension MobileCLIPService {
    func embed(_ cgImage: CGImage) -> [Float]? { embedImage(cgImage) }
}
