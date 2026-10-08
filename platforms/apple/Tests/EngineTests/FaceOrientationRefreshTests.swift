import CoreGraphics
import Foundation
import ImageIO
import Testing
@testable import FileIDEngine

@Suite("Imported face orientation")
struct FaceOrientationRefreshTests {
    private let bitmapInfo = CGBitmapInfo.byteOrder32Big.rawValue | CGImageAlphaInfo.premultipliedLast.rawValue

    private func center(_ image: CGImage) throws -> (red: Int, green: Int, blue: Int) {
        var bytes = [UInt8](repeating: 0, count: 4)
        return try bytes.withUnsafeMutableBytes { buffer in
            let context = try #require(CGContext(data: buffer.baseAddress, width: 1, height: 1,
                bitsPerComponent: 8, bytesPerRow: 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: bitmapInfo))
            context.interpolationQuality = .none
            context.draw(image, in: CGRect(x: 0, y: 0, width: 1, height: 1))
            return (Int(buffer[0]), Int(buffer[1]), Int(buffer[2]))
        }
    }

    @Test func importedRawBoxesFollowTheDecodedJpegRegion() throws {
        let width = 84
        let height = 64
        var bytes = [UInt8](repeating: 0, count: width * height * 4)
        for y in 0..<height {
            for x in 0..<width {
                let offset = (y * width + x) * 4
                let green = (8..<24).contains(x) && (8..<24).contains(y)
                bytes[offset + 1] = green ? 255 : 0
                bytes[offset + 2] = green ? 0 : 255
                bytes[offset + 3] = 255
            }
        }
        let provider = try #require(CGDataProvider(data: Data(bytes) as CFData))
        let raw = try #require(CGImage(width: width, height: height, bitsPerComponent: 8,
            bitsPerPixel: 32, bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: bitmapInfo), provider: provider,
            decode: nil, shouldInterpolate: false, intent: .defaultIntent))
        let bbox = #"{"x":8,"y":8,"w":16,"h":16,"coordinateSpace":"pixel-top-left","sourceWidth":84,"sourceHeight":64}"#
        for orientation in 1...8 {
            let data = NSMutableData()
            let destination = try #require(CGImageDestinationCreateWithData(data, "public.jpeg" as CFString, 1, nil))
            CGImageDestinationAddImage(destination, raw,
                [kCGImagePropertyOrientation: orientation, kCGImageDestinationLossyCompressionQuality: 1.0] as CFDictionary)
            #expect(CGImageDestinationFinalize(destination))
            let source = try #require(CGImageSourceCreateWithData(data, nil))
            let properties = try #require(CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any])
            let decodedOrientation = try #require(properties[kCGImagePropertyOrientation] as? NSNumber).intValue
            #expect(decodedOrientation == orientation)
            let decoded = try #require(decodeBoundedImage(source, maxPixelSize: 2048))
            let crop = try #require(FaceClustering.cropFaceCGImage(cgImage: decoded, bboxString: bbox,
                sourceOrientation: decodedOrientation))
            let sample = try center(crop)
            #expect(sample.green > 200 && sample.blue < 40)
            if orientation == 6 {
                let oldCrop = try #require(FaceClustering.cropFaceCGImage(cgImage: decoded, bboxString: bbox))
                let oldSample = try center(oldCrop)
                #expect(oldSample.blue > 200 && oldSample.green < 40)
            }
        }
    }
}
