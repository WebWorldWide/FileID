import Foundation
import MLX

/// RAM++ (Swin-L @384 + label-query tagging head) on the GPU through MLX,
/// built from the installed fp16 ONNX file. Mirrors the exported graph op for
/// op: fp16 activations, with LayerNorm, softmax and the head's scaled scores
/// in fp32 where the export casts. The ONNX graph is fixed at batch 1 and runs
/// mostly on the CPU under the CoreML provider (~1.3 s per image).
final class RamPlusMLX: @unchecked Sendable {
    static let inputSize = 384
    private static let patch = 4
    private static let window = 12
    private static let swinEps: Float = 1e-5
    private static let headEps: Float = 1e-12
    // The export folds these constants to fp16.
    private static let swinScale: Float = 0.1767578125
    private static let geluDivisor: Float = 1.4140625
    private static let headScoreDivisor: Float = 13.859375
    private static let headHeads = 4

    private struct Block {
        let n1w, n1b, qkvW, qkvB, projW, projB, n2w, n2b, fc1W, fc1B, fc2W, fc2B, bias: MLXArray
        let mask: MLXArray?
        let heads: Int
    }
    private struct Merge { let nw, nb, reduction: MLXArray }
    private struct Stage { let blocks: [Block]; let merge: Merge? }
    private struct HeadLayer {
        let qW, qB, kW, kB, vW, vB, oW, oB, ln1w, ln1b, iW, iB, o2W, o2B, ln2w, ln2b: MLXArray
    }

    private let patchW, patchB, patchNW, patchNB: MLXArray
    private let stages: [Stage]
    private let normW, normB, imageProjW, imageProjB: MLXArray
    private let reweightScale: MLXArray
    private let labelEmbed: MLXArray
    private let classes: Int
    private let descriptions: Int
    private let wordvecW, wordvecB: MLXArray
    private let head: [HeadLayer]
    private let fcW, fcB, tagMask: MLXArray
    private let lock = NSLock()
    private var forward: (@Sendable (MLXArray) -> MLXArray)?

    var classCount: Int { classes }

    init(onnxURL: URL) throws {
        typealias LoadError = CLIPVisionMLX.LoadError
        var named: [String: MLXArray] = [:]
        var byNode: [String: MLXArray] = [:]
        var labelShape: [Int] = []
        try ONNXInitializerReader.withMappedModel(at: onnxURL) { bytes in
            let (tensors, nodes) = try ONNXInitializerReader.graph(bytes)
            var tensorByName: [String: ONNXInitializerReader.Tensor] = [:]
            for t in tensors { tensorByName[t.name] = t }
            var alias: [String: String] = [:]
            for n in nodes where n.op == "Identity" {
                if let input = n.inputs.first, let output = n.outputs.first { alias[output] = input }
            }
            for n in nodes where n.op == "MatMul" || n.op == "Add" || n.op == "Mul" {
                guard let input = n.inputs.first(where: { $0.hasPrefix("onnx::") }) else { continue }
                let source = alias[input] ?? input
                if n.name == "/MatMul" { continue }
                guard let t = tensorByName[source], let value = ONNXInitializerReader.array(t, in: bytes) else { continue }
                if n.name == "/Mul_1" { labelShape = t.dims }
                byNode[n.name] = value
            }
            for t in tensors where t.name.hasPrefix("m.") || t.name == "tag_mask" {
                if let value = ONNXInitializerReader.array(t, in: bytes) { named[t.name] = value }
            }
        }
        func p(_ name: String) throws -> MLXArray {
            guard let v = named[name] else { throw LoadError.missing(name) }
            return v
        }
        func n(_ name: String) throws -> MLXArray {
            guard let v = byNode[name] else { throw LoadError.missing(name) }
            return v
        }
        func f32(_ name: String) throws -> MLXArray { try p(name).asType(.float32) }

        let conv = try p("m.visual_encoder.patch_embed.proj.weight")
        let embedDim = conv.dim(0)
        patchW = conv.reshaped([embedDim, -1]).transposed()
        patchB = try p("m.visual_encoder.patch_embed.proj.bias")
        patchNW = try f32("m.visual_encoder.patch_embed.norm.weight")
        patchNB = try f32("m.visual_encoder.patch_embed.norm.bias")

        var stages: [Stage] = []
        var stage = 0
        while named["m.visual_encoder.layers.\(stage).blocks.0.norm1.weight"] != nil {
            var blocks: [Block] = []
            var index = 0
            while named["m.visual_encoder.layers.\(stage).blocks.\(index).norm1.weight"] != nil {
                let w = "m.visual_encoder.layers.\(stage).blocks.\(index)."
                let g = "/visual_encoder/layers.\(stage)/blocks.\(index)/"
                let bias = try n(g + "attn/Add")
                guard bias.ndim == 4 else { throw LoadError.malformed }
                blocks.append(Block(
                    n1w: try f32(w + "norm1.weight"), n1b: try f32(w + "norm1.bias"),
                    qkvW: try n(g + "attn/qkv/MatMul"), qkvB: try p(w + "attn.qkv.bias"),
                    projW: try n(g + "attn/proj/MatMul"), projB: try p(w + "attn.proj.bias"),
                    n2w: try f32(w + "norm2.weight"), n2b: try f32(w + "norm2.bias"),
                    fc1W: try n(g + "mlp/fc1/MatMul"), fc1B: try p(w + "mlp.fc1.bias"),
                    fc2W: try n(g + "mlp/fc2/MatMul"), fc2B: try p(w + "mlp.fc2.bias"),
                    bias: bias.expandedDimensions(axis: 1),
                    mask: byNode[g + "attn/Add_1"],
                    heads: bias.dim(1)))
                index += 1
            }
            let d = "m.visual_encoder.layers.\(stage).downsample."
            let merge = named[d + "norm.weight"] == nil ? nil : Merge(
                nw: try f32(d + "norm.weight"), nb: try f32(d + "norm.bias"),
                reduction: try n("/visual_encoder/layers.\(stage)/downsample/reduction/MatMul"))
            stages.append(Stage(blocks: blocks, merge: merge))
            stage += 1
        }
        guard stages.count == 4 else { throw LoadError.malformed }
        self.stages = stages
        normW = try f32("m.visual_encoder.norm.weight")
        normB = try f32("m.visual_encoder.norm.bias")
        imageProjW = try n("/image_proj/MatMul")
        imageProjB = try p("m.image_proj.bias")
        reweightScale = exp(try f32("m.reweight_scale")).asType(.float16)

        let des = try n("/Mul_1")
        guard labelShape.count == 4 else { throw LoadError.malformed }
        classes = labelShape[1]
        descriptions = labelShape[2]
        labelEmbed = des.reshaped([classes, descriptions, labelShape[3]])
        wordvecW = try n("/wordvec_proj/MatMul")
        wordvecB = try p("m.wordvec_proj.bias")

        var layers: [HeadLayer] = []
        var l = 0
        while named["m.tagging_head.encoder.layer.\(l).output.LayerNorm.weight"] != nil {
            let w = "m.tagging_head.encoder.layer.\(l)."
            let g = "/tagging_head/encoder/layer.\(l)/"
            layers.append(HeadLayer(
                qW: try n(g + "crossattention/self/query/MatMul"), qB: try p(w + "crossattention.self.query.bias"),
                kW: try n(g + "crossattention/self/key/MatMul"), kB: try p(w + "crossattention.self.key.bias"),
                vW: try n(g + "crossattention/self/value/MatMul"), vB: try p(w + "crossattention.self.value.bias"),
                oW: try n(g + "crossattention/output/dense/MatMul"), oB: try p(w + "crossattention.output.dense.bias"),
                ln1w: try f32(w + "crossattention.output.LayerNorm.weight"), ln1b: try f32(w + "crossattention.output.LayerNorm.bias"),
                iW: try n(g + "intermediate/dense/MatMul"), iB: try p(w + "intermediate.dense.bias"),
                o2W: try n(g + "output/dense/MatMul"), o2B: try p(w + "output.dense.bias"),
                ln2w: try f32(w + "output.LayerNorm.weight"), ln2b: try f32(w + "output.LayerNorm.bias")))
            l += 1
        }
        guard !layers.isEmpty else { throw LoadError.malformed }
        head = layers
        fcW = try n("/fc/MatMul")
        fcB = try p("m.fc.bias")
        tagMask = try p("tag_mask")

        var all: [MLXArray] = [patchW, patchB, patchNW, patchNB, normW, normB, imageProjW, imageProjB, reweightScale,
                               labelEmbed, wordvecW, wordvecB, fcW, fcB, tagMask]
        for s in stages {
            for b in s.blocks {
                all += [b.n1w, b.n1b, b.qkvW, b.qkvB, b.projW, b.projB, b.n2w, b.n2b, b.fc1W, b.fc1B, b.fc2W, b.fc2B, b.bias]
                if let m = b.mask { all.append(m) }
            }
            if let m = s.merge { all += [m.nw, m.nb, m.reduction] }
        }
        for h in head { all += [h.qW, h.qB, h.kW, h.kB, h.vW, h.vB, h.oW, h.oB, h.ln1w, h.ln1b, h.iW, h.iB, h.o2W, h.o2B, h.ln2w, h.ln2b] }
        eval(all)
    }

    private static func norm(_ x: MLXArray, _ w: MLXArray, _ b: MLXArray, _ eps: Float) -> MLXArray {
        MLXFast.layerNorm(x.asType(.float32), weight: w, bias: b, eps: eps).asType(.float16)
    }

    private static func gelu(_ x: MLXArray) -> MLXArray {
        x * (erf(x / geluDivisor) + 1) * 0.5
    }

    private func block(_ x: MLXArray, _ b: Block, count: Int, side: Int) -> MLXArray {
        let w = Self.window, c = x.dim(-1), grid = side / w, nW = grid * grid, headDim = c / b.heads
        var h = Self.norm(x, b.n1w, b.n1b, Self.swinEps).reshaped([count, side, side, c])
        if b.mask != nil { h = roll(roll(h, shift: -w / 2, axis: 1), shift: -w / 2, axis: 2) }
        h = h.reshaped([count, grid, w, grid, w, c]).transposed(axes: [0, 1, 3, 2, 4, 5]).reshaped([count, nW, w * w, c])
        let qkv = (matmul(h, b.qkvW) + b.qkvB).reshaped([count, nW, w * w, 3, b.heads, headDim])
            .transposed(axes: [3, 0, 1, 4, 2, 5])
        var scores = matmul(qkv[0] * Self.swinScale, qkv[1].swappedAxes(-1, -2)) + b.bias
        if let mask = b.mask { scores = scores + mask }
        let attention = matmul(softmax(scores, axis: -1, precise: true), qkv[2])
        h = matmul(attention.transposed(axes: [0, 1, 3, 2, 4]).reshaped([count, nW, w * w, c]), b.projW) + b.projB
        h = h.reshaped([count, grid, grid, w, w, c]).transposed(axes: [0, 1, 3, 2, 4, 5]).reshaped([count, side, side, c])
        if b.mask != nil { h = roll(roll(h, shift: w / 2, axis: 1), shift: w / 2, axis: 2) }
        let y = x + h.reshaped([count, side * side, c])
        let m = Self.gelu(matmul(Self.norm(y, b.n2w, b.n2b, Self.swinEps), b.fc1W) + b.fc1B)
        return y + (matmul(m, b.fc2W) + b.fc2B)
    }

    private func merge(_ x: MLXArray, _ m: Merge, count: Int, side: Int) -> MLXArray {
        let c = x.dim(-1), half = side / 2
        let merged = x.reshaped([count, half, 2, half, 2, c]).transposed(axes: [0, 1, 3, 4, 2, 5])
            .reshaped([count, half * half, 4 * c])
        return matmul(Self.norm(merged, m.nw, m.nb, Self.swinEps), m.reduction)
    }

    /// `pixels` holds `count` ImageNet-normalized CHW 384×384 images back to
    /// back. Returns `count` logit vectors, one per tag class.
    func logits(pixels: [Float], count: Int) -> [[Float]] {
        lock.lock(); defer { lock.unlock() }
        if forward == nil {
            forward = compile { [unowned self] input in tagLogits(imageEmbeds(input)) }
        }
        let out = forward!(Self.input(pixels, count: count))
        eval(out)
        let values = out.asArray(Float.self)
        return (0..<count).map { Array(values[($0 * classes)..<(($0 + 1) * classes)]) }
    }

    private static func input(_ pixels: [Float], count: Int) -> MLXArray {
        let grid = inputSize / patch
        return MLXArray(pixels, [count, 3, grid, patch, grid, patch]).asType(.float16)
    }

    func imageEmbeds(pixels: [Float], count: Int) -> MLXArray { imageEmbeds(Self.input(pixels, count: count)) }

    private func imageEmbeds(_ input: MLXArray) -> MLXArray {
        let count = input.dim(0), grid = Self.inputSize / Self.patch
        var x = matmul(input.transposed(axes: [0, 2, 4, 1, 3, 5]).reshaped([count, grid * grid, 3 * Self.patch * Self.patch]), patchW) + patchB
        x = Self.norm(x, patchNW, patchNB, Self.swinEps)
        var resolution = grid
        for s in stages {
            for b in s.blocks { x = block(x, b, count: count, side: resolution) }
            if let m = s.merge {
                x = merge(x, m, count: count, side: resolution)
                resolution /= 2
            }
        }
        x = Self.norm(x, normW, normB, Self.swinEps)
        let pooled = x.mean(axis: 1, keepDims: true)
        return matmul(concatenated([pooled, x], axis: 1), imageProjW) + imageProjB
    }

    func tagLogits(_ embeds: MLXArray) -> MLXArray {
        let count = embeds.dim(0)
        let cls = embeds[0..., 0, 0...].asType(.float32)
        let length = sqrt((cls * cls).sum(axis: -1, keepDims: true)).asType(.float16).asType(.float32)
        let query = reweightScale * (cls / length).asType(.float16)
        let flat = labelEmbed.reshaped([classes * descriptions, -1])
        let weights = softmax(matmul(query, flat.transposed()).reshaped([count, classes, descriptions]), axis: -1, precise: true)
        let reweighted = matmul(weights.transposed(axes: [1, 0, 2]), labelEmbed).transposed(axes: [1, 0, 2])
        var h = maximum(matmul(reweighted, wordvecW) + wordvecB, MLXArray(Float16(0)))

        let heads = Self.headHeads
        for layer in head {
            let dim = h.dim(-1), headDim = dim / heads, tokens = embeds.dim(1)
            let k = (matmul(embeds, layer.kW) + layer.kB).reshaped([count, tokens, heads, headDim]).transposed(axes: [0, 2, 3, 1])
            let v = (matmul(embeds, layer.vW) + layer.vB).reshaped([count, tokens, heads, headDim]).transposed(axes: [0, 2, 1, 3])
            let q = (matmul(h, layer.qW) + layer.qB).reshaped([count, classes, heads, headDim]).transposed(axes: [0, 2, 1, 3])
            let scores = (matmul(q, k).asType(.float32) / Self.headScoreDivisor).asType(.float16)
            let context = matmul(softmax(scores, axis: -1, precise: true), v).transposed(axes: [0, 2, 1, 3]).reshaped([count, classes, dim])
            h = Self.norm(matmul(context, layer.oW) + layer.oB + h, layer.ln1w, layer.ln1b, Self.headEps)
            let inner = Self.gelu(matmul(h, layer.iW) + layer.iB)
            h = Self.norm(matmul(inner, layer.o2W) + layer.o2B + h, layer.ln2w, layer.ln2b, Self.headEps)
        }
        return ((matmul(h, fcW) + fcB).squeezed(axis: -1) + tagMask).asType(.float32)
    }
}
