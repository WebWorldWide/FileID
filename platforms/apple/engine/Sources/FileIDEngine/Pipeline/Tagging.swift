// Tagging stage — Stage B of the pipeline.
//
// Per-file work: load CGImage, run Vision primary pass, optionally OCR,
// compute dHash, read EXIF, build a TaggedFile struct. Bounded ANE access
// via a semaphore — the v1 lesson was that flooding ANE with 14 simultaneous
// requests causes thrashing and a throughput collapse. 3-4 in-flight is
// enough to keep ANE saturated.
//
// All work happens on a concurrent GCD queue so a slow file (corrupt JPEG,
// network volume hiccup) doesn't park a Swift cooperative thread.
import Foundation
import CoreImage
import ImageIO
import AVFoundation
import UniformTypeIdentifiers
import AsyncAlgorithms
import FileIDShared

public enum Tagging {

    /// One-time GCD queue for the synchronous Vision/AVFoundation/ImageIO
    /// calls that block their caller.
    public static let visionQueue = DispatchQueue(
        label: "com.fileid.v2.vision",
        qos: .userInitiated,
        attributes: .concurrent
    )

    // Hot-loop constants — allocated once, not per file.
    private static let gregorianCalendar = Calendar(identifier: .gregorian)
    private static let docHints: Set<String> = [
        "document", "text", "screenshot", "receipt",
        "presentation", "menu", "sign"
    ]

    /// Process one file. Pure function over the inputs: worker + url + size +
    /// dates → TaggedFile. Caller wraps in `pool.with { ... }` and pushes the
    /// result into the AsyncChannel feeding DBWriter.
    ///
    /// CLIP concurrency is bounded internally by `MobileCLIPService` via a
    /// static DispatchSemaphore — no extra parameter needed at this layer.
    public static func processFile(
        discovered: DiscoveredFile,
        worker: VisionWorker
    ) async -> TaggedFile {
        let url = discovered.url
        let kind = discovered.kind.rawValue
        let ext = url.pathExtension.lowercased()
        let started = CFAbsoluteTimeGetCurrent()

        switch discovered.kind {
        case .image:
            return await processImage(discovered: discovered, worker: worker, started: started)
        case .video:
            return await processVideo(discovered: discovered, worker: worker, started: started)
        case .pdf:
            return await processPDF(discovered: discovered, worker: worker, started: started)
        case .doc, .audio, .other:
            return TaggedFile(
                url: url, kind: kind, extension: ext, sizeBytes: discovered.sizeBytes,
                createdAt: discovered.creationDate, modifiedAt: discovered.modificationDate,
                visionTags: [discovered.kind.rawValue.capitalized],
                perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000,
                tagsEvaluated: true
            )
        }
    }

    // MARK: - Image pipeline

    private static func processImage(
        discovered: DiscoveredFile,
        worker: VisionWorker,
        started: CFAbsoluteTime
    ) async -> TaggedFile {
        await withCheckedContinuation { (cont: CheckedContinuation<TaggedFile, Never>) in
            visionQueue.async {
                let result = autoreleasepool { () -> TaggedFile in
                    let url = discovered.url
                    let ext = url.pathExtension.lowercased()
                    let sizeMB = Double(discovered.sizeBytes) / 1_048_576

                    let loadStart = CFAbsoluteTimeGetCurrent()
                    guard let (cgImage, exif) = loadImageAndEXIF(url: url) else {
                        JSONLog.shared.warn(ev: "image_decode_failed", path: redactPathForLog(url.path))
                        return TaggedFile(
                            url: url, kind: "image", extension: ext,
                            sizeBytes: discovered.sizeBytes,
                            createdAt: discovered.creationDate,
                            modifiedAt: discovered.modificationDate,
                            failed: true,
                            errorMessage: "Could not decode image",
                            perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000
                        )
                    }
                    let loadMs = (CFAbsoluteTimeGetCurrent() - loadStart) * 1000

                    // Vision primary pass — bundled classify + faces + saliency.
                    let visionStart = CFAbsoluteTimeGetCurrent()
                    let pass = worker.runPrimaryPass(cgImage)
                    let visionMs = (CFAbsoluteTimeGetCurrent() - visionStart) * 1000

                    // A timed-out primary pass returns an empty result. Persisting
                    // it as failed=false-and-empty would (a) let the DBWriter wipe
                    // prior auto-tags/faces — gated below by tagsEvaluated/
                    // facesEvaluated being false — and (b) strand the file at
                    // failed=false so the incremental skip never re-tags it. Mark
                    // it failed (gates all stay false) so the next scan retries it,
                    // mirroring the Windows per-file-timeout row. (F-C3-001/036)
                    if !pass.didComplete {
                        JSONLog.shared.warn(ev: "vision_pass_timeout", path: redactPathForLog(url.path))
                        return TaggedFile(
                            url: url, kind: "image", extension: ext,
                            sizeBytes: discovered.sizeBytes,
                            createdAt: discovered.creationDate,
                            modifiedAt: discovered.modificationDate,
                            failed: true,
                            errorMessage: "Vision pass timed out (will retry next scan)",
                            perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000
                        )
                    }

                    // OCR — only if classify suggests there's text to read. The
                    // OCR stage "ran" iff we entered this branch; the DBWriter
                    // gates its ocr_text delete/reinsert on ocrStageRan so a photo
                    // we never OCR'd (or a primary-pass timeout) can't wipe valid
                    // prior OCR text. Mirrors the Windows `ocr_stage_ran` gate.
                    var ocr: String? = nil
                    var ocrMs: Double = 0
                    var ocrStageRan = false
                    if pass.classifyTags.contains(where: { docHints.contains($0.lowercased()) }) {
                        ocrStageRan = true
                        let ocrStart = CFAbsoluteTimeGetCurrent()
                        let text = worker.ocrFast(cgImage)
                        ocr = text.isEmpty ? nil : text
                        ocrMs = (CFAbsoluteTimeGetCurrent() - ocrStart) * 1000
                    }

                    let phash = computeDHash(cgImage)
                    let aesthetic = lightweightAesthetic(cgImage: cgImage, fileSizeMB: sizeMB)

                    // CLIP image embedding — internally bounded by inferenceSem.
                    let clipStart = CFAbsoluteTimeGetCurrent()
                    let clipBlob = MobileCLIPService.shared.embedImage(cgImage)
                        .map { MobileCLIPService.embeddingToBlob($0) }
                    let clipMs = (CFAbsoluteTimeGetCurrent() - clipStart) * 1000

                    // Enrich Vision-classified tags with EXIF + dimension
                    // signals we already have for free. Cheap, sync,
                    // gives the Library tile chips real value beyond
                    // the Vision classifier's narrow vocabulary.
                    var enrichedTags = pass.classifyTags
                    enrichedTags.append(contentsOf: extraTags(
                        cgImage: cgImage,
                        cameraModel: exif.cameraModel,
                        creationDate: discovered.creationDate,
                        hasFaces: pass.faceCount > 0,
                        hasOCR: ocr?.isEmpty == false,
                        hasLocation: exif.lat != nil && exif.lon != nil
                    ))

                    var tagged = TaggedFile(
                        url: url, kind: "image", extension: ext,
                        sizeBytes: discovered.sizeBytes,
                        createdAt: discovered.creationDate,
                        modifiedAt: discovered.modificationDate,
                        visionTags: enrichedTags,
                        phash: phash,
                        aestheticScore: aesthetic,
                        hasFaces: pass.faceCount > 0,
                        facePrints: pass.facePrints,
                        faceBBoxes: pass.faceBBoxes,
                        faceQualities: pass.faceQualities,
                        faceYaws: pass.faceYaws,
                        facePitches: pass.facePitches,
                        ocrText: ocr,
                        cameraModel: exif.cameraModel,
                        locationLat: exif.lat,
                        locationLon: exif.lon,
                        perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000,
                        clipEmbeddingBlob: clipBlob,
                        tagsEvaluated: true,
                        facesEvaluated: true,
                        ocrStageRan: ocrStageRan
                    )
                    tagged.loadMs = loadMs
                    tagged.visionMs = visionMs
                    tagged.clipMs = clipMs
                    tagged.ocrMs = ocrMs
                    return tagged
                }
                cont.resume(returning: result)
            }
        }
    }

    // MARK: - Video pipeline

    /// Video processing — metadata-only. Running Vision on a decoded
    /// video frame deadlocks `VNControlledCapacityTasksQueue` on some
    /// inputs and Vision's perform is synchronous GCD that Task
    /// cancellation can't reach. We record kind/size/dates only;
    /// captions are produced by Deep Analyze on demand.
    private static func processVideo(
        discovered: DiscoveredFile,
        worker: VisionWorker,
        started: CFAbsoluteTime
    ) async -> TaggedFile {
        // No AVFoundation on the scan hot path — `AVURLAsset.load`
        // can hang for seconds on NAS-resident files. Duration is
        // recovered on-demand by the UI's QL preview when needed.
        let url = discovered.url
        var tagged = TaggedFile(
            url: url, kind: "video", extension: url.pathExtension.lowercased(),
            sizeBytes: discovered.sizeBytes,
            createdAt: discovered.creationDate,
            modifiedAt: discovered.modificationDate,
            visionTags: ["Video"],
            tagsEvaluated: true
        )
        _ = worker  // unused — kept for signature parity with image/pdf paths
        tagged.perFileTotalMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
        return tagged
    }

    // MARK: - PDF pipeline

    private static let maxPDFRenderPixels: CGFloat = 50_000_000

    /// First-page OCR (fast tier), 3-page cap, skip files > 20 MB.
    /// Mirrors v1's Batch 10 heuristics — anything bigger is usually a
    /// scanned manual where filename + Large_Document tag is enough.
    private static func processPDF(
        discovered: DiscoveredFile,
        worker: VisionWorker,
        started: CFAbsoluteTime
    ) async -> TaggedFile {
        let url = discovered.url
        let sizeMB = Double(discovered.sizeBytes) / 1_048_576
        // Skip OCR on large PDFs — usually scanned manuals where OCR cost
        // far exceeds the value of the indexable text.
        if sizeMB > 20 {
            return TaggedFile(
                url: url, kind: "pdf", extension: "pdf",
                sizeBytes: discovered.sizeBytes,
                createdAt: discovered.creationDate,
                modifiedAt: discovered.modificationDate,
                visionTags: ["PDF", "Large_Document"],
                perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000,
                tagsEvaluated: true
            )
        }

        return await withCheckedContinuation { (cont: CheckedContinuation<TaggedFile, Never>) in
            visionQueue.async {
                let result = autoreleasepool { () -> TaggedFile in
                    guard let pdf = CGPDFDocument(url as CFURL) else {
                        return TaggedFile(
                            url: url, kind: "pdf", extension: "pdf",
                            sizeBytes: discovered.sizeBytes,
                            createdAt: discovered.creationDate,
                            modifiedAt: discovered.modificationDate,
                            visionTags: ["PDF"],
                            perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000,
                            tagsEvaluated: true
                        )
                    }
                    let pageCount = min(pdf.numberOfPages, 3)
                    var fullText: [String] = []
                    for pageNum in 1...pageCount {
                        guard let page = pdf.page(at: pageNum) else { continue }
                        let bounds = page.getBoxRect(.mediaBox)
                        guard bounds.width > 0, bounds.height > 0 else { continue }
                        // The 20 MB gate above is byte-size only — a tiny
                        // vector PDF with a plotter/poster-size mediaBox
                        // (CAD, GIS) would render a multi-GB bitmap at a
                        // fixed 2x (data:nil commits w*h*4 bytes), times up
                        // to workerCap concurrent files. Clamp the scale so
                        // each page stays under the same 50 MP ceiling the
                        // Windows engine enforces (MAX_DECODED_PIXELS,
                        // tagging.rs).
                        var scale: CGFloat = 2.0
                        let scaledPixels = bounds.width * bounds.height * scale * scale
                        if scaledPixels > maxPDFRenderPixels {
                            scale *= sqrt(maxPDFRenderPixels / scaledPixels)
                        }
                        let w = Int(bounds.width  * scale)
                        let h = Int(bounds.height * scale)
                        guard w > 0, h > 0 else { continue }
                        let cs = CGColorSpaceCreateDeviceRGB()
                        guard let ctx = CGContext(
                            data: nil, width: w, height: h, bitsPerComponent: 8,
                            bytesPerRow: 0, space: cs,
                            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
                        ) else { continue }
                        ctx.setFillColor(CGColor(gray: 1, alpha: 1))
                        ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
                        ctx.scaleBy(x: scale, y: scale)
                        ctx.drawPDFPage(page)
                        if let cg = ctx.makeImage() {
                            let text = worker.ocrFast(cg)
                            if !text.isEmpty { fullText.append(text) }
                        }
                    }
                    let ocr = fullText.joined(separator: "\n\n")
                    return TaggedFile(
                        url: url, kind: "pdf", extension: "pdf",
                        sizeBytes: discovered.sizeBytes,
                        createdAt: discovered.creationDate,
                        modifiedAt: discovered.modificationDate,
                        visionTags: ["PDF"],
                        ocrText: ocr.isEmpty ? nil : ocr,
                        perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000,
                        tagsEvaluated: true,
                        ocrStageRan: true
                    )
                }
                cont.resume(returning: result)
            }
        }
    }

    // MARK: - Helpers

    // EXIF is read from the SAME CGImageSource as the decode — a separate
    // CGImageSourceCreateWithURL re-opened and re-parsed every file, which
    // on NAS volumes cost ms per image across 14-32 workers.
    private static func loadImageAndEXIF(
        url: URL
    ) -> (CGImage, (cameraModel: String?, lat: Double?, lon: Double?))? {
        // Skip files smaller than 256 B — corrupt or zero-byte. Avoids the
        // ImageIO crash mode v1's Session-B-hardening fixed.
        if let attrs = try? FileManager.default.attributesOfItem(atPath: url.path),
           let size = attrs[.size] as? Int, size < 256 {
            return nil
        }
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        // Iteration 5 perf finding: load (NAS I/O + decode) was P95 252ms — by
        // far the dominant per-file cost. Two changes:
        //   - `IfAbsent` (was `Always`): use embedded JPEG thumbnails when
        //     present (every modern camera + iPhone photo embeds one). ~5-10x
        //     faster read on photos-with-thumbs; ImageIO falls back to decoding
        //     the full image only when the file lacks an embedded preview.
        //   - 512 px (was 1024 px): MobileCLIP downscales to 256 internally and
        //     Vision face/OCR work fine at 512. Half the pixels = half the
        //     decode + resample cost for files without an embedded preview.
        let opts: [CFString: Any] = [
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceCreateThumbnailFromImageIfAbsent: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 512
        ]
        guard let img = CGImageSourceCreateThumbnailAtIndex(src, 0, opts as CFDictionary) else {
            return nil
        }
        return (img, readEXIF(from: src))
    }

    /// dHash — perceptual hash for duplicate detection. 9x8 grayscale,
    /// compare adjacent pixels horizontally → 64-bit hash.
    private static func computeDHash(_ cgImage: CGImage) -> UInt64 {
        guard cgImage.width > 0, cgImage.height > 0 else { return 0 }
        let w = 9, h = 8
        var pixels = [UInt8](repeating: 0, count: w * h)
        let cs = CGColorSpaceCreateDeviceGray()
        guard let ctx = CGContext(
            data: &pixels, width: w, height: h, bitsPerComponent: 8,
            bytesPerRow: w, space: cs,
            bitmapInfo: CGImageAlphaInfo.none.rawValue
        ) else { return 0 }
        ctx.draw(cgImage, in: CGRect(x: 0, y: 0, width: CGFloat(w), height: CGFloat(h)))
        var hash: UInt64 = 0
        for row in 0..<h {
            for col in 0..<(w - 1) {
                if pixels[row * w + col] > pixels[row * w + col + 1] {
                    hash |= (UInt64(1) << UInt64(row * 8 + col))
                }
            }
        }
        return hash
    }

    /// Cheap aesthetic proxy: file-size + megapixel score.
    private static func lightweightAesthetic(cgImage: CGImage, fileSizeMB: Double) -> Double {
        let mp = Double(cgImage.width * cgImage.height) / 1_000_000
        let sizeScore = min(fileSizeMB / 5.0, 1.0)
        let resScore  = min(mp / 12.0, 1.0)
        return min(1.0, sizeScore * 0.5 + resScore * 0.5)
    }

    /// Free-from-the-data tags layered on top of Vision's classifier
    /// output: Year (so users can search "2024") and camera family
    /// ("iPhone" / "Canon"). Sync — these don't add measurable per-file
    /// cost. Aspect orientation (Wide/Tall/Square) and capability flags
    /// (Has Faces / Has Text / Has Location) used to be emitted here too,
    /// but they dominated `TopTwoTags` on EXIF-less files and read as UI
    /// concerns rather than content; the signals still live in their own
    /// DB columns/facets. Mirrors Windows `push_enriched_extras`.
    private static func extraTags(
        cgImage: CGImage,
        cameraModel: String?,
        creationDate: Date?,
        hasFaces: Bool,
        hasOCR: Bool,
        hasLocation: Bool = false
    ) -> [String] {
        var out: [String] = []
        // Year tag from creation date.
        if let d = creationDate {
            let y = gregorianCalendar.component(.year, from: d)
            if y > 1990 && y < 2100 { out.append("Year_\(y)") }
        }
        // Camera family — collapse "Apple iPhone 15 Pro Max" → "iPhone",
        // "Canon EOS R5" → "Canon", etc. Helps users filter by gear.
        if let cm = cameraModel, !cm.isEmpty {
            let lower = cm.lowercased()
            let family: String?
            if lower.contains("iphone") { family = "iPhone" }
            else if lower.contains("ipad") { family = "iPad" }
            else if lower.contains("canon") { family = "Canon" }
            else if lower.contains("nikon") { family = "Nikon" }
            else if lower.contains("sony") { family = "Sony" }
            else if lower.contains("fuji") { family = "Fuji" }
            else if lower.contains("leica") { family = "Leica" }
            else if lower.contains("gopro") { family = "GoPro" }
            else if lower.contains("samsung") { family = "Samsung" }
            else if lower.contains("pixel") { family = "Pixel" }
            else { family = nil }
            if let family { out.append(family) }
        }
        return out
    }

    /// Read EXIF camera model + GPS coords from an already-open source.
    private static func readEXIF(from src: CGImageSource) -> (cameraModel: String?, lat: Double?, lon: Double?) {
        guard let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any]
        else {
            return (nil, nil, nil)
        }
        let tiff = props[kCGImagePropertyTIFFDictionary] as? [CFString: Any]
        let cameraModel = tiff?[kCGImagePropertyTIFFModel] as? String
        let gps = props[kCGImagePropertyGPSDictionary] as? [CFString: Any]
        var lat = gps?[kCGImagePropertyGPSLatitude] as? Double
        var lon = gps?[kCGImagePropertyGPSLongitude] as? Double
        if let latRef = gps?[kCGImagePropertyGPSLatitudeRef] as? String, latRef == "S",
           let l = lat { lat = -l }
        if let lonRef = gps?[kCGImagePropertyGPSLongitudeRef] as? String, lonRef == "W",
           let l = lon { lon = -l }
        return (cameraModel, lat, lon)
    }
}
