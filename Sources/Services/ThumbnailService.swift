import AppKit
import AVFoundation
import QuickLookThumbnailing

actor ThumbnailService {
    static let shared = ThumbnailService()
    
    // Hardcap cache to strictly 500 images to prevent any out-of-memory crashes
    private let cache: NSCache<NSURL, NSImage> = {
        let c = NSCache<NSURL, NSImage>()
        c.countLimit = 500
        return c
    }()
    
    private init() {}
    
    func getThumbnail(for url: URL) async -> NSImage? {
        if let cached = cache.object(forKey: url as NSURL) {
            return cached
        }
        
        let ext = url.pathExtension.lowercased()
        let isVideo = ["mp4", "mov"].contains(ext)
        let isPDF = ext == "pdf"
        
        var image: NSImage?
        
        if isVideo {
            image = await generateVideoThumbnail(url: url)
        } else {
            // Native Apple QuickLook for instantaneous, zero-overhead OS-cached thumbnails
            let size = CGSize(width: 300, height: 300)
            let request = QLThumbnailGenerator.Request(fileAt: url, size: size, scale: 2.0, representationTypes: .thumbnail)
            
            do {
                let rep = try await QLThumbnailGenerator.shared.generateBestRepresentation(for: request)
                image = rep.nsImage
            } catch {
                // Fallback to manual CGImageSource
                let thumbOpts = [
                    kCGImageSourceCreateThumbnailFromImageAlways: true,
                    kCGImageSourceCreateThumbnailWithTransform: true,
                    kCGImageSourceThumbnailMaxPixelSize: 300
                ] as CFDictionary
                
                if let source = CGImageSourceCreateWithURL(url as CFURL, nil),
                   let cgThumb = CGImageSourceCreateThumbnailAtIndex(source, 0, thumbOpts) {
                    image = NSImage(cgImage: cgThumb, size: NSSize(width: cgThumb.width, height: cgThumb.height))
                }
            }
        }
        
        if let finalImage = image {
            cache.setObject(finalImage, forKey: url as NSURL)
        }
        return image
    }
    
    private func generateVideoThumbnail(url: URL) async -> NSImage? {
        let asset = AVAsset(url: url)
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: 300, height: 300)
        
        do {
            let duration = try await asset.load(.duration)
            let midTime = CMTimeMultiplyByFloat64(duration, multiplier: 0.5)
            let cgImage = try generator.copyCGImage(at: midTime, actualTime: nil)
            return NSImage(cgImage: cgImage, size: NSSize(width: cgImage.width, height: cgImage.height))
        } catch {
            return nil
        }
    }
}
