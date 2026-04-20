import Foundation
import Vision
import CoreImage
import AppKit

/// Async service for AI-powered image manipulation.
/// Replaces the old `remove_bg.swift` CLI script with a proper callable API.
actor ImageProcessingService {
    static let shared = ImageProcessingService()
    
    /// Removes the background from an image using Apple's VNGenerateForegroundInstanceMaskRequest.
    /// Returns an NSImage with a transparent background (PNG-ready).
    func removeBackground(from url: URL) async throws -> NSImage {
        guard let nsImage = NSImage(contentsOf: url),
              let cgImage = nsImage.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
            throw ImageProcessingError.failedToLoadImage
        }
        
        return try await removeBackground(from: cgImage, originalSize: nsImage.size)
    }
    
    /// Removes background from a CGImage directly.
    func removeBackground(from cgImage: CGImage, originalSize: NSSize) async throws -> NSImage {
        let request = VNGenerateForegroundInstanceMaskRequest()
        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        
        try handler.perform([request])
        
        guard let result = request.results?.first else {
            throw ImageProcessingError.noForegroundDetected
        }
        
        let maskPixelBuffer = try result.generateScaledMaskForImage(
            forInstances: result.allInstances,
            from: handler
        )
        
        let maskImage = CIImage(cvPixelBuffer: maskPixelBuffer)
        let originalImage = CIImage(cgImage: cgImage)
        
        guard let blendFilter = CIFilter(name: "CIBlendWithMask") else {
            throw ImageProcessingError.filterCreationFailed
        }
        
        blendFilter.setValue(originalImage, forKey: kCIInputImageKey)
        blendFilter.setValue(maskImage, forKey: kCIInputMaskImageKey)
        
        guard let outputCIImage = blendFilter.outputImage else {
            throw ImageProcessingError.filterOutputFailed
        }
        
        let context = CIContext()
        guard let outputCGImage = context.createCGImage(outputCIImage, from: originalImage.extent) else {
            throw ImageProcessingError.renderFailed
        }
        
        return NSImage(cgImage: outputCGImage, size: originalSize)
    }
    
    /// Saves the processed image as a PNG to the specified URL.
    func saveAsPNG(_ image: NSImage, to url: URL) throws {
        guard let tiffData = image.tiffRepresentation,
              let bitmapRep = NSBitmapImageRep(data: tiffData),
              let pngData = bitmapRep.representation(using: .png, properties: [:]) else {
            throw ImageProcessingError.encodingFailed
        }
        try pngData.write(to: url)
    }
    
    enum ImageProcessingError: LocalizedError {
        case failedToLoadImage
        case noForegroundDetected
        case filterCreationFailed
        case filterOutputFailed
        case renderFailed
        case encodingFailed
        
        var errorDescription: String? {
            switch self {
            case .failedToLoadImage: return "Failed to load the image file."
            case .noForegroundDetected: return "No foreground subject was detected in the image."
            case .filterCreationFailed: return "Failed to create the blend filter."
            case .filterOutputFailed: return "The blend filter produced no output."
            case .renderFailed: return "Failed to render the final image."
            case .encodingFailed: return "Failed to encode the image as PNG."
            }
        }
    }
}
