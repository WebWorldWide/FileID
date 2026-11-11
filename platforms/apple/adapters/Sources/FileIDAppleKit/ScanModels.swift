// Stage 5: CLIP image embeddings and RAM++ tags on the GPU through MLX, as the
// Swift engine's MobileCLIPService and RamPlusService run them: the same MLX
// graphs (FileIDImaging CLIPVisionMLX, RamPlusMLX) behind the same
// GPURequestBatcher, one queue and MLX stream per model so concurrent scan
// workers share GPU batches. Preprocessing and tag selection follow those
// services line for line. The Rust engine checks the model files against the
// manifest before it asks for a load; the ONNX Runtime fallback stays in Rust.
import CoreGraphics
import Foundation
import FileIDImaging

enum ScanGPU {
    /// `DeepAnalyzeCapability.metallibPresent`: MLX aborts the process without
    /// its Metal library, which it loads from beside the executable.
    static func metallibPresent() -> Bool {
        let dir = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath().deletingLastPathComponent()
        return ["mlx.metallib", "default.metallib"].contains {
            FileManager.default.fileExists(atPath: dir.appendingPathComponent($0).path)
        }
    }

    /// Resize to side x side through CoreGraphics (.high) and pack normalized
    /// float32 NCHW, as both services do.
    static func tensor(_ src: CGImage, side: Int, mean: [Float], std: [Float]) -> [Float]? {
        var rgba = [UInt8](repeating: 0, count: side * side * 4)
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let ctx = rgba.withUnsafeMutableBufferPointer({ buf -> CGContext? in
            CGContext(data: buf.baseAddress, width: side, height: side,
                      bitsPerComponent: 8, bytesPerRow: side * 4, space: cs,
                      bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue)
        }) else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(src, in: CGRect(x: 0, y: 0, width: side, height: side))
        let pixelCount = side * side
        var planes = [Float](repeating: 0, count: pixelCount * 3)
        for i in 0..<pixelCount {
            planes[i] = (Float(rgba[i * 4]) / 255.0 - mean[0]) / std[0]
            planes[pixelCount + i] = (Float(rgba[i * 4 + 1]) / 255.0 - mean[1]) / std[1]
            planes[2 * pixelCount + i] = (Float(rgba[i * 4 + 2]) / 255.0 - mean[2]) / std[2]
        }
        return planes
    }

    static func lines(_ path: String) -> [Substring]? {
        guard let data = FileManager.default.contents(atPath: path) else { return nil }
        return String(decoding: data, as: UTF8.self).split(omittingEmptySubsequences: false, whereSeparator: { $0.isNewline })
    }
}

final class ScanCLIP: @unchecked Sendable {
    static let shared = ScanCLIP()
    static let inputSize = 224
    static let embeddingDim = 512
    private static let mean: [Float] = [0.481_454_66, 0.457_827_5, 0.408_210_73]
    private static let std: [Float] = [0.268_629_54, 0.261_302_58, 0.275_777_11]
    private let lock = NSLock()
    private var batcher: CLIPGPUBatcher?
    private var loadedPath: String?

    func load(onnxPath: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if batcher != nil, loadedPath == onnxPath { return true }
        guard ProcessInfo.processInfo.environment["FILEID_CLIP_BACKEND"] != "onnx", ScanGPU.metallibPresent(),
              let encoder = try? CLIPVisionMLX(onnxURL: URL(fileURLWithPath: onnxPath)) else { return false }
        batcher = CLIPGPUBatcher(encoder: encoder)
        loadedPath = onnxPath
        return true
    }

    func embed(_ image: CGImage) -> [Float]? {
        lock.lock()
        let batcher = batcher
        lock.unlock()
        guard let batcher, let tensor = ScanGPU.tensor(image, side: Self.inputSize, mean: Self.mean, std: Self.std),
              let raw = batcher.embed(tensor), raw.count == Self.embeddingDim else { return nil }
        let norm = raw.reduce(Float(0)) { $0 + $1 * $1 }.squareRoot()
        return norm > 0 ? raw.map { $0 / norm } : raw
    }
}

final class ScanRamPlus: @unchecked Sendable {
    static let shared = ScanRamPlus()
    static let inputSize = 384
    private static let defaultThreshold: Float = 0.68
    private static let defaultMaxTags = 8
    private static let defaultPrecisionFloor: Float = 0.62
    private static let mean: [Float] = [0.485, 0.456, 0.406]
    private static let std: [Float] = [0.229, 0.224, 0.225]
    private static let gpuMaxBatch = 8
    static let suppressedBuiltin: Set<String> = [
        "image", "photo", "photograph", "photography", "picture", "face", "catch",
        "stand", "sit", "lay", "pose", "wear",
        "person", "people", "hair", "head", "hand", "play", "game", "make", "fill", "dark",
        "blue", "red", "white", "black", "green", "brown", "yellow", "pink", "gray", "grey", "purple",
    ]
    static let peopleWords: Set<String> = ["man", "woman", "boy", "girl", "child", "couple"]
    static let redundantWith: [String: String] = ["goggles": "sunglasses"]

    private struct Vocabulary {
        let tags: [String]
        let perClass: [Float]?
        let globalThreshold: Float
        let precisionFloor: Float
        let suppressExtra: Set<String>
    }

    private let lock = NSLock()
    private var vocabulary: Vocabulary?
    private var batcher: GPURequestBatcher?
    private var loadedDir: String?

    /// `RamPlusService.load` with the GPU backend: ram_plus.onnx and
    /// ram_plus_tags.txt, optional per-class thresholds and suppress list.
    func load(directory: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if batcher != nil, loadedDir == directory { return true }
        let dir = URL(fileURLWithPath: directory, isDirectory: true)
        guard ProcessInfo.processInfo.environment["FILEID_RAMPLUS_BACKEND"] != "onnx", ScanGPU.metallibPresent(),
              let vocabulary = Self.loadVocabulary(dir) else { return false }
        guard let model = try? RamPlusMLX(onnxURL: dir.appendingPathComponent("ram_plus.onnx")) else { return false }
        batcher = GPURequestBatcher(label: "fileid.ramplus.gpu", maxBatch: Self.gpuMaxBatch, gather: .milliseconds(5)) {
            model.logits(pixels: $0, count: $1)
        }
        self.vocabulary = vocabulary
        loadedDir = directory
        return true
    }

    private static func loadVocabulary(_ dir: URL) -> Vocabulary? {
        guard let parsed = ScanGPU.lines(dir.appendingPathComponent("ram_plus_tags.txt").path)?
            .map({ $0.trimmingCharacters(in: .whitespacesAndNewlines) }) else { return nil }
        let tags = parsed.last?.isEmpty == true ? Array(parsed.dropLast()) : parsed
        guard !tags.isEmpty, !tags.contains(where: { $0.isEmpty }) else { return nil }
        let env = ProcessInfo.processInfo.environment
        func envCut(_ key: String) -> Float? {
            env[key].flatMap { Float($0) }.flatMap { (0.0...1.0).contains($0) ? $0 : nil }
        }
        let envThreshold = envCut("FILEID_RAMPLUS_THRESHOLD")
        var perClass: [Float]?
        if envThreshold == nil, let lines = ScanGPU.lines(dir.appendingPathComponent("ram_plus_thresholds.txt").path) {
            let values = lines.filter { !$0.isEmpty }.compactMap { Float($0.trimmingCharacters(in: .whitespacesAndNewlines)) }
            if values.count == tags.count { perClass = values }
        }
        var extra: Set<String> = []
        for line in ScanGPU.lines(dir.appendingPathComponent("ram_plus_suppress.txt").path) ?? [] {
            let t = line.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            if !t.isEmpty, !t.hasPrefix("#") { extra.insert(t) }
        }
        return Vocabulary(tags: tags, perClass: perClass, globalThreshold: envThreshold ?? defaultThreshold,
                          precisionFloor: envCut("FILEID_RAMPLUS_PRECISION_FLOOR") ?? defaultPrecisionFloor,
                          suppressExtra: extra)
    }

    /// `RamPlusService.tag`; nil when the model is not loaded or inference failed.
    func tag(_ image: CGImage) -> [(tag: String, score: Float)]? {
        lock.lock()
        let batcher = batcher
        let vocabulary = vocabulary
        lock.unlock()
        guard let batcher, let vocabulary,
              let tensor = ScanGPU.tensor(image, side: Self.inputSize, mean: Self.mean, std: Self.std),
              let logits = batcher.submit(tensor), logits.count == vocabulary.tags.count else { return nil }
        return Self.select(logits, vocabulary)
    }

    private static func select(_ logits: [Float], _ v: Vocabulary) -> [(tag: String, score: Float)] {
        var hits: [(Int, Float)] = []
        for (i, z) in logits.enumerated() {
            let lower = v.tags[i].lowercased()
            if suppressedBuiltin.contains(lower) || (!v.suppressExtra.isEmpty && v.suppressExtra.contains(lower)) { continue }
            let p = 1.0 / (1.0 + Float(Foundation.exp(-Double(z))))
            if p >= max(v.perClass?[i] ?? v.globalThreshold, v.precisionFloor) { hits.append((i, p)) }
        }
        hits.sort { $0.1 > $1.1 }
        var refined = refine(hits.map { (v.tags[$0.0], $0.1) })
        if refined.count > defaultMaxTags { refined.removeLast(refined.count - defaultMaxTags) }
        return refined
    }

    /// `RamPlusService.refine`.
    static func refine(_ hits: [(tag: String, score: Float)]) -> [(tag: String, score: Float)] {
        let names = Set(hits.map { $0.tag.lowercased() })
        let heads = Set(names.compactMap { name -> String? in
            guard let space = name.lastIndex(of: " ") else { return nil }
            return String(name[name.index(after: space)...])
        })
        var keptPeopleWord = false
        return hits.filter { hit in
            let name = hit.tag.lowercased()
            if !name.contains(" "), heads.contains(name) { return false }
            if let partner = redundantWith[name], names.contains(partner) { return false }
            if peopleWords.contains(name) {
                if keptPeopleWord { return false }
                keptPeopleWord = true
            }
            return true
        }
    }
}

@_cdecl("fileid_apple_clip_load")
public func fileid_apple_clip_load(_ onnxPath: UnsafePointer<CChar>?) -> Bool {
    guard let path = scanString(onnxPath) else { return false }
    return ScanCLIP.shared.load(onnxPath: path)
}

@_cdecl("fileid_apple_clip_embed")
public func fileid_apple_clip_embed(_ image: UInt64, _ out: UnsafeMutablePointer<Float>?, _ capacity: Int) -> Bool {
    guard let out, capacity >= ScanCLIP.embeddingDim, let cgImage = ScanImages.shared.image(image),
          let vector = autoreleasepool(invoking: { ScanCLIP.shared.embed(cgImage) }) else { return false }
    vector.withUnsafeBufferPointer { out.update(from: $0.baseAddress!, count: vector.count) }
    return true
}

@_cdecl("fileid_apple_ramplus_load")
public func fileid_apple_ramplus_load(_ directory: UnsafePointer<CChar>?) -> Bool {
    guard let directory = scanString(directory) else { return false }
    return ScanRamPlus.shared.load(directory: directory)
}

/// RAM++ tags combined with the primary pass's classifier confidences and face
/// count (`TagAgreement.combine`). Returns [[tag, score], ...]; NULL when RAM++
/// is not loaded or failed on this image.
@_cdecl("fileid_apple_ramplus_tag")
public func fileid_apple_ramplus_tag(
    _ image: UInt64,
    _ visionConfidences: UnsafePointer<CChar>?,
    _ faceCount: Int
) -> UnsafeMutablePointer<CChar>? {
    guard let cgImage = ScanImages.shared.image(image),
          let tags = autoreleasepool(invoking: { ScanRamPlus.shared.tag(cgImage) }) else { return nil }
    var vision: [String: Float] = [:]
    if let data = scanString(visionConfidences)?.data(using: .utf8),
       let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] {
        for (key, value) in object { if let number = value as? NSNumber { vision[key] = number.floatValue } }
    }
    let combined = TagAgreement.combine(ram: tags, vision: vision, faceCount: faceCount, peopleWords: ScanRamPlus.peopleWords)
    return scanJSON(combined.map { [$0.tag, Double($0.score)] as [Any] })
}
