import CoreGraphics
import Foundation
import ImageIO
import Testing
import Vision
@testable import FileIDEngine

@Suite("Face Mesh eye openness (shared geometry fixture)")
struct FaceMeshTests {
    struct Fixture: Decodable {
        struct Image: Decodable { let width: Int; let height: Int }
        struct Crop: Decodable { let cx, cy, side, cos, sin: Double }
        struct Project: Decodable { let u, v: Float; let expect: [Double] }
        struct Sample: Decodable { let u, v: Int; let rgb: [Double] }
        struct DetectionCase: Decodable { let bbox: [Float]; let eyeA, eyeB, nose: [Float]; let crop: Crop; let project: Project; let samples: [Sample] }
        struct MeshCase: Decodable { let points: [[Float]]; let crop: Crop; let ratio: Double; let openness: Double }
        struct OpennessCase: Decodable { let ratio: Double; let expect: Double }
        let image: Image
        let detections: [DetectionCase]
        let meshes: [MeshCase]
        let openness: [OpennessCase]
    }

    static func fixture() throws -> Fixture {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        return try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: root.appendingPathComponent("shared/test-corpus/face-mesh-geometry.json")))
    }

    private func close(_ a: Float, _ b: Double) -> Bool { abs(Double(a) - b) < 1e-3 }

    private func matches(_ c: FaceMesh.Crop, _ e: Fixture.Crop) -> Bool {
        close(c.cx, e.cx) && close(c.cy, e.cy) && close(c.side, e.side) && close(c.cos, e.cos) && close(c.sin, e.sin)
    }

    @Test func sharedGeometryMatchesTheRustEngine() throws {
        let fixture = try Self.fixture()
        let w = fixture.image.width, h = fixture.image.height
        var bytes = [UInt8](repeating: 0, count: w * h * 3)
        for y in 0..<h {
            for x in 0..<w {
                let i = (y * w + x) * 3
                bytes[i] = UInt8((7 * x + 3 * y) % 256)
                bytes[i + 1] = UInt8((x * x + 5 * y) % 256)
                bytes[i + 2] = UInt8((x * y) % 256)
            }
        }
        let pixels = FaceMesh.Pixels(bytes: bytes, width: w, height: h, bytesPerPixel: 3, bytesPerRow: w * 3)
        let point = { (p: [Float]) in FaceMesh.Point(x: p[0], y: p[1]) }
        for item in fixture.detections {
            let face = FaceMesh.Detection(x1: item.bbox[0], y1: item.bbox[1], x2: item.bbox[2], y2: item.bbox[3],
                                          eyeA: point(item.eyeA), eyeB: point(item.eyeB), nose: point(item.nose))
            let crop = FaceMesh.crop(from: face)
            #expect(matches(crop, item.crop), "\(crop) vs \(item.crop)")
            let p = crop.project(item.project.u, item.project.v)
            #expect(close(p.x, item.project.expect[0]) && close(p.y, item.project.expect[1]))
            let tensor = FaceMesh.sample(pixels, crop)
            for s in item.samples {
                for c in 0..<3 {
                    let value = tensor[(s.v * FaceMesh.input + s.u) * 3 + c]
                    #expect(close(value, s.rgb[c]), "sample (\(s.u), \(s.v)) channel \(c): \(value) vs \(s.rgb[c])")
                }
            }
        }
        for mesh in fixture.meshes {
            let points = mesh.points.map(point)
            let crop = try #require(FaceMesh.crop(from: points))
            #expect(matches(crop, mesh.crop), "\(crop) vs \(mesh.crop)")
            let ratio = try #require(FaceMesh.eyeAspectRatio(points))
            #expect(abs(ratio - mesh.ratio) < 1e-4)
            #expect(abs(FaceMesh.openness(ratio) - mesh.openness) < 1e-3)
        }
        for item in fixture.openness { #expect(abs(FaceMesh.openness(item.ratio) - item.expect) < 1e-9) }
    }

    @Test func ratioMapsToOpennessAtTheSharedThreshold() {
        #expect(FaceMesh.openness(0.30) == 1)
        #expect(FaceMesh.openness(0.05) == 0)
        #expect(abs(FaceMesh.openness(0.175) - TakeScoring.eyesClosedBelow) < 1e-9)
        #expect(FaceMesh.openness(0.17) < TakeScoring.eyesClosedBelow)
        #expect(FaceMesh.openness(0.18) > TakeScoring.eyesClosedBelow)
        #expect(FaceMesh.eyeAspectRatio([FaceMesh.Point](repeating: .init(x: 0, y: 0), count: 10)) == nil)
    }

    @Test func visionFacesBecomeTopLeftPixelDetections() {
        let face = VNFaceObservation(boundingBox: CGRect(x: 0.25, y: 0.5, width: 0.25, height: 0.25))
        let d = VisionTakeFaceAnalyzer.meshDetection(face, width: 400, height: 200)
        #expect(d == .boxOnly(x1: 100, y1: 50, x2: 200, y2: 100))
        #expect(d.eyeA.y < d.nose.y && d.eyeA.x < d.eyeB.x)
    }

    @Test func detailImageIsMadeOnceAndOnlyWhenRead() throws {
        nonisolated(unsafe) var made = 0
        let context = try #require(CGContext(data: nil, width: 40, height: 20, bitsPerComponent: 8, bytesPerRow: 0,
                                             space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let image = try #require(context.makeImage())
        let detail = TakeDetailImage { made += 1; return image }
        #expect(made == 0)
        #expect(detail.pixels?.width == 40 && detail.image?.height == 20 && detail.pixels?.height == 20)
        #expect(made == 1)
    }

    private final class Recorder: TakeFaceAnalyzing, @unchecked Sendable {
        let id = "recorder"
        var detailWidth: Int??
        func analyze(_ image: CGImage) -> (faces: [TakeScoring.Face], people: [TakeScoring.Box])? { ([], []) }
        func analyze(_ image: CGImage, detail: TakeDetailImage?) -> (faces: [TakeScoring.Face], people: [TakeScoring.Box])? {
            detailWidth = .some(detail?.image?.width)
            return ([], [])
        }
    }

    @Test func meshDetailIsTheTileSizeDecodeOfLargePhotos() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        for (width, expected) in [(3000, 2048 as Int?), (800, nil)] {
            let context = try #require(CGContext(data: nil, width: width, height: width / 2, bitsPerComponent: 8, bytesPerRow: 0,
                                                 space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
            let url = directory.appendingPathComponent("\(width).png")
            let destination = try #require(CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil))
            CGImageDestinationAddImage(destination, try #require(context.makeImage()), nil)
            #expect(CGImageDestinationFinalize(destination))
            let recorder = Recorder()
            _ = await TakeMediaAnalysis.signals(path: url.path, isVideo: false, faces: recorder)
            #expect(recorder.detailWidth == .some(expected))
        }
    }

    @Test func cacheIDNamesTheEyeModel() {
        #expect(VisionTakeFaceAnalyzer(mesh: nil).id == "vision-landmarks-humans-v1")
        #expect(VisionTakeFaceAnalyzer(mesh: FaceMeshService()).id == "vision-mesh-humans-v1")
    }

    /// Eval harness, skipped unless FILEID_FACE_MESH_JOBS names a jobs file (one JSON object
    /// per line: id, image, bbox [x1, y1, x2, y2] in that image's pixels) and
    /// FILEID_FACE_MESH_MODEL the pinned ONNX. Writes FILEID_FACE_MESH_OUT as JSON lines with
    /// the box-only mesh ratio (as the Rust eye_state_eval example), the mesh ratio and the
    /// Vision openness for the Vision face that best overlaps the box, and timings.
    /// FILEID_FACE_MESH_LONG_SIDE first fits each image as take scoring does (1024); with
    /// FILEID_FACE_MESH_DETAIL=1 faces are still found there but the mesh reads the file's own size,
    /// or, with FILEID_FACE_MESH_ORIGINALS (a folder of the source files under the jobs' names), what the
    /// engine reads: a camera JPEG's preview, else a 2048 px decode.
    @Test(.enabled(if: ProcessInfo.processInfo.environment["FILEID_FACE_MESH_JOBS"] != nil))
    func evalHarness() throws {
        let env = ProcessInfo.processInfo.environment
        let mesh = FaceMeshService()
        try #require(mesh.load(URL(fileURLWithPath: try #require(env["FILEID_FACE_MESH_MODEL"]))))
        struct Job: Decodable { let id: String; let image: String; let bbox: [Float] }
        var out = ""
        var cache: (path: String, image: CGImage, pixels: FaceMesh.Pixels, faces: [VNFaceObservation])?
        let detail = env["FILEID_FACE_MESH_DETAIL"] == "1"
        var scale: Float = 1
        var decodedWidth = 1
        for line in try String(contentsOfFile: try #require(env["FILEID_FACE_MESH_JOBS"]), encoding: .utf8).split(separator: "\n") {
            let job = try JSONDecoder().decode(Job.self, from: Data(line.utf8))
            if cache?.path != job.image {
                let source = try #require(CGImageSourceCreateWithURL(URL(fileURLWithPath: job.image) as CFURL, nil))
                let decoded = try #require(CGImageSourceCreateImageAtIndex(source, 0, nil))
                var image = decoded
                if let side = env["FILEID_FACE_MESH_LONG_SIDE"].flatMap(Int.init) {
                    image = try #require(TakeMediaAnalysis.fit(decoded, longSide: side)).image
                }
                scale = Float(image.width) / Float(decoded.width)
                decodedWidth = decoded.width
                let request = VNDetectFaceLandmarksRequest()
                try VNImageRequestHandler(cgImage: image, options: [:]).perform([request])
                var meshImage = decoded
                if let original = env["FILEID_FACE_MESH_ORIGINALS"], let name = job.image.split(separator: "/").last {
                    let url = URL(fileURLWithPath: original).appendingPathComponent(String(name))
                    let full = try #require(CGImageSourceCreateWithURL(url as CFURL, nil))
                    meshImage = try #require(decodeScanImage(full, path: url.path, maxPixelSize: TakeScoring.tileLongSide))
                }
                cache = (job.image, image, try #require(FaceMesh.Pixels(detail ? meshImage : image)), request.results ?? [])
            }
            guard let (_, image, pixels, faces) = cache else { continue }
            let (W, H) = (Float(image.width), Float(image.height))
            let b = job.bbox.map { $0 * scale }
            let m = detail ? job.bbox.map { $0 * Float(pixels.width) / Float(decodedWidth) } : b
            var started = CFAbsoluteTimeGetCurrent()
            let boxRatio = try mesh.eyeRatio(pixels, .boxOnly(x1: m[0], y1: m[1], x2: m[2], y2: m[3]))
            let boxMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
            func iou(_ d: FaceMesh.Detection) -> Float {
                let ix = max(0, min(d.x2, b[2]) - max(d.x1, b[0])), iy = max(0, min(d.y2, b[3]) - max(d.y1, b[1]))
                let inter = ix * iy
                return inter / ((d.x2 - d.x1) * (d.y2 - d.y1) + (b[2] - b[0]) * (b[3] - b[1]) - inter)
            }
            let best = faces.map { ($0, VisionTakeFaceAnalyzer.meshDetection($0, width: W, height: H)) }.max { iou($0.1) < iou($1.1) }
            var record: [String: Any] = ["id": job.id, "boxRatio": boxRatio ?? NSNull(), "boxMs": boxMs]
            if let best, iou(best.1) >= 0.3 {
                started = CFAbsoluteTimeGetCurrent()
                let visionCropRatio = try mesh.eyeRatio(pixels, VisionTakeFaceAnalyzer.meshDetection(best.0, width: Float(pixels.width), height: Float(pixels.height)))
                record["meshRatio"] = visionCropRatio ?? NSNull()
                record["meshMs"] = (CFAbsoluteTimeGetCurrent() - started) * 1000
                record["visionOpen"] = VisionTakeFaceAnalyzer.eyesOpen(best.0) ?? NSNull()
                record["iou"] = iou(best.1)
            }
            out += String(decoding: try JSONSerialization.data(withJSONObject: record, options: [.sortedKeys]), as: UTF8.self) + "\n"
        }
        try out.write(toFile: try #require(env["FILEID_FACE_MESH_OUT"]), atomically: true, encoding: .utf8)
    }
}
