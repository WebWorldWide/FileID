// Lazy thumbnail service for the Library grid, backed by Apple's
// QuickLookThumbnailing (handles images, videos, PDFs, docs).
//
// Cache + inflight-task bookkeeping live on MainActor; the actual QL
// request runs nonisolated because QL's completion block fires on its
// private GCD queue and resuming a checked continuation across a
// MainActor boundary trips Swift 6's executor-isolation check.
import SwiftUI
import AppKit
import QuickLookThumbnailing

@MainActor
public final class ThumbnailService {
    public static let shared = ThumbnailService()

    private let cache: NSCache<NSString, NSImage> = {
        let c = NSCache<NSString, NSImage>()
        c.countLimit = 800           // ~800 thumbnails resident
        c.totalCostLimit = 256 * 1024 * 1024  // 256 MB ceiling
        return c
    }()

    private var inflight: [String: Task<NSImage?, Never>] = [:]

    private init() {}

    public func thumbnail(for url: URL, size: CGFloat = 192) async -> NSImage? {
        let key = "\(url.path)|\(Int(size))" as NSString
        if let hit = cache.object(forKey: key) { return hit }
        if let task = inflight[key as String] { return await task.value }

        // Capture the screen scale on MainActor BEFORE entering the detached
        // task; NSScreen.main also requires MainActor isolation.
        let scale = NSScreen.main?.backingScaleFactor ?? 2
        let task = Task<NSImage?, Never> {
            await ThumbnailService.generate(url: url, size: size, scale: scale)
        }
        inflight[key as String] = task
        let image = await task.value
        inflight.removeValue(forKey: key as String)
        if let image {
            // Cost in PIXELS, not points — a Retina request yields a
            // scale× representation, and point-based costs undercount
            // 4× so the totalCostLimit never engages.
            cache.setObject(image, forKey: key, cost: Int(size * scale * size * scale * 4))
        }
        return image
    }

    /// `generateBestRepresentation` (single-callback) — using the
    /// plural `generateRepresentations(for:)` calls back per
    /// representation type and double-resumes the continuation.
    nonisolated private static func generate(url: URL, size: CGFloat, scale: CGFloat) async -> NSImage? {
        let req = QLThumbnailGenerator.Request(
            fileAt: url,
            size: CGSize(width: size, height: size),
            scale: scale,
            representationTypes: .thumbnail
        )
        return await withCheckedContinuation { (cont: CheckedContinuation<NSImage?, Never>) in
            QLThumbnailGenerator.shared.generateBestRepresentation(for: req) { rep, error in
                guard let rep, error == nil else {
                    cont.resume(returning: nil); return
                }
                cont.resume(returning: rep.nsImage)
            }
        }
    }
}
