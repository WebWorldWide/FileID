import Cocoa
import Vision
import CoreImage

guard CommandLine.arguments.count > 2 else { exit(1) }
let inputPath = CommandLine.arguments[1]
let outputPath = CommandLine.arguments[2]

guard let img = NSImage(contentsOfFile: inputPath),
      let cgImage = img.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    print("Failed to load image")
    exit(1)
}

if #available(macOS 14.0, *) {
    let request = VNGenerateForegroundInstanceMaskRequest()
    let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
    
    do {
        try handler.perform([request])
        guard let result = request.results?.first else {
            print("No foreground found")
            exit(1)
        }
        
        let maskPixelBuffer = try result.generateScaledMaskForImage(forInstances: result.allInstances, from: handler)
        let maskImage = CIImage(cvPixelBuffer: maskPixelBuffer)
        let originalImage = CIImage(cgImage: cgImage)
        
        let filter = CIFilter(name: "CIBlendWithMask")!
        filter.setValue(originalImage, forKey: kCIInputImageKey)
        filter.setValue(maskImage, forKey: kCIInputMaskImageKey)
        
        let context = CIContext()
        guard let outputCGImage = context.createCGImage(filter.outputImage!, from: originalImage.extent) else {
            print("Failed to create CGImage")
            exit(1)
        }
        
        let outputNSImage = NSImage(cgImage: outputCGImage, size: img.size)
        let tiffData = outputNSImage.tiffRepresentation!
        let bitmapRep = NSBitmapImageRep(data: tiffData)!
        let pngData = bitmapRep.representation(using: .png, properties: [:])!
        try pngData.write(to: URL(fileURLWithPath: outputPath))
        print("Success")
    } catch {
        print("Error: \(error)")
        exit(1)
    }
} else {
    print("Requires macOS 14")
    exit(1)
}
