// RAM++ (Recognize Anything Plus) multi-label image tagger — Swin-Large @ 384px,
// 4585-tag English vocabulary, Apache-2.0. Run via ONNX Runtime with the CoreML
// execution provider (ANE/GPU on Apple Silicon). 1:1 port of the Windows engine
// reference (models/ram_plus.rs) so macOS tags match the Windows commercial-clean
// stack instead of the weaker Apple Vision classifier.
//
// Contract (must match the Windows ONNX + export script):
//   input  "image"  [1, 3, 384, 384] f32, ImageNet mean/std normalized
//   output "logits" [1, 4585] f32 → sigmoid → per-class/global threshold (clamped
//          up to a precision floor) → suppress-list filter → sort desc → top-8.
//   confidence = sigmoid probability (0..1), persisted in tags.score.
//
// Files under FileID/Models/ram_plus/:
//   ram_plus.onnx            (required)
//   ram_plus_tags.txt        (required; 4585 lines, index-aligned with logits)
//   ram_plus_thresholds.txt  (optional; per-class f32 cutoffs, len must == tags)
//   ram_plus_suppress.txt    (optional; extra suppressed tags, one per line)
//
// Mirrors ArcFaceService's ORT/load/concurrency structure exactly.
import Foundation
import CoreGraphics
import FileIDShared
import OnnxRuntimeBindings

public final class RamPlusService: @unchecked Sendable {
    public static let shared = RamPlusService()

    // Defaults mirror ram_plus.rs (DEFAULT_THRESHOLD / DEFAULT_MAX_TAGS /
    // DEFAULT_PRECISION_FLOOR). Env overrides match the Windows knobs so a
    // threshold sweep behaves identically on both platforms.
    private static let inputSize = 384
    private static let defaultThreshold: Float = 0.68
    private static let defaultMaxTags = 8
    private static let defaultPrecisionFloor: Float = 0.62
    // ImageNet normalization (matches the RAM++ export script).
    private static let mean: [Float] = [0.485, 0.456, 0.406]
    private static let std: [Float] = [0.229, 0.224, 0.225]
    // Built-in suppress list — byte-identical to ram_plus.rs SUPPRESSED_TAGS.
    private static let suppressedBuiltin: Set<String> = [
        "image", "photo", "photograph", "photography", "picture", "face", "catch",
        "stand", "sit", "lay", "pose", "wear",
    ]

    private let lock = NSLock()
    private let loadLock = NSLock()
    private var env: ORTEnv?
    private var session: ORTSession?
    private var inputName: String?
    private var tags: [String] = []
    private var perClassThreshold: [Float]?
    private var globalThreshold: Float = RamPlusService.defaultThreshold
    private var precisionFloor: Float = RamPlusService.defaultPrecisionFloor
    private var maxTags: Int = RamPlusService.defaultMaxTags
    private var suppressExtra: Set<String> = []
    // Match ArcFace's ANE-thrash cap.
    private let inferenceSem = DispatchSemaphore(value: 4)

    private init() {}

    // MARK: - Paths

    public static var modelsRoot: URL { ArcFaceService.modelsRoot }
    private static var dir: URL { modelsRoot.appendingPathComponent("ram_plus", isDirectory: true) }
    public static var onnxURL: URL { dir.appendingPathComponent("ram_plus.onnx") }
    public static var tagsURL: URL { dir.appendingPathComponent("ram_plus_tags.txt") }
    private static var thresholdsURL: URL { dir.appendingPathComponent("ram_plus_thresholds.txt") }
    private static var suppressURL: URL { dir.appendingPathComponent("ram_plus_suppress.txt") }

    public static var isInstalled: Bool {
        FileManager.default.fileExists(atPath: onnxURL.path)
            && FileManager.default.fileExists(atPath: tagsURL.path)
    }

    // MARK: - Loading

    /// Load the RAM++ ONNX + tag list (+ optional sidecars). Returns true on
    /// success; false if the model/tag-list isn't on disk yet — the caller falls
    /// back to the CLIP/Vision scene tags, so tagging still works without RAM++.
    @discardableResult
    public func load() -> Bool {
        lock.lock()
        if session != nil, !tags.isEmpty { lock.unlock(); return true }
        lock.unlock()

        loadLock.lock()
        defer { loadLock.unlock() }
        lock.lock()
        if session != nil, !tags.isEmpty { lock.unlock(); return true }
        lock.unlock()

        guard Self.isInstalled else {
            JSONLog.shared.warn(ev: "ramplus_model_missing",
                                path: redactPathForLog(Self.onnxURL.path),
                                error: "RAM++ ONNX/tag list not present; tagging falls back to CLIP scene tags")
            return false
        }

        do {
            let tagsText = try String(contentsOf: Self.tagsURL, encoding: .utf8)
            let parsedTags = tagsText.split(separator: "\n", omittingEmptySubsequences: false)
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            // Keep trailing empties out, but DON'T filter interior blanks — the
            // index must stay aligned with the logit vector. A blank line in the
            // vocab is a malformed sidecar; bail rather than silently misalign.
            let trimmedTags = parsedTags.last?.isEmpty == true ? Array(parsedTags.dropLast()) : parsedTags
            guard !trimmedTags.isEmpty, !trimmedTags.contains(where: { $0.isEmpty }) else {
                JSONLog.shared.error(ev: "ramplus_model_load_failed",
                                     path: redactPathForLog(Self.tagsURL.path),
                                     error: "RAM++ tag list empty or has blank interior lines")
                return false
            }

            // Optional per-class thresholds sidecar; must match tag count or it's ignored.
            var perClass: [Float]?
            if let tText = try? String(contentsOf: Self.thresholdsURL, encoding: .utf8) {
                let vals = tText.split(separator: "\n").compactMap { Float($0.trimmingCharacters(in: .whitespaces)) }
                if vals.count == trimmedTags.count {
                    perClass = vals
                } else {
                    JSONLog.shared.warn(ev: "ramplus_threshold_sidecar_mismatch",
                                        error: "threshold sidecar count \(vals.count) != tags \(trimmedTags.count); using global cutoff")
                }
            }

            // Optional suppress sidecar (lowercased), merged with the built-in set.
            var extra: Set<String> = []
            if let sText = try? String(contentsOf: Self.suppressURL, encoding: .utf8) {
                for line in sText.split(separator: "\n") {
                    let t = line.trimmingCharacters(in: .whitespaces).lowercased()
                    if !t.isEmpty, !t.hasPrefix("#") { extra.insert(t) }
                }
            }

            // Env overrides (match ram_plus.rs FILEID_RAMPLUS_* knobs for sweeps).
            let env = ProcessInfo.processInfo.environment
            let globalT = env["FILEID_RAMPLUS_THRESHOLD"].flatMap { Float($0) } ?? Self.defaultThreshold
            let floor = env["FILEID_RAMPLUS_PRECISION_FLOOR"].flatMap { Float($0) } ?? Self.defaultPrecisionFloor

            let ortEnv: ORTEnv
            if let existing = self.env { ortEnv = existing }
            else { ortEnv = try ORTEnv(loggingLevel: ORTLoggingLevel.warning) }
            let opts = try ORTSessionOptions()
            let coremlOpts = ORTCoreMLExecutionProviderOptions()
            coremlOpts.enableOnSubgraphs = true
            coremlOpts.useCPUAndGPU = true
            try opts.appendCoreMLExecutionProvider(with: coremlOpts)
            let ortSession = try ORTSession(env: ortEnv, modelPath: Self.onnxURL.path, sessionOptions: opts)
            guard let firstInput = try ortSession.inputNames().first else {
                JSONLog.shared.error(ev: "ramplus_model_load_failed",
                                     path: redactPathForLog(Self.onnxURL.path),
                                     error: "ONNX session reports no inputs")
                return false
            }

            lock.lock()
            self.env = ortEnv
            self.session = ortSession
            self.inputName = firstInput
            self.tags = trimmedTags
            self.perClassThreshold = perClass
            self.globalThreshold = globalT
            self.precisionFloor = floor
            self.suppressExtra = extra
            lock.unlock()
            JSONLog.shared.info(ev: "ramplus_model_loaded",
                                extra: ["tags": AnyCodable(trimmedTags.count),
                                        "perClassThresholds": AnyCodable(perClass != nil),
                                        "input": AnyCodable(firstInput)])
            return true
        } catch {
            JSONLog.shared.error(ev: "ramplus_model_load_failed",
                                 path: redactPathForLog(Self.onnxURL.path), error: "\(error)")
            return false
        }
    }

    public var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return session != nil
    }

    // MARK: - Inference

    /// Tag an image. Returns (tag, sigmoid-probability) pairs, suppressed-filtered,
    /// thresholded, sorted desc, capped at maxTags. Empty if not loaded / on error.
    public func tag(_ image: CGImage) -> [(tag: String, score: Float)] {
        lock.lock()
        let s = session
        let name = inputName
        lock.unlock()
        guard let s, let name else { return [] }
        guard let tensor = makeImageNetTensor(image, side: Self.inputSize) else { return [] }

        inferenceSem.wait()
        defer { inferenceSem.signal() }

        do {
            let nsData = tensor.withUnsafeBufferPointer { buf -> NSMutableData in
                NSMutableData(bytes: buf.baseAddress, length: buf.count * MemoryLayout<Float>.stride)
            }
            let shape: [NSNumber] = [1, 3, NSNumber(value: Self.inputSize), NSNumber(value: Self.inputSize)]
            let value = try ORTValue(tensorData: nsData, elementType: .float, shape: shape)
            let outputs = try s.run(withInputs: [name: value],
                                    outputNames: Set(try s.outputNames()),
                                    runOptions: nil)
            guard let first = outputs.values.first else { return [] }
            let outData = try first.tensorData() as Data
            let count = outData.count / MemoryLayout<Float>.stride
            var logits = [Float](repeating: 0, count: count)
            outData.withUnsafeBytes { raw in
                guard let src = raw.baseAddress?.assumingMemoryBound(to: Float.self) else { return }
                for i in 0..<count { logits[i] = src[i] }
            }
            lock.lock()
            let tagList = tags
            lock.unlock()
            guard logits.count == tagList.count else {
                JSONLog.shared.error(ev: "ramplus_inference_failed",
                                     error: "RAM++ output dim \(logits.count) != tag list \(tagList.count) — ONNX and tag list out of sync")
                return []
            }
            return selectTags(logits)
        } catch {
            JSONLog.shared.error(ev: "ramplus_inference_failed", error: "\(error)")
            return []
        }
    }

    /// Pre-warm the model + ANE pipeline (mirrors ArcFaceService.preWarm).
    public func preWarm() {
        guard load() else { return }
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let ctx = CGContext(data: nil, width: 32, height: 32, bitsPerComponent: 8,
                                  bytesPerRow: 0, space: cs,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
              let img = ctx.makeImage() else { return }
        let started = CFAbsoluteTimeGetCurrent()
        _ = tag(img)
        JSONLog.shared.info(ev: "ramplus_prewarmed",
                            extra: ["ms": AnyCodable((CFAbsoluteTimeGetCurrent() - started) * 1000)])
    }

    // MARK: - Selection (port of ram_plus.rs select_tags)

    private func selectTags(_ logits: [Float]) -> [(tag: String, score: Float)] {
        lock.lock()
        let tagList = tags
        let perClass = perClassThreshold
        let globalT = globalThreshold
        let floor = precisionFloor
        let extra = suppressExtra
        let cap = maxTags
        lock.unlock()

        var hits: [(Int, Float)] = []
        hits.reserveCapacity(16)
        for (i, z) in logits.enumerated() {
            if Self.isSuppressed(tagList[i], extra: extra) { continue }
            let p = Self.sigmoid(z)
            let cut = max(perClass?[i] ?? globalT, floor)
            if p >= cut { hits.append((i, p)) }
        }
        // Sort by probability descending (total order, matches Rust total_cmp).
        hits.sort { $0.1 > $1.1 }
        if hits.count > cap { hits.removeLast(hits.count - cap) }
        return hits.map { (tagList[$0.0], $0.1) }
    }

    private static func isSuppressed(_ tag: String, extra: Set<String>) -> Bool {
        let lower = tag.lowercased()
        if suppressedBuiltin.contains(lower) { return true }
        return !extra.isEmpty && extra.contains(lower)
    }

    private static func sigmoid(_ z: Float) -> Float { 1.0 / (1.0 + Float(Foundation.exp(-Double(z)))) }

    // MARK: - Preprocessing

    /// Resize to side×side RGB and pack as Float32 NCHW with ImageNet
    /// normalization: v = pixel/255; out = (v - mean[c]) / std[c]. Layout
    /// [1, 3, side, side], C-major. CGContext bilinear (.high) ≈ the Windows
    /// Triangle resampling; tag output is robust to the minor resampler delta.
    private func makeImageNetTensor(_ src: CGImage, side: Int) -> [Float]? {
        let bytesPerPixel = 4
        let bytesPerRow = side * bytesPerPixel
        var rgba = [UInt8](repeating: 0, count: side * side * bytesPerPixel)
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
            let r = (Float(rgba[i * 4 + 0]) / 255.0 - mean[0]) / std[0]
            let g = (Float(rgba[i * 4 + 1]) / 255.0 - mean[1]) / std[1]
            let b = (Float(rgba[i * 4 + 2]) / 255.0 - mean[2]) / std[2]
            planes[0 * pixelCount + i] = r
            planes[1 * pixelCount + i] = g
            planes[2 * pixelCount + i] = b
        }
        return planes
    }
}
