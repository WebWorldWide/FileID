import Foundation
import Darwin
import MLX

/// OpenAI CLIP ViT-B/32 image tower on the GPU through MLX, loaded from the
/// same pinned ONNX file the ONNX Runtime path uses, so embeddings stay in the
/// existing CLIP space. The ONNX graph is built from dynamic-shape primitives
/// the CoreML provider cannot compile, which left CLIP on the CPU at ~190 ms
/// per image under scan load.
final class CLIPVisionMLX: @unchecked Sendable {
    static let inputSize = 224
    private static let patch = 32
    private static let width = 768
    private static let heads = 12
    private static let layers = 12
    private static let eps: Float = 1e-5

    private struct Layer {
        let ln1w, ln1b, qw, qb, kw, kb, vw, vb, ow, ob, ln2w, ln2b, fc1w, fc1b, fc2w, fc2b: MLXArray
    }

    private let patchWeight: MLXArray
    private let classEmbedding: MLXArray
    private let positionEmbedding: MLXArray
    private let preW, preB, postW, postB, projection: MLXArray
    private let blocks: [Layer]
    private let lock = NSLock()

    enum LoadError: Error { case unreadable, malformed, missing(String) }

    private let dtype: DType

    init(onnxURL: URL, dtype: DType = .float16) throws {
        self.dtype = dtype
        let weights = try ONNXInitializerReader.visionWeights(at: onnxURL)
        func w(_ name: String) throws -> MLXArray {
            guard let value = weights[name] else { throw LoadError.missing(name) }
            return value.asType(dtype)
        }
        patchWeight = try w("vision_model.embeddings.patch_embedding.weight")
            .reshaped([Self.width, 3 * Self.patch * Self.patch]).transposed()
        classEmbedding = try w("vision_model.embeddings.class_embedding").reshaped([1, 1, Self.width])
        positionEmbedding = try w("vision_model.embeddings.position_embedding.weight").expandedDimensions(axis: 0)
        preW = try w("vision_model.pre_layrnorm.weight"); preB = try w("vision_model.pre_layrnorm.bias")
        postW = try w("vision_model.post_layernorm.weight"); postB = try w("vision_model.post_layernorm.bias")
        projection = try w("visual_projection.weight")
        blocks = try (0..<Self.layers).map { i in
            let p = "vision_model.encoder.layers.\(i)."
            return Layer(
                ln1w: try w(p + "layer_norm1.weight"), ln1b: try w(p + "layer_norm1.bias"),
                qw: try w(p + "self_attn.q_proj.weight"), qb: try w(p + "self_attn.q_proj.bias"),
                kw: try w(p + "self_attn.k_proj.weight"), kb: try w(p + "self_attn.k_proj.bias"),
                vw: try w(p + "self_attn.v_proj.weight"), vb: try w(p + "self_attn.v_proj.bias"),
                ow: try w(p + "self_attn.out_proj.weight"), ob: try w(p + "self_attn.out_proj.bias"),
                ln2w: try w(p + "layer_norm2.weight"), ln2b: try w(p + "layer_norm2.bias"),
                fc1w: try w(p + "mlp.fc1.weight"), fc1b: try w(p + "mlp.fc1.bias"),
                fc2w: try w(p + "mlp.fc2.weight"), fc2b: try w(p + "mlp.fc2.bias"))
        }
        eval([patchWeight, classEmbedding, positionEmbedding, projection])
    }

    /// `pixels` holds `count` normalized CHW 224×224 images back to back.
    /// Returns `count` unnormalized 512-d embeddings.
    func embed(pixels: [Float], count: Int) -> [[Float]] {
        let side = Self.inputSize, grid = side / Self.patch, dim = Self.width, headDim = dim / Self.heads
        lock.lock(); defer { lock.unlock() }
        let input = MLXArray(pixels, [count, 3, grid, Self.patch, grid, Self.patch]).asType(dtype)
        var x = matmul(input.transposed(axes: [0, 2, 4, 1, 3, 5]).reshaped([count, grid * grid, 3 * Self.patch * Self.patch]), patchWeight)
        x = concatenated([broadcast(classEmbedding, to: [count, 1, dim]), x], axis: 1) + positionEmbedding
        x = MLXFast.layerNorm(x, weight: preW, bias: preB, eps: Self.eps)
        let tokens = grid * grid + 1
        for l in blocks {
            let h = MLXFast.layerNorm(x, weight: l.ln1w, bias: l.ln1b, eps: Self.eps)
            func heads(_ t: MLXArray) -> MLXArray { t.reshaped([count, tokens, Self.heads, headDim]).transposed(axes: [0, 2, 1, 3]) }
            let attention = MLXFast.scaledDotProductAttention(
                queries: heads(matmul(h, l.qw) + l.qb), keys: heads(matmul(h, l.kw) + l.kb), values: heads(matmul(h, l.vw) + l.vb),
                scale: 1 / Float(headDim).squareRoot(), mask: nil)
            x = x + matmul(attention.transposed(axes: [0, 2, 1, 3]).reshaped([count, tokens, dim]), l.ow) + l.ob
            var m = matmul(MLXFast.layerNorm(x, weight: l.ln2w, bias: l.ln2b, eps: Self.eps), l.fc1w) + l.fc1b
            m = m * sigmoid(1.702 * m)
            x = x + matmul(m, l.fc2w) + l.fc2b
        }
        let pooled = MLXFast.layerNorm(x[0..., 0, 0...], weight: postW, bias: postB, eps: Self.eps)
        let out = matmul(pooled, projection).asType(.float32)
        eval(out)
        let flat = out.asArray(Float.self)
        let d = out.dim(1)
        return (0..<count).map { Array(flat[($0 * d)..<(($0 + 1) * d)]) }
    }
}

/// Minimal protobuf walk over an ONNX model: float32 initializers by name,
/// with the exporter's anonymous MatMul weights renamed through the graph
/// (each one feeds a named `.bias` Add, or produces `image_embeds`).
enum ONNXInitializerReader {
    static func visionWeights(at url: URL) throws -> [String: MLXArray] {
        let fd = open(url.path, O_RDONLY)
        guard fd >= 0 else { throw CLIPVisionMLX.LoadError.unreadable }
        defer { close(fd) }
        var info = stat()
        guard fstat(fd, &info) == 0, info.st_size > 0 else { throw CLIPVisionMLX.LoadError.unreadable }
        let size = Int(info.st_size)
        guard let base = mmap(nil, size, PROT_READ, MAP_PRIVATE, fd, 0), base != MAP_FAILED else { throw CLIPVisionMLX.LoadError.unreadable }
        defer { munmap(base, size) }
        let bytes = UnsafeRawBufferPointer(start: base, count: size)
        return try parse(bytes)
    }

    struct Tensor { var dims: [Int] = []; var type = 0; var name = ""; var raw: Range<Int>? }
    struct Node { var inputs: [String] = []; var outputs: [String] = []; var op = "" }

    static func parse(_ b: UnsafeRawBufferPointer) throws -> [String: MLXArray] {
        var tensors: [Tensor] = []
        var nodes: [Node] = []
        try fields(b, 0..<b.count) { field, wire, value, span in
            guard field == 7, wire == 2, let span else { return }
            try fields(b, span) { gField, gWire, _, gSpan in
                guard gWire == 2, let gSpan else { return }
                if gField == 5 {
                    var t = Tensor()
                    try fields(b, gSpan) { f, w, v, s in
                        switch f {
                        case 1 where w == 0: t.dims.append(v)
                        case 1 where w == 2: if let s { var i = s.lowerBound; while i < s.upperBound { let (d, n) = try varint(b, i); t.dims.append(d); i = n } }
                        case 2: t.type = v
                        case 8: if let s { t.name = string(b, s) }
                        case 9: t.raw = s
                        default: break
                        }
                    }
                    tensors.append(t)
                } else if gField == 1 {
                    var n = Node()
                    try fields(b, gSpan) { f, _, _, s in
                        guard let s else { return }
                        switch f {
                        case 1: n.inputs.append(string(b, s))
                        case 2: n.outputs.append(string(b, s))
                        case 4: n.op = string(b, s)
                        default: break
                        }
                    }
                    nodes.append(n)
                }
            }
        }
        var rename: [String: String] = [:]
        var producer: [String: Node] = [:]
        for n in nodes { for o in n.outputs { producer[o] = n } }
        for n in nodes {
            if n.op == "MatMul", n.outputs.contains("image_embeds"), let w = n.inputs.first(where: { $0.hasPrefix("onnx::") }) {
                rename[w] = "visual_projection.weight"
            }
            guard n.op == "Add", let bias = n.inputs.first(where: { $0.hasPrefix("vision_model.") && $0.hasSuffix(".bias") }),
                  let other = n.inputs.first(where: { $0 != bias }), let mm = producer[other], mm.op == "MatMul",
                  let w = mm.inputs.first(where: { $0.hasPrefix("onnx::") }) else { continue }
            rename[w] = String(bias.dropLast(".bias".count)) + ".weight"
        }
        var out: [String: MLXArray] = [:]
        for t in tensors where t.type == 1 {
            guard let raw = t.raw, !t.dims.isEmpty, t.dims.reduce(1, *) * 4 == raw.count else { continue }
            let name = rename[t.name] ?? t.name
            guard name.hasPrefix("vision_model.") || name == "visual_projection.weight" else { continue }
            let floats = [Float](unsafeUninitializedCapacity: raw.count / 4) { buffer, initialized in
                memcpy(buffer.baseAddress!, b.baseAddress! + raw.lowerBound, raw.count)
                initialized = raw.count / 4
            }
            out[name] = MLXArray(floats, t.dims)
        }
        guard out.count >= 200 else { throw CLIPVisionMLX.LoadError.malformed }
        return out
    }

    private static func string(_ b: UnsafeRawBufferPointer, _ r: Range<Int>) -> String {
        String(decoding: UnsafeRawBufferPointer(rebasing: b[r]), as: UTF8.self)
    }

    private static func varint(_ b: UnsafeRawBufferPointer, _ start: Int) throws -> (Int, Int) {
        var result = 0, shift = 0, i = start
        while true {
            guard i < b.count, shift < 64 else { throw CLIPVisionMLX.LoadError.malformed }
            let c = Int(b[i]); i += 1
            result |= (c & 0x7f) << shift
            if c < 0x80 { return (result, i) }
            shift += 7
        }
    }

    private static func fields(_ b: UnsafeRawBufferPointer, _ range: Range<Int>,
                               _ visit: (Int, Int, Int, Range<Int>?) throws -> Void) throws {
        var i = range.lowerBound
        while i < range.upperBound {
            let (key, next) = try varint(b, i); i = next
            let field = key >> 3, wire = key & 7
            switch wire {
            case 0: let (v, n) = try varint(b, i); i = n; try visit(field, wire, v, nil)
            case 1: guard i + 8 <= range.upperBound else { throw CLIPVisionMLX.LoadError.malformed }; i += 8
            case 5: guard i + 4 <= range.upperBound else { throw CLIPVisionMLX.LoadError.malformed }; i += 4
            case 2:
                let (len, n) = try varint(b, i)
                guard len >= 0, n + len <= range.upperBound else { throw CLIPVisionMLX.LoadError.malformed }
                try visit(field, wire, 0, n..<(n + len)); i = n + len
            default: throw CLIPVisionMLX.LoadError.malformed
            }
        }
    }
}

/// Collects concurrent single-image requests from scan workers and runs them
/// as one GPU batch: 21 ms for one image, 3.3 ms per image in a batch of 16.
final class CLIPGPUBatcher: @unchecked Sendable {
    private final class Slot: @unchecked Sendable {
        let done = DispatchSemaphore(value: 0)
        var result: [Float]?
    }
    private let encoder: CLIPVisionMLX
    private let maxBatch: Int
    private let gather: DispatchTimeInterval
    private let lock = NSLock()
    private var pending: [(tensor: [Float], slot: Slot)] = []
    private var draining = false
    private let queue = DispatchQueue(label: "fileid.clip.gpu", qos: .userInitiated)

    init(encoder: CLIPVisionMLX, maxBatch: Int = 16, gather: DispatchTimeInterval = .milliseconds(3)) {
        self.encoder = encoder
        self.maxBatch = maxBatch
        self.gather = gather
    }

    func embed(_ tensor: [Float]) -> [Float]? {
        let slot = Slot()
        lock.lock()
        pending.append((tensor, slot))
        let start = !draining
        if start { draining = true }
        lock.unlock()
        if start { queue.asyncAfter(deadline: .now() + gather) { self.drain() } }
        slot.done.wait()
        return slot.result
    }

    private func drain() {
        Stream.withNewDefaultStream {
            while true {
                lock.lock()
                let batch = Array(pending.prefix(maxBatch))
                pending.removeFirst(batch.count)
                if batch.isEmpty { draining = false; lock.unlock(); return }
                lock.unlock()
                let outputs = autoreleasepool { encoder.embed(pixels: batch.flatMap(\.tensor), count: batch.count) }
                for (index, request) in batch.enumerated() {
                    request.slot.result = index < outputs.count ? outputs[index] : nil
                    request.slot.done.signal()
                }
            }
        }
    }
}
