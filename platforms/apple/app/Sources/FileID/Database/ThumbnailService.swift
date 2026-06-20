// Lazy thumbnail service for the Library grid, backed by Apple's
// QuickLookThumbnailing (handles images, videos, PDFs, docs).
//
// Cache + inflight-task bookkeeping live on MainActor; the actual QL
// request runs nonisolated because QL's completion block fires on its
// private GCD queue and resuming a checked continuation across a
// MainActor boundary trips Swift 6's executor-isolation check.
//
// Two cache tiers keep a NAS / USB library scrollable: an in-memory
// NSCache sized for a whole ~3–4k-file library, and an on-disk JPEG cache
// keyed by path+mtime+size so thumbnails survive relaunch and never
// re-decode a (slow, remote) original twice. A small actor gate bounds
// concurrent QL decodes so a fast scroll can't fan out hundreds of
// simultaneous reads of the originals.
import SwiftUI
import AppKit
import CryptoKit
import QuickLookThumbnailing

/// Async permit gate (counting semaphore) limiting concurrent QL decodes.
/// File-scope global so the nonisolated `generate` can reach it without
/// crossing the @MainActor service's isolation.
private let thumbDecodeGate = ThumbnailGate(limit: 6)

@MainActor
public final class ThumbnailService {
    public static let shared = ThumbnailService()

    private let cache: NSCache<NSString, NSImage> = {
        let c = NSCache<NSString, NSImage>()
        c.countLimit = 4000          // cover a full ~3–4k-file library
        c.totalCostLimit = 512 * 1024 * 1024  // 512 MB ceiling
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

    /// Off-main thumbnail production: on-disk cache first (fast, local),
    /// else a concurrency-gated QL decode of the original whose result is
    /// written back to disk. `generateBestRepresentation` (single-callback)
    /// — the plural `generateRepresentations(for:)` calls back per
    /// representation type and double-resumes the continuation.
    nonisolated private static func generate(url: URL, size: CGFloat, scale: CGFloat) async -> NSImage? {
        // File identity for invalidation — a stat is cheap next to a remote
        // decode, and lets an edited / replaced file miss its stale thumb.
        let attrs = try? FileManager.default.attributesOfItem(atPath: url.path)
        let mtime = (attrs?[.modificationDate] as? Date)?.timeIntervalSince1970 ?? 0
        let bytes = (attrs?[.size] as? NSNumber)?.intValue ?? 0
        let diskURL = ThumbnailDiskCache.url(path: url.path, mtime: mtime,
                                             bytes: bytes, size: size, scale: scale)
        if let cached = ThumbnailDiskCache.load(diskURL) { return cached }

        await thumbDecodeGate.acquire()
        let image: NSImage? = await withCheckedContinuation { (cont: CheckedContinuation<NSImage?, Never>) in
            let req = QLThumbnailGenerator.Request(
                fileAt: url,
                size: CGSize(width: size, height: size),
                scale: scale,
                representationTypes: .thumbnail
            )
            QLThumbnailGenerator.shared.generateBestRepresentation(for: req) { rep, error in
                guard let rep, error == nil else {
                    cont.resume(returning: nil); return
                }
                cont.resume(returning: rep.nsImage)
            }
        }
        await thumbDecodeGate.release()
        if let image { ThumbnailDiskCache.store(image, to: diskURL) }
        return image
    }
}

/// Counting-semaphore actor. A releasing task with a pending waiter hands
/// its permit straight to the waiter (active count stays constant), so the
/// in-flight permit total never exceeds `limit`.
private actor ThumbnailGate {
    private let limit: Int
    private var active = 0
    private var waiters: [CheckedContinuation<Void, Never>] = []

    init(limit: Int) { self.limit = limit }

    func acquire() async {
        if active < limit {
            active += 1
            return
        }
        await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
            waiters.append(c)
        }
        // Resumed by release(), which transferred its permit to us.
    }

    func release() {
        if waiters.isEmpty {
            active -= 1
        } else {
            waiters.removeFirst().resume()
        }
    }
}

/// On-disk JPEG thumbnail cache under Application Support/FileID/thumbnails/.
/// Stateless beyond the filesystem; every I/O error degrades gracefully.
private enum ThumbnailDiskCache {
    static let dir: URL = {
        let d = AppSupportPath.fileID.appendingPathComponent("thumbnails", isDirectory: true)
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }()

    /// Hash of path + mtime + byte size + requested px so a different file
    /// (or a different thumbnail size) never collides on the same blob.
    static func url(path: String, mtime: TimeInterval, bytes: Int,
                    size: CGFloat, scale: CGFloat) -> URL {
        let raw = "\(path)|\(mtime)|\(bytes)|\(Int(size))|\(Int(scale))"
        let hex = SHA256.hash(data: Data(raw.utf8))
            .map { String(format: "%02x", $0) }.joined()
        return dir.appendingPathComponent("\(hex).jpg")
    }

    static func load(_ url: URL) -> NSImage? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return NSImage(data: data)
    }

    static func store(_ image: NSImage, to url: URL) {
        guard let tiff = image.tiffRepresentation,
              let rep = NSBitmapImageRep(data: tiff),
              let jpeg = rep.representation(using: .jpeg,
                                            properties: [.compressionFactor: 0.8]) else { return }
        try? jpeg.write(to: url, options: .atomic)
    }
}
