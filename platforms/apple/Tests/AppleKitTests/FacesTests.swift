import CoreGraphics
import Foundation
import ImageIO
import Testing
import Vision
@testable import FileIDAppleKit

@Suite("FileIDAppleKit faces (Stage 6)", .serialized)
struct FacesTests {
    private func gradient(_ width: Int, _ height: Int) -> [UInt8] {
        var out = [UInt8]()
        for y in 0..<height {
            for x in 0..<width { out += [UInt8((x * 3) % 256), UInt8((y * 5) % 256), UInt8((x + y) % 256), 255] }
        }
        return out
    }

    private func writeJPEG(width: Int, height: Int, orientation: Int = 1) throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDAppleKit-faces-\(UUID().uuidString).jpg")
        let image = try #require(FacesExtraction.makeCGImage(gradient(width, height), width: width, height: height))
        let destination = try #require(CGImageDestinationCreateWithURL(url as CFURL, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(destination, image, [kCGImagePropertyOrientation: orientation] as CFDictionary)
        #expect(CGImageDestinationFinalize(destination))
        return url
    }

    private struct Opened: Decodable { let width: Int; let height: Int; let orientation: Int; let faces: [Face] }
    private struct Face: Decodable { let bounds: [UInt64]; let points: [UInt32] }

    @Test func openDecodesBoundedAndReportsNoFacesInAGradient() throws {
        let url = try writeJPEG(width: 640, height: 400)
        defer { try? FileManager.default.removeItem(at: url) }
        var json: UnsafeMutablePointer<CChar>?
        let before = FacesImages.shared.count
        let handle = url.path.withCString { fileid_apple_faces_open($0, 320, true, &json) }
        #expect(handle != 0)
        let text = try #require(json.map { String(cString: $0) })
        fileid_apple_free(json)
        let opened = try JSONDecoder().decode(Opened.self, from: Data(text.utf8))
        #expect(max(opened.width, opened.height) == 320 && opened.orientation == 1 && opened.faces.isEmpty)

        var rgba: UnsafeMutablePointer<UInt8>?
        var length = 0
        var width: UInt32 = 0, height: UInt32 = 0
        #expect(fileid_apple_faces_pixels(handle, &rgba, &length, &width, &height))
        #expect(length == Int(width) * Int(height) * 4 && Int(width) == opened.width && Int(height) == opened.height)
        fileid_apple_free(rgba)

        var tensor = [UInt8](repeating: 0, count: 112 * 112 * 4)
        var jpeg: UnsafeMutablePointer<UInt8>?
        var jpegLength = 0
        #expect(tensor.withUnsafeMutableBufferPointer { fileid_apple_faces_crop(handle, 10, 20, 60, 80, $0.baseAddress, &jpeg, &jpegLength) })
        #expect(jpegLength > 100 && tensor.contains { $0 != 0 })
        fileid_apple_free(jpeg)
        #expect(!tensor.withUnsafeMutableBufferPointer { fileid_apple_faces_crop(handle, 0, 0, 4, 4, $0.baseAddress, nil, nil) })

        fileid_apple_faces_close(handle)
        fileid_apple_faces_close(handle)
        #expect(FacesImages.shared.count == before)
    }

    @Test func openRefusesTinyAndMissingFiles() throws {
        let tiny = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDAppleKit-tiny-\(UUID().uuidString).jpg")
        try Data(repeating: 0xFF, count: 100).write(to: tiny)
        defer { try? FileManager.default.removeItem(at: tiny) }
        var json: UnsafeMutablePointer<CChar>?
        #expect(tiny.path.withCString { fileid_apple_faces_open($0, 2048, true, &json) } == 0)
        #expect(json == nil)
        #expect("/nonexistent/FileID.jpg".withCString { fileid_apple_faces_open($0, 2048, false, &json) } == 0)
        #expect(fileid_apple_faces_open(nil, 2048, false, &json) == 0)
    }

    @Test func openKeepsTheExifOrientation() throws {
        let url = try writeJPEG(width: 300, height: 200, orientation: 6)
        defer { try? FileManager.default.removeItem(at: url) }
        var json: UnsafeMutablePointer<CChar>?
        let handle = url.path.withCString { fileid_apple_faces_open($0, 2048, false, &json) }
        defer { fileid_apple_faces_close(handle) }
        let text = try #require(json.map { String(cString: $0) })
        fileid_apple_free(json)
        let opened = try JSONDecoder().decode(Opened.self, from: Data(text.utf8))
        #expect(opened.orientation == 6 && opened.width == 200 && opened.height == 300)
    }

    /// The Rust engine feeds SFace the aligned crop's bytes directly: ArcFaceService's
    /// 112 px redraw of a 112 px device-RGB image changes nothing.
    @Test func tensorRedrawOfAnAlignedCropIsTheIdentity() throws {
        let pixels = gradient(112, 112)
        let image = try #require(FacesExtraction.makeCGImage(pixels, width: 112, height: 112))
        #expect(FacesExtraction.tensorPixels(image) == pixels)
    }

    @Test func jpegEncodesRGBA() {
        var length = 0
        let pixels = gradient(112, 112)
        let jpeg = pixels.withUnsafeBufferPointer { fileid_apple_faces_jpeg($0.baseAddress, 112, 112, &length) }
        #expect(jpeg != nil && length > 100)
        if let jpeg { #expect(jpeg[0] == 0xFF && jpeg[1] == 0xD8) }
        fileid_apple_free(jpeg)
        #expect(fileid_apple_faces_jpeg(nil, 112, 112, &length) == nil)
    }

    private struct Analysis: Decodable { let complete: Bool; let faces: [FaceEntry]; let people: [PersonEntry]? }
    private struct FaceEntry: Decodable { let box: [UInt64] }
    private struct PersonEntry: Decodable { let box: [UInt64] }

    @Test func takeDecodeAndAnalysisOnAPhotoWithoutPeople() throws {
        let url = try writeJPEG(width: 3000, height: 2000)
        defer { try? FileManager.default.removeItem(at: url) }
        var sizes: [[UInt32]] = []
        for kind: UInt32 in [0, 1, 2] {
            var rgb: UnsafeMutablePointer<UInt8>?
            var length = 0
            var width: UInt32 = 0, height: UInt32 = 0, source: UInt32 = 0
            let side: UInt32 = kind == 0 ? 1024 : 2048
            #expect(url.path.withCString { fileid_apple_take_decode($0, kind, side, &rgb, &length, &width, &height, &source) })
            #expect(length == Int(width) * Int(height) * 3 && source == 3000)
            sizes.append([width, height])
            if kind == 0, let rgb {
                var kept: UInt64 = 99
                let json = fileid_apple_take_faces(rgb, width, height, true, &kept)
                let text = try #require(json.map { String(cString: $0) })
                fileid_apple_free(json)
                let analysis = try JSONDecoder().decode(Analysis.self, from: Data(text.utf8))
                #expect(analysis.complete && analysis.faces.isEmpty && analysis.people != nil)
                #expect(kept == 0, "no faces, nothing kept")
            }
            fileid_apple_free(rgb)
        }
        #expect(sizes[0] == [1024, 683])
        #expect(sizes[2] == [2048, 1365])
        #expect(!url.path.withCString { fileid_apple_take_decode($0, 9, 1024, nil, nil, nil, nil, nil) })
        #expect(fileid_apple_take_faces(nil, 10, 10, false, nil) == nil)
        #expect(fileid_apple_take_mesh(12345, 100, 100) == nil)
        fileid_apple_take_release(12345)
    }

    @Test func keptObservationsGiveMeshInputsForAnotherSize() throws {
        let face = VNFaceObservation(boundingBox: CGRect(x: 0.25, y: 0.5, width: 0.2, height: 0.3))
        let id = FacesObservations.shared.insert([face])
        let json = fileid_apple_take_mesh(id, 1000, 500)
        let text = try #require(json.map { String(cString: $0) })
        fileid_apple_free(json)
        struct Mesh: Decodable { let bits: [UInt32]; let boxOnly: Bool }
        let meshes = try JSONDecoder().decode([Mesh].self, from: Data(text.utf8))
        #expect(meshes.count == 1 && meshes[0].boxOnly)
        let values = meshes[0].bits.map(Float.init(bitPattern:))
        #expect(values[0] == 250 && values[2] == 450)
        #expect(abs(values[1] - 100) < 1e-3 && abs(values[3] - 250) < 1e-3)
        fileid_apple_take_release(id)
        #expect(FacesObservations.shared.faces(id) == nil)
    }
}
