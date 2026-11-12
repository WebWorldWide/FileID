// Lazy thumbnail service for the Library grid, backed by Apple's
// QuickLookThumbnailing (handles images, videos, PDFs, docs). Video frames
// come from the engine when it answers; Quick Look is the fallback.
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
import ImageIO
import FileIDShared

/// Async permit gate (counting semaphore) limiting concurrent QL decodes.
/// File-scope global so the nonisolated `generate` can reach it without
/// crossing the @MainActor service's isolation.
private let thumbDecodeGate = ThumbnailGate(limit: 6)
/// The engine decodes two videos at a time; a short queue beside it keeps the
/// reply timeout from counting time spent waiting behind other videos.
private let engineVideoGate = ThumbnailGate(limit: 4)

/// Asks something outside the app process (the engine) for a video frame:
/// path, modified time and long side in pixels in, JPEG bytes out, nil when
/// it can't answer.
public typealias VideoThumbnailSource = @MainActor @Sendable (String, Double?, Int) async -> Data?

@MainActor
public final class ThumbnailService {
    public static let shared = ThumbnailService()

    public var videoSource: VideoThumbnailSource?

    private let cache: NSCache<NSString, NSImage> = {
        let c = NSCache<NSString, NSImage>()
        // Cost (decoded bytes) is the real bound: a Retina grid tile is about
        // 1 MB, so 512 MB holds roughly 500 of them. The count is only a
        // ceiling for small, non-Retina tiles.
        c.countLimit = 4000
        c.totalCostLimit = 512 * 1024 * 1024
        return c
    }()

    @MainActor private final class Inflight {
        let task: Task<Data?, Never>
        var waiters = 1
        init(task: Task<Data?, Never>) { self.task = task }
    }

    private var inflight: [String: Inflight] = [:]

    private init() { ThumbnailDiskCache.trimInBackground() }

    /// Waits for the shared render. When the last view waiting on it goes away
    /// (a tile scrolled off), the render is cancelled before it starts.
    private func wait(on entry: Inflight, key: String) async -> Data? {
        await withTaskCancellationHandler {
            await entry.task.value
        } onCancel: {
            Task { @MainActor in
                entry.waiters -= 1
                guard entry.waiters <= 0 else { return }
                entry.task.cancel()
                if self.inflight[key] === entry { self.inflight[key] = nil }
            }
        }
    }

    public func thumbnail(for url: URL, size requested: CGFloat = 192, revision: String? = nil) async -> NSImage? {
        let size = Self.bucket(requested)
        let key = ("\(url.path)|\(Int(size))" + (revision.map { "|" + $0 } ?? "")) as NSString
        if let hit = cache.object(forKey: key) { return hit }
        let keyString = key as String
        // Capture the screen scale on MainActor BEFORE entering the detached
        // task; NSScreen.main also requires MainActor isolation.
        let scale = NSScreen.main?.backingScaleFactor ?? 2
        let entry: Inflight
        if let running = inflight[keyString] {
            running.waiters += 1
            entry = running
        } else {
            let videoSource = Self.isVideo(url) ? videoSource : nil
            entry = Inflight(task: Task<Data?, Never> {
                await ThumbnailService.generate(url: url, size: size, scale: scale, videoSource: videoSource)
            })
            inflight[keyString] = entry
        }
        let data = await wait(on: entry, key: keyString)
        if inflight[keyString] === entry { inflight[keyString] = nil }
        guard !Task.isCancelled else { return nil }
        if let hit = cache.object(forKey: key) { return hit }
        if let data, let image = await Self.decoded(data) {
            // Cost in PIXELS, not points — a Retina request yields a
            // scale× representation, and point-based costs undercount
            // 4× so the totalCostLimit never engages.
            cache.setObject(image, forKey: key, cost: Int(size * scale * size * scale * 4))
            return image
        }
        return nil
    }

    /// The bucket's pixel size on this display, within what the engine renders.
    nonisolated static func enginePixelSize(size: CGFloat, scale: CGFloat) -> Int {
        min(max(Int((size * scale).rounded()), 64), 2048)
    }

    nonisolated static func isVideo(_ url: URL) -> Bool {
        FileTypes.videos.contains(url.pathExtension.lowercased())
    }

    /// Requests round up to a few shared sizes so views reuse one cache entry.
    nonisolated static func bucket(_ size: CGFloat) -> CGFloat {
        [256, 512, 1024].first { size <= $0 } ?? 2048
    }

    /// Screen-sized still for the preview sheet, decoded straight from the
    /// file with ImageIO (a RAW's embedded preview, or a scaled decode). Skips
    /// Quick Look and the thumbnail gate, which grid thumbnails keep busy.
    nonisolated static func previewImage(for url: URL, maxPixelSize: Int) async -> NSImage? {
        await Task.detached(priority: .userInitiated) { () -> NSImage? in
            guard let source = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary) else { return nil }
            let options: [CFString: Any] = [
                kCGImageSourceCreateThumbnailFromImageIfAbsent: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceShouldCacheImmediately: true,
                kCGImageSourceThumbnailMaxPixelSize: maxPixelSize
            ]
            var image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
            if let small = image, max(small.width, small.height) < 800 {
                var full = options
                full[kCGImageSourceCreateThumbnailFromImageIfAbsent] = nil
                full[kCGImageSourceCreateThumbnailFromImageAlways] = true
                image = CGImageSourceCreateThumbnailAtIndex(source, 0, full as CFDictionary) ?? small
            }
            guard let image else { return nil }
            return NSImage(cgImage: image, size: NSSize(width: image.width, height: image.height))
        }.value
    }

    /// Decodes the cached JPEG into a bitmap off the main thread. NSImage(data:)
    /// defers decoding to the first draw, which put a JPEG decode on the main
    /// thread for every tile scrolled into view.
    nonisolated static func decoded(_ data: Data) async -> NSImage? {
        await Task.detached(priority: .userInitiated) { () -> NSImage? in
            guard let source = CGImageSourceCreateWithData(data as CFData, nil),
                  let image = CGImageSourceCreateImageAtIndex(source, 0, [kCGImageSourceShouldCacheImmediately: true] as CFDictionary)
            else { return nil }
            return NSImage(cgImage: image, size: NSSize(width: image.width, height: image.height))
        }.value
    }

    nonisolated static func sourceOrientation(for url: URL) async -> Int? {
        await thumbDecodeGate.acquire()
        defer { Task { await thumbDecodeGate.release() } }
        guard !Task.isCancelled else { return nil }
        return await Task.detached(priority: .utility) { () -> Int? in
            guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
                  let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
            else { return nil }
            let orientation = (properties[kCGImagePropertyOrientation] as? NSNumber)?.intValue ?? 1
            return (1...8).contains(orientation) ? orientation : nil
        }.value
    }

    nonisolated static func cropFace(in image: NSImage, bbox: String, sourceOrientation: Int = 1) -> NSImage? {
        guard let pixels = image.cgImage(forProposedRect: nil, context: nil, hints: nil),
              let bounds = FaceBBox.parseNormalized(bbox, imageWidth: pixels.width,
                  imageHeight: pixels.height, sourceOrientation: sourceOrientation)
        else { return nil }
        let pad = 0.20
        let x = max(0, bounds.x - bounds.w * pad)
        let y = max(0, bounds.y - bounds.h * pad)
        let w = min(1 - x, bounds.w * (1 + 2 * pad))
        let h = min(1 - y, bounds.h * (1 + 2 * pad))
        let rect = CGRect(x: x * Double(pixels.width), y: (1 - y - h) * Double(pixels.height),
            width: w * Double(pixels.width), height: h * Double(pixels.height)).integral
            .intersection(CGRect(x: 0, y: 0, width: pixels.width, height: pixels.height))
        guard rect.width > 4, rect.height > 4, let cropped = pixels.cropping(to: rect) else { return nil }
        // `cropping(to:)` shares the parent's bitmap, so a cached crop would keep
        // the whole photo alive while the cache counts only the crop's pixels.
        let (width, height) = (Int(rect.width), Int(rect.height))
        guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                      space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                      bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else {
            return NSImage(cgImage: cropped, size: rect.size)
        }
        context.interpolationQuality = .high
        context.draw(cropped, in: CGRect(x: 0, y: 0, width: width, height: height))
        guard let owned = context.makeImage() else { return NSImage(cgImage: cropped, size: rect.size) }
        return NSImage(cgImage: owned, size: rect.size)
    }

    /// Off-main thumbnail production: on-disk cache first (fast, local),
    /// else a concurrency-gated decode of the original whose result is
    /// written back to disk.
    nonisolated private static func generate(url: URL, size: CGFloat, scale: CGFloat,
                                             videoSource: VideoThumbnailSource?) async -> Data? {
        // The stat and the disk read wait behind the gate too: on a network
        // volume a fast scroll would otherwise stat hundreds of files at once,
        // most of them for tiles already scrolled past.
        await thumbDecodeGate.acquire()
        guard !Task.isCancelled else {
            await thumbDecodeGate.release()
            return nil
        }
        // File identity for invalidation — a stat is cheap next to a remote
        // decode, and lets an edited / replaced file miss its stale thumb.
        let attrs = try? FileManager.default.attributesOfItem(atPath: url.path)
        let modified = (attrs?[.modificationDate] as? Date)?.timeIntervalSince1970
        let mtime = modified ?? 0
        let bytes = (attrs?[.size] as? NSNumber)?.intValue ?? 0
        let diskURL = ThumbnailDiskCache.url(path: url.path, mtime: mtime,
                                             bytes: bytes, size: size, scale: scale)
        let cachedData = ThumbnailDiskCache.load(diskURL)
        await thumbDecodeGate.release()
        if let cachedData { return cachedData }

        let jpegData = await render(url: url, size: size, scale: scale, modifiedAt: modified,
                                    videoSource: videoSource, quickLook: quickLookJPEG)
        if let jpegData, !Task.isCancelled {
            if (try? ReadOnlyLocations.requireWritable(diskURL)) != nil {
                try? jpegData.write(to: diskURL, options: .atomic)
                ThumbnailDiskCache.noteWrite()
            }
        }
        return jpegData
    }

    /// The engine's frame when it answers for a video, else Quick Look.
    nonisolated static func render(
        url: URL, size: CGFloat, scale: CGFloat, modifiedAt: Double?,
        videoSource: VideoThumbnailSource?,
        quickLook: @Sendable (URL, CGFloat, CGFloat) async -> Data?
    ) async -> Data? {
        if let videoSource {
            await engineVideoGate.acquire()
            let engineData = Task.isCancelled ? nil
                : await videoSource(url.path, modifiedAt, enginePixelSize(size: size, scale: scale))
            await engineVideoGate.release()
            if let engineData { return engineData }
        }
        await thumbDecodeGate.acquire()
        let jpegData = Task.isCancelled ? nil : await quickLook(url, size, scale)
        await thumbDecodeGate.release()
        return jpegData
    }

    /// `generateBestRepresentation` (single-callback) — the plural
    /// `generateRepresentations(for:)` calls back per representation type
    /// and double-resumes the continuation.
    nonisolated static func quickLookJPEG(url: URL, size: CGFloat, scale: CGFloat) async -> Data? {
        await withCheckedContinuation { (cont: CheckedContinuation<Data?, Never>) in
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
                cont.resume(returning: jpegData(rep.cgImage))
            }
        }
    }

    nonisolated static func jpegData(_ image: CGImage) -> Data? {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, "public.jpeg" as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: 0.8] as CFDictionary)
        return CGImageDestinationFinalize(destination) ? data as Data : nil
    }
}

/// Counting-semaphore actor. A releasing task with a pending waiter hands
/// its permit straight to the waiter (active count stays constant), so the
/// in-flight permit total never exceeds `limit`. The newest waiter goes
/// first: after a fast scroll, visible tiles load before ones scrolled past.
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
            waiters.removeLast().resume()
        }
    }
}

/// On-disk JPEG thumbnail cache under Application Support/FileID/thumbnails/.
/// Stateless beyond the filesystem; every I/O error degrades gracefully.
private enum ThumbnailDiskCache {
    static let dir: URL = {
        let d = AppSupportPath.fileID.appendingPathComponent("thumbnails", isDirectory: true)
        if (try? ReadOnlyLocations.requireWritable(d)) != nil {
            try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        }
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

    static func load(_ url: URL) -> Data? {
        return try? Data(contentsOf: url)
    }

    static let byteCap: Int64 = 2 * 1024 * 1024 * 1024
    private static let writesBetweenTrims = 300
    private static let writeCount = MutexBox(0)

    static func noteWrite() {
        let due = writeCount.withLock { count -> Bool in
            count += 1
            guard count >= writesBetweenTrims else { return false }
            count = 0
            return true
        }
        if due { trimInBackground() }
    }

    static func trimInBackground() {
        DispatchQueue.global(qos: .utility).async { trim() }
    }

    /// Deletes the oldest thumbnails until the folder is back under the cap.
    private static func trim() {
        guard (try? ReadOnlyLocations.requireWritable(dir)) != nil,
              let files = try? FileManager.default.contentsOfDirectory(
                at: dir, includingPropertiesForKeys: [.fileSizeKey, .contentModificationDateKey], options: [.skipsHiddenFiles])
        else { return }
        var entries: [(url: URL, bytes: Int64, date: Date)] = []
        var total: Int64 = 0
        for file in files where file.pathExtension == "jpg" {
            guard let values = try? file.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey]) else { continue }
            let bytes = Int64(values.fileSize ?? 0)
            total += bytes
            entries.append((file, bytes, values.contentModificationDate ?? .distantPast))
        }
        guard total > byteCap else { return }
        let target = byteCap / 10 * 9
        for entry in entries.sorted(by: { $0.date < $1.date }) {
            if total <= target { break }
            if (try? FileManager.default.removeItem(at: entry.url)) != nil { total -= entry.bytes }
        }
    }
}
