import Testing
import Foundation
import CoreGraphics
import ImageIO
import OnnxRuntimeBindings
import MLX
@testable import FileIDEngine

/// Opt-in: FILEID_GPU_TESTS=1 with RAM++ installed. FILEID_RAMPLUS_IMAGES
/// names a folder of real photos; synthetic cards stand in without it.
@Suite("RAM++ backends", .serialized)
struct RamPlusBackendTests {
    private static let env = ProcessInfo.processInfo.environment
    private static var installed: Bool { env["FILEID_GPU_TESTS"] == "1" && RamPlusService.isInstalled }
    private static var sweep: Bool { installed && env["FILEID_RAMPLUS_EP_SWEEP"] == "1" }

    static func images(limit: Int = 8) -> [CGImage] {
        if let dir = env["FILEID_RAMPLUS_IMAGES"],
           let names = try? FileManager.default.contentsOfDirectory(atPath: dir) {
            let urls = names.filter { $0.lowercased().hasSuffix(".jpg") }.sorted().prefix(limit)
                .map { URL(fileURLWithPath: dir).appendingPathComponent($0) }
            let decoded = urls.compactMap { url -> CGImage? in
                guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
                return decodeBoundedImage(source, maxPixelSize: 1024)
            }
            if !decoded.isEmpty { return decoded }
        }
        return (0..<limit).map(card)
    }

    private static func card(_ seed: Int) -> CGImage {
        let ctx = CGContext(data: nil, width: 400, height: 300, bitsPerComponent: 8, bytesPerRow: 0,
                            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        for i in 0..<12 {
            let v = Double((seed * 37 + i * 53) % 255) / 255
            ctx.setFillColor(red: v, green: 1 - v, blue: Double((i * 29 + seed) % 7) / 7, alpha: 1)
            ctx.fill(CGRect(x: (i * 47 + seed * 13) % 360, y: (i * 31 + seed * 7) % 260, width: 40 + i * 3, height: 30 + seed % 20))
        }
        return ctx.makeImage()!
    }

    private static func run(_ session: ORTSession, _ tensor: [Float]) throws -> [Float] {
        let data = tensor.withUnsafeBufferPointer { NSMutableData(bytes: $0.baseAddress, length: $0.count * 4) }
        let side = RamPlusService.inputSize as NSNumber
        let value = try ORTValue(tensorData: data, elementType: .float, shape: [1, 3, side, side])
        let out = try session.run(withInputs: ["image": value], outputNames: ["logits"], runOptions: nil)
        let bytes = try out["logits"]!.tensorData() as Data
        return bytes.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
    }

    @Test(.enabled(if: installed)) func gpuMatchesOnnxRuntime() throws {
        let service = RamPlusService.shared
        #expect(service.load())
        #expect(service.loadSession())
        #expect(service.loadGPU())
        let tensors = Self.images(limit: Int(Self.env["FILEID_RAMPLUS_PARITY_COUNT"] ?? "") ?? 8).compactMap(RamPlusService.imageTensor)
        try #require(tensors.count >= 8)
        var worst: Float = 0, worstProb: Float = 0
        for (index, tensor) in tensors.enumerated() {
            let reference = try #require(service.onnxLogits(tensor))
            let gpu = try #require(service.gpuLogits(tensor))
            try #require(reference.count == gpu.count)
            let diff = zip(reference, gpu).map { abs($0 - $1) }.max() ?? 0
            let probDiff = zip(reference, gpu).map { abs(1 / (1 + exp(-$0)) - 1 / (1 + exp(-$1))) }.max() ?? 0
            worst = max(worst, diff); worstProb = max(worstProb, probDiff)
            let expected = service.selectTags(reference).map(\.tag)
            let actual = service.selectTags(gpu).map(\.tag)
            #expect(Set(expected) == Set(actual), "image \(index): onnx \(expected) gpu \(actual)")
            #expect(diff < 0.05, "image \(index) max logit diff \(diff)")
            print("ramplus-parity image \(index) max logit diff \(diff) prob diff \(probDiff) tags \(actual)")
        }
        print("ramplus-parity \(tensors.count) images, worst logit diff \(worst), worst probability diff \(worstProb)")
    }

    @Test(.enabled(if: installed)) func gpuThroughput() throws {
        let service = RamPlusService.shared
        #expect(service.load())
        #expect(service.loadGPU())
        let tensors = Self.images().compactMap(RamPlusService.imageTensor)
        try #require(!tensors.isEmpty)
        let model = try #require(service.mlxModel)
        var t = CFAbsoluteTimeGetCurrent()
        _ = model.logits(pixels: tensors[0], count: 1)
        let first = CFAbsoluteTimeGetCurrent() - t
        t = CFAbsoluteTimeGetCurrent()
        for tensor in tensors { _ = model.logits(pixels: tensor, count: 1) }
        let single = (CFAbsoluteTimeGetCurrent() - t) / Double(tensors.count)
        let batch = Array(tensors.prefix(8))
        let pixels = batch.flatMap { $0 }
        _ = model.logits(pixels: pixels, count: batch.count)
        t = CFAbsoluteTimeGetCurrent()
        for _ in 0..<3 { _ = model.logits(pixels: pixels, count: batch.count) }
        let batched = (CFAbsoluteTimeGetCurrent() - t) / Double(3 * batch.count)
        t = CFAbsoluteTimeGetCurrent()
        for _ in 0..<3 { eval(model.imageEmbeds(pixels: pixels, count: batch.count)) }
        let backbone = (CFAbsoluteTimeGetCurrent() - t) / Double(3 * batch.count)
        print(String(format: "ramplus-speed backbone %.1f ms, head %.1f ms per image (batch%d)",
                     backbone * 1000, (batched - backbone) * 1000, batch.count))
        print(String(format: "ramplus-speed mlx first %.0f ms, batch1 %.1f ms, batch%d %.1f ms per image",
                     first * 1000, single * 1000, batch.count, batched * 1000))
    }

    @Test(.enabled(if: sweep)) func executionProviderSweep() throws {
        let tensors = Self.images().compactMap(RamPlusService.imageTensor)
        try #require(!tensors.isEmpty)
        let configs: [(String, [String: String]?)] = [
            ("cpu", nil),
            ("coreml-nn-all", ["ModelFormat": "NeuralNetwork", "MLComputeUnits": "ALL"]),
            ("coreml-nn-cpu+gpu", ["ModelFormat": "NeuralNetwork", "MLComputeUnits": "CPUAndGPU"]),
            ("coreml-mlprogram-cpu+gpu", ["ModelFormat": "MLProgram", "MLComputeUnits": "CPUAndGPU"]),
            ("coreml-mlprogram-all", ["ModelFormat": "MLProgram", "MLComputeUnits": "ALL"]),
        ]
        let only = Self.env["FILEID_RAMPLUS_EP"].map { Set($0.split(separator: ",").map(String.init)) }
        let ortEnv = try ORTEnv(loggingLevel: .warning)
        for (name, options) in configs where only?.contains(name) ?? true {
            let opts = try ORTSessionOptions()
            if var options {
                options["RequireStaticInputShapes"] = "1"
                options["EnableOnSubgraphs"] = "1"
                try opts.appendCoreMLExecutionProvider(withOptionsV2: options)
            }
            var t = CFAbsoluteTimeGetCurrent()
            let session = try ORTSession(env: ortEnv, modelPath: RamPlusService.onnxURL.path, sessionOptions: opts)
            let create = CFAbsoluteTimeGetCurrent() - t
            t = CFAbsoluteTimeGetCurrent()
            _ = try Self.run(session, tensors[0])
            let first = CFAbsoluteTimeGetCurrent() - t
            t = CFAbsoluteTimeGetCurrent()
            for tensor in tensors { _ = try Self.run(session, tensor) }
            let warm = (CFAbsoluteTimeGetCurrent() - t) / Double(tensors.count)
            print(String(format: "ramplus-ep %@ create %.1f s, first %.0f ms, warm %.0f ms per image (batch 1)",
                         name, create, first * 1000, warm * 1000))
        }
    }
}
