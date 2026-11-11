// MediaPipe Face Mesh V2 (Google, Apache-2.0), the 478-point face landmark network,
// pinned in shared/models/manifest.json as `face_mesh_landmarks`. Best Takes uses it for
// eye openness when it is installed; Vision's eye contours stay the fallback. Mirrors the
// shared engine's models/face_mesh.rs step for step, so both engines score the same eyes
// (shared/test-corpus/face-mesh-geometry.json pins the geometry).
//
// Input `input_12`: [1, 256, 256, 3] RGB in [0, 1], a square crop around one face with a
// 25% margin on each side, rolled so the eyes are level. Outputs: `Identity` 478 × (x, y, z)
// in crop pixels, `Identity_1` the face presence logit.
import CoreGraphics
import Foundation
import FileIDShared
import OnnxRuntimeBindings

enum FaceMesh {
    static let input = 256
    static let points = 478
    static let margin: Float = 1.5
    static let minPresenceLogit: Float = 0
    static let rightEye = [33, 160, 158, 133, 153, 144]
    static let leftEye = [362, 385, 387, 263, 373, 380]
    static let closedRatio = 0.14
    static let openRatio = 0.24

    struct Point: Equatable { var x: Float; var y: Float }

    struct Crop: Equatable {
        var cx: Float, cy: Float, side: Float, cos: Float, sin: Float

        func project(_ u: Float, _ v: Float) -> Point {
            let n = Float(FaceMesh.input)
            let lx = (u / n - 0.5) * side, ly = (v / n - 0.5) * side
            return Point(x: cx + lx * cos - ly * sin, y: cy + lx * sin + ly * cos)
        }
    }

    /// A face in image pixels (top-left origin): its box and two eye points plus one point
    /// below them (the nose), which only levels the crop and tells which way is up.
    struct Detection: Equatable {
        var x1: Float, y1: Float, x2: Float, y2: Float
        var eyeA: Point, eyeB: Point, nose: Point

        /// Eye and nose guesses from the box alone, as the shared engine's eval does.
        static func boxOnly(x1: Float, y1: Float, x2: Float, y2: Float) -> Detection {
            let w = x2 - x1, h = y2 - y1
            return Detection(x1: x1, y1: y1, x2: x2, y2: y2,
                             eyeA: Point(x: x1 + 0.3 * w, y: y1 + 0.4 * h),
                             eyeB: Point(x: x1 + 0.7 * w, y: y1 + 0.4 * h),
                             nose: Point(x: x1 + 0.5 * w, y: y1 + 0.6 * h))
        }
    }

    /// 8-bit pixels, any channel count of 3 or more with R, G, B first.
    struct Pixels {
        let bytes: [UInt8]
        let width: Int, height: Int, bytesPerPixel: Int, bytesPerRow: Int

        /// Draws `image` unscaled into sRGB RGBX.
        init?(_ image: CGImage) {
            let w = image.width, h = image.height
            guard w > 0, h > 0 else { return nil }
            var buffer = [UInt8](repeating: 0, count: w * h * 4)
            let drawn = buffer.withUnsafeMutableBytes { raw -> Bool in
                guard let context = CGContext(data: raw.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                                              space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                              bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue)
                else { return false }
                context.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
                return true
            }
            guard drawn else { return nil }
            self.init(bytes: buffer, width: w, height: h, bytesPerPixel: 4, bytesPerRow: w * 4)
        }

        init(bytes: [UInt8], width: Int, height: Int, bytesPerPixel: Int, bytesPerRow: Int) {
            self.bytes = bytes; self.width = width; self.height = height
            self.bytesPerPixel = bytesPerPixel; self.bytesPerRow = bytesPerRow
        }
    }

    /// The crop resampled bilinearly into the network's NHWC input, values in 0…1.
    static func sample(_ pixels: Pixels, _ crop: Crop) -> [Float] {
        var out = [Float](repeating: 0, count: input * input * 3)
        let maxX = Float(pixels.width - 1), maxY = Float(pixels.height - 1)
        let bpp = pixels.bytesPerPixel, row = pixels.bytesPerRow
        pixels.bytes.withUnsafeBufferPointer { src in
            out.withUnsafeMutableBufferPointer { dst in
                for v in 0..<input {
                    for u in 0..<input {
                        let p = crop.project(Float(u) + 0.5, Float(v) + 0.5)
                        let x = min(max(p.x - 0.5, 0), maxX), y = min(max(p.y - 0.5, 0), maxY)
                        let x0 = Int(x), y0 = Int(y)
                        let x1 = min(x0 + 1, pixels.width - 1), y1 = min(y0 + 1, pixels.height - 1)
                        let fx = x - Float(x0), fy = y - Float(y0)
                        let a = y0 * row + x0 * bpp, b = y0 * row + x1 * bpp, c = y1 * row + x0 * bpp, d = y1 * row + x1 * bpp
                        let o = (v * input + u) * 3
                        for k in 0..<3 {
                            let top = Float(src[a + k]) * (1 - fx) + Float(src[b + k]) * fx
                            let bottom = Float(src[c + k]) * (1 - fx) + Float(src[d + k]) * fx
                            dst[o + k] = (top * (1 - fy) + bottom * fy) / 255
                        }
                    }
                }
            }
        }
        return out
    }

    static func rolled(cx: Float, cy: Float, side: Float, from: Point, to: Point, below: Point) -> Crop {
        let dx = to.x - from.x, dy = to.y - from.y
        let flip = dx * (below.y - from.y) - dy * (below.x - from.x) < 0
        let angle = atan2(dy, dx) + (flip ? Float.pi : 0)
        return Crop(cx: cx, cy: cy, side: side, cos: cos(angle), sin: sin(angle))
    }

    static func crop(from face: Detection) -> Crop {
        let side = max(max(face.x2 - face.x1, face.y2 - face.y1), 1) * margin
        let (left, right) = face.eyeA.x <= face.eyeB.x ? (face.eyeA, face.eyeB) : (face.eyeB, face.eyeA)
        return rolled(cx: (face.x1 + face.x2) * 0.5, cy: (face.y1 + face.y2) * 0.5, side: side, from: left, to: right, below: face.nose)
    }

    /// The second pass: a crop refitted to the mesh's own points, as MediaPipe tracks.
    static func crop(from points: [Point]) -> Crop? {
        guard points.count >= Self.points else { return nil }
        let level = rolled(cx: 0, cy: 0, side: 0, from: points[33], to: points[263], below: points[152])
        var loX = Float.greatestFiniteMagnitude, loY = Float.greatestFiniteMagnitude
        var hiX = -Float.greatestFiniteMagnitude, hiY = -Float.greatestFiniteMagnitude
        for p in points[..<468] {
            let x = p.x * level.cos + p.y * level.sin, y = -p.x * level.sin + p.y * level.cos
            loX = min(loX, x); loY = min(loY, y); hiX = max(hiX, x); hiY = max(hiY, y)
        }
        let side = max(hiX - loX, hiY - loY) * margin
        let mx = (loX + hiX) * 0.5, my = (loY + hiY) * 0.5
        guard side.isFinite, side >= 1 else { return nil }
        return Crop(cx: mx * level.cos - my * level.sin, cy: mx * level.sin + my * level.cos, side: side, cos: level.cos, sin: level.sin)
    }

    /// Mean eye aspect ratio over both eyes: eyelid gap over eye width.
    static func eyeAspectRatio(_ points: [Point]) -> Double? {
        guard points.count >= Self.points else { return nil }
        func distance(_ a: Int, _ b: Int) -> Double { Double(hypot(points[a].x - points[b].x, points[a].y - points[b].y)) }
        let ratios = [rightEye, leftEye].compactMap { eye -> Double? in
            let width = distance(eye[0], eye[3])
            return width > 0 ? (distance(eye[1], eye[5]) + distance(eye[2], eye[4])) / (2 * width) : nil
        }
        return ratios.isEmpty ? nil : ratios.reduce(0, +) / Double(ratios.count)
    }

    static func openness(_ ratio: Double) -> Double {
        min(1, max(0, (ratio - closedRatio) / (openRatio - closedRatio)))
    }
}

final class FaceMeshService: @unchecked Sendable {
    static let shared = FaceMeshService()
    static let artifactID = "face_mesh_landmarks"

    static var modelURL: URL {
        ArcFaceService.modelsRoot.appendingPathComponent("face_mesh/face_landmarks.onnx")
    }

    private let lock = NSLock()
    private var session: ORTSession?
    private var inputName = "input_12"

    init() {}

    /// The shared mesh when its model is installed and loads; nil leaves Vision's check in charge.
    static func installed() -> FaceMeshService? {
        guard FileManager.default.fileExists(atPath: modelURL.path) else { return nil }
        return shared.load(modelURL) ? shared : nil
    }

    /// Loads the pinned file only: a different file would change the scores behind the
    /// cache id. `FILEID_FACE_EP=coreml` binds the CoreML provider; the default is CPU.
    @discardableResult
    func load(_ url: URL) -> Bool {
        lock.lock(); defer { lock.unlock() }
        if session != nil { return true }
        let started = CFAbsoluteTimeGetCurrent()
        do {
            guard let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize, size > 0, size <= 64 * 1024 * 1024,
                  let fingerprint = ExactFileDigest.computeWithIdentity(url: url, expectedSize: Int64(size)) else { return false }
            let digest = fingerprint.digest.map { String(format: "%02x", $0) }.joined()
            guard digest == ModelManifest.artifact(id: Self.artifactID)?.sha256 else {
                JSONLog.shared.warn(ev: "face_mesh_model_mismatch", path: redactPathForLog(url.path),
                                    error: "Face mesh file doesn't match the pinned SHA-256; using Vision's eye check")
                return false
            }
            let coreML = (ProcessInfo.processInfo.environment["FILEID_FACE_EP"] ?? "").lowercased() == "coreml"
            let built = try ORTSessions.make(modelPath: url.path, coreML: coreML ? .optional : .disabled,
                                             cache: ORTSessions.CoreMLCache(modelDigest: digest, mlProgram: true, subgraphs: true))
            guard ExactFileDigest.cacheKey(url: url, expectedSize: Int64(size)) == fingerprint.identity else { return false }
            let outputs = try built.session.outputNames()
            guard outputs.contains("Identity"), outputs.contains("Identity_1"),
                  let input = try built.session.inputNames().first else {
                JSONLog.shared.error(ev: "face_mesh_model_load_failed", path: redactPathForLog(url.path),
                                     error: "Face mesh outputs \(outputs.sorted()) don't match the landmark contract")
                return false
            }
            session = built.session
            inputName = input
            JSONLog.shared.info(ev: "face_mesh_model_loaded",
                                extra: ["ep": AnyCodable(built.coreML ? "coreml" : "cpu"),
                                        "ms": AnyCodable((CFAbsoluteTimeGetCurrent() - started) * 1000)])
            return true
        } catch {
            JSONLog.shared.error(ev: "face_mesh_model_load_failed", path: redactPathForLog(url.path), error: "\(error)")
            return false
        }
    }

    /// Eye openness 0…1 for one face, nil when the network finds no face in the crop.
    func eyesOpen(_ pixels: FaceMesh.Pixels, _ face: FaceMesh.Detection) throws -> Double? {
        try eyeRatio(pixels, face).map(FaceMesh.openness)
    }

    func eyeRatio(_ pixels: FaceMesh.Pixels, _ face: FaceMesh.Detection) throws -> Double? {
        guard let first = try run(pixels, FaceMesh.crop(from: face)) else { return nil }
        let points = try FaceMesh.crop(from: first).flatMap { try run(pixels, $0) } ?? first
        return FaceMesh.eyeAspectRatio(points)
    }

    private struct NotLoaded: Error {}
    private struct BadOutput: Error {}

    private func run(_ pixels: FaceMesh.Pixels, _ crop: FaceMesh.Crop) throws -> [FaceMesh.Point]? {
        lock.lock()
        let session = self.session, inputName = self.inputName
        lock.unlock()
        guard let session else { throw NotLoaded() }
        let tensor = FaceMesh.sample(pixels, crop)
        let data = tensor.withUnsafeBufferPointer { NSMutableData(bytes: $0.baseAddress, length: $0.count * MemoryLayout<Float>.stride) }
        let value = try ORTValue(tensorData: data, elementType: .float, shape: [1, NSNumber(value: FaceMesh.input), NSNumber(value: FaceMesh.input), 3])
        let outputs = try session.run(withInputs: [inputName: value], outputNames: ["Identity", "Identity_1"], runOptions: nil)
        guard let landmarks = outputs["Identity"], let presence = outputs["Identity_1"] else { throw BadOutput() }
        let logit = try Self.floats(presence).first ?? .nan
        let values = try Self.floats(landmarks)
        guard values.count == FaceMesh.points * 3 else { throw BadOutput() }
        guard !logit.isNaN, logit >= FaceMesh.minPresenceLogit, values.allSatisfy(\.isFinite) else { return nil }
        return (0..<FaceMesh.points).map { crop.project(values[$0 * 3], values[$0 * 3 + 1]) }
    }

    private static func floats(_ value: ORTValue) throws -> [Float] {
        let data = try value.tensorData() as Data
        return data.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
    }
}
