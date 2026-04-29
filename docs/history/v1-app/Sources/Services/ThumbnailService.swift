import AppKit
import AVFoundation
import QuickLookThumbnailing

actor ThumbnailService {
    static let shared = ThumbnailService()

    // Soft count + byte caps; scale with physical RAM via `Hardware`. NSCache
    // uses whichever limit is reached first, so both matter. The cost is the
    // decoded pixel byte count, set per-entry on `setObject(_:forKey:cost:)`.
    private let cache: NSCache<NSURL, NSImage> = {
        let c = NSCache<NSURL, NSImage>()
        c.countLimit     = Hardware.thumbnailCountLimit
        c.totalCostLimit = Hardware.thumbnailCacheMB * 1_048_576
        return c
    }()

    // Deduplicates concurrent requests for the same URL — only one QuickLook
    // call fires per URL; all other callers await the same Task.
    private var inflight: [URL: Task<NSImage?, Never>] = [:]

    private init() {}

    func getThumbnail(for url: URL) async -> NSImage? {
        if let cached = cache.object(forKey: url as NSURL) { return cached }
        if let existing = inflight[url] { return await existing.value }

        // Detached so thumbnail generation runs off the actor and doesn't
        // prevent other callers from entering and finding the inflight entry.
        let task: Task<NSImage?, Never> = Task.detached {
            await ThumbnailService.generate(for: url)
        }
        inflight[url] = task
        let result = await task.value
        inflight.removeValue(forKey: url)
        if let img = result {
            // Cost = pixel bytes (RGBA). NSCache evicts when totalCostLimit exceeded.
            let px   = Int(img.size.width * img.size.height)
            let cost = max(px * 4, 1)
            cache.setObject(img, forKey: url as NSURL, cost: cost)
        }
        return result
    }

    nonisolated private static func generate(for url: URL) async -> NSImage? {
        let ext = url.pathExtension.lowercased()
        if FileTypes.videos.contains(ext) {
            return await generateVideoThumbnail(url: url)
        }

        let size    = CGSize(width: 300, height: 300)
        let request = QLThumbnailGenerator.Request(
            fileAt: url, size: size, scale: 2.0, representationTypes: .thumbnail
        )
        if let rep = try? await QLThumbnailGenerator.shared.generateBestRepresentation(for: request) {
            return rep.nsImage
        }

        // Fallback: CGImageSource thumbnail
        let thumbOpts = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform:  true,
            kCGImageSourceThumbnailMaxPixelSize:         300
        ] as CFDictionary
        if let source  = CGImageSourceCreateWithURL(url as CFURL, nil),
           let cgThumb = CGImageSourceCreateThumbnailAtIndex(source, 0, thumbOpts) {
            return NSImage(cgImage: cgThumb, size: NSSize(width: cgThumb.width, height: cgThumb.height))
        }
        return nil
    }

    nonisolated private static func generateVideoThumbnail(url: URL) async -> NSImage? {
        let asset     = AVURLAsset(url: url)
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: 300, height: 300)
        guard let duration = try? await asset.load(.duration) else { return nil }
        let midTime = CMTimeMultiplyByFloat64(duration, multiplier: 0.5)
        guard let cgImage = try? await generator.image(at: midTime).image else { return nil }
        return NSImage(cgImage: cgImage, size: NSSize(width: cgImage.width, height: cgImage.height))
    }
}
