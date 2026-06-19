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

        var tagged: TaggedFile
        switch discovered.kind {
        case .image:
            tagged = await processImage(discovered: discovered, worker: worker, started: started)
        case .video:
            tagged = await processVideo(discovered: discovered, worker: worker, started: started)
        case .pdf:
            tagged = await processPDF(discovered: discovered, worker: worker, started: started)
        case .doc:
            tagged = await processDoc(discovered: discovered, worker: worker, started: started)
        case .audio:
            tagged = await processAudio(discovered: discovered, started: started)
        case .model:
            tagged = await processModel(discovered: discovered, started: started)
        case .other:
            tagged = TaggedFile(
                url: url, kind: kind, extension: ext, sizeBytes: discovered.sizeBytes,
                createdAt: discovered.creationDate, modifiedAt: discovered.modificationDate,
                visionTags: [discovered.kind.rawValue.capitalized],
                perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000,
                tagsEvaluated: true
            )
        }
        // Single choke point: stamp the volume-local identity (st_ino) computed
        // at discovery onto every TaggedFile so DBWriter's rename/move heal can
        // re-bind a moved file's row instead of orphaning its tags/faces/OCR.
        tagged.fileRef = discovered.fileRef
        return tagged
    }

    // MARK: - Audio pipeline

    /// Read embedded ID3/Vorbis/MP4 metadata (artist/album/title) at SCAN and surface
    /// them as auto tags, so audio clusters by artist/album in the non-image restructure
    /// pass — lockstep with the Windows engine, which reads the same metadata (symphonia)
    /// at scan and stores it as tags. The read re-opens the file, so it's bounded against
    /// a stalled network volume (the same NAS caution as the video-keyframe path).
    private static func processAudio(
        discovered: DiscoveredFile,
        started: CFAbsoluteTime
    ) async -> TaggedFile {
        let url = discovered.url
        let ext = url.pathExtension.lowercased()
        var tags = ["Audio"]
        let meta = await boundedAudioTags(url: url, timeoutMs: 5_000)
        // Artist + album are the shared tokens the clusterer groups on (tracks from one
        // album/artist share them); title rides along to mirror the Windows tag set.
        for value in [meta.artist, meta.album, meta.title] {
            guard let v = value?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !v.isEmpty, !tags.contains(v) else { continue }
            tags.append(v)
        }
        return TaggedFile(
            url: url, kind: "audio", extension: ext, sizeBytes: discovered.sizeBytes,
            createdAt: discovered.creationDate, modifiedAt: discovered.modificationDate,
            visionTags: tags,
            perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000,
            tagsEvaluated: true
        )
    }

    /// Race `extractAudioTags` against a timeout so a metadata read on a stalled volume
    /// can't park a scan worker. Empties on timeout → the file just clusters by filename.
    private static func boundedAudioTags(
        url: URL, timeoutMs: Int
    ) async -> (title: String?, artist: String?, album: String?) {
        await withTaskGroup(of: (title: String?, artist: String?, album: String?)?.self) { group in
            group.addTask { await DeepAnalyzeNaming.extractAudioTags(url: url) }
            group.addTask {
                try? await Task.sleep(nanoseconds: UInt64(timeoutMs) * 1_000_000)
                return nil
            }
            let first = await group.next() ?? nil
            group.cancelAll()
            return first ?? (nil, nil, nil)
        }
    }

    // MARK: - 3D model pipeline

    /// Render a 3D model to a thumbnail and CLIP-embed it at SCAN — so a `.obj` clusters by
    /// its rendered shape like a photo (it joins the image/video visual pass). Restricted to
    /// `.obj` to stay lockstep with the Windows engine, whose `obj_render` only parses
    /// Wavefront `.obj`; the other recognized 3D formats are grouped under `3D Models/` by the
    /// rule cascade and named by Deep Analyze (which renders them via QuickLook on demand).
    /// `quickLookThumbnail` is itself watchdog-bounded (8 s), and runs on `visionQueue`.
    private static func processModel(
        discovered: DiscoveredFile,
        started: CFAbsoluteTime
    ) async -> TaggedFile {
        await withCheckedContinuation { (cont: CheckedContinuation<TaggedFile, Never>) in
            visionQueue.async {
                let url = discovered.url
                let ext = url.pathExtension.lowercased()
                var tagged = TaggedFile(
                    url: url, kind: "model", extension: ext, sizeBytes: discovered.sizeBytes,
                    createdAt: discovered.creationDate, modifiedAt: discovered.modificationDate,
                    visionTags: ["3D Model"],
                    tagsEvaluated: true
                )
                if ext == "obj",
                   let cg = DeepAnalyze.quickLookThumbnail(url: url, maxPixelSize: 512),
                   let blob = MobileCLIPService.shared.embedImage(cg)
                        .map({ MobileCLIPService.embeddingToBlob($0) }) {
                    tagged.clipEmbeddingBlob = blob
                }
                tagged.perFileTotalMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
                cont.resume(returning: tagged)
            }
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
                    guard let (cgImage, exif) = loadImageAndEXIF(url: url, sizeBytes: discovered.sizeBytes) else {
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
                    // Byte-exact identity for Cleanup's literal-duplicate detection
                    // (item 1) — computed from the file bytes, lockstep in structure
                    // with the Windows engine's content_hash. nil on read error, in
                    // which case the file simply won't participate in dedup.
                    let contentHash = ContentHash.compute(
                        url: url, size: UInt64(max(0, discovered.sizeBytes)))
                    let aesthetic = lightweightAesthetic(cgImage: cgImage, fileSizeMB: sizeMB)

                    // CLIP image embedding — internally bounded by inferenceSem.
                    let clipStart = CFAbsoluteTimeGetCurrent()
                    let clipBlob = MobileCLIPService.shared.embedImage(cgImage)
                        .map { MobileCLIPService.embeddingToBlob($0) }
                    let clipMs = (CFAbsoluteTimeGetCurrent() - clipStart) * 1000

                    // RAM++ primary tagger (4585-class, Apache-2.0) — the macOS
                    // lockstep mirror of the Windows stack. Falls back to the
                    // Vision classifier's narrow scene vocabulary when the RAM++
                    // ONNX isn't installed (tag() returns [] until load() succeeds),
                    // so tagging always works. RAM++ carries per-tag confidences →
                    // tags.score; the Vision fallback has none. The OCR doc-hint
                    // above keeps using pass.classifyTags (its heuristic was tuned
                    // for the Vision vocab).
                    let ramTags = RamPlusService.shared.tag(cgImage)
                    var tagScores: [String: Double]? = nil
                    var primaryTags: [String]
                    if !ramTags.isEmpty {
                        primaryTags = ramTags.map { $0.tag }
                        tagScores = Dictionary(ramTags.map { ($0.tag, Double($0.score)) },
                                               uniquingKeysWith: { first, _ in first })
                    } else {
                        primaryTags = pass.classifyTags
                    }

                    // Enrich the primary tags with EXIF + dimension signals we
                    // already have for free. Cheap, sync, gives the Library tile
                    // chips real value. EXIF/derived tags carry no score.
                    var enrichedTags = primaryTags
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
                        tagScores: tagScores,
                        phash: phash,
                        contentHash: contentHash,
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

    /// Video processing — metadata + a CLIP keyframe embedding (for restructure's content
    /// pass). We deliberately do NOT run Vision on a video frame: it deadlocks
    /// `VNControlledCapacityTasksQueue` on some inputs and Vision's perform is synchronous
    /// GCD that Task cancellation can't reach. CLIP/ORT has no such queue, and the keyframe
    /// extract is hard-bounded; visual tags + captions are still produced by Deep Analyze
    /// on demand.
    private static func processVideo(
        discovered: DiscoveredFile,
        worker: VisionWorker,
        started: CFAbsoluteTime
    ) async -> TaggedFile {
        _ = worker  // unused — kept for signature parity with image/pdf paths
        // Hop onto visionQueue (a growable GCD queue) like processImage/processPDF, so the
        // blocking keyframe wait + ORT embed run there instead of parking a narrow
        // cooperative-pool worker (which would throttle the other workers + the DB/IPC).
        return await withCheckedContinuation { (cont: CheckedContinuation<TaggedFile, Never>) in
            visionQueue.async {
                let url = discovered.url
                var tagged = TaggedFile(
                    url: url, kind: "video", extension: url.pathExtension.lowercased(),
                    sizeBytes: discovered.sizeBytes,
                    createdAt: discovered.creationDate,
                    modifiedAt: discovered.modificationDate,
                    visionTags: ["Video"],
                    tagsEvaluated: true
                )
                // Content-cluster videos like images: embed a ~25%-duration keyframe with the
                // same CLIP model, so restructure's content pass groups a vacation's videos
                // WITH its photos (it selects kind IN ('image','video')). BOUNDED — the extract
                // runs off-thread with a hard timeout, since AVFoundation duration/decode can
                // hang on a NAS file; on timeout the video clusters by filename, as before.
                // Mirrors the Windows engine, which CLIP-embeds the video keyframe at scan.
                if let cg = boundedVideoKeyframe(url: url, maxPixelSize: 512, timeout: 6),
                   let blob = MobileCLIPService.shared.embedImage(cg)
                        .map({ MobileCLIPService.embeddingToBlob($0) }) {
                    tagged.clipEmbeddingBlob = blob
                }
                tagged.perFileTotalMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
                cont.resume(returning: tagged)
            }
        }
    }

    /// Extract a video keyframe with a hard wall-clock bound. `extractVideoKeyframe` is
    /// synchronous AVFoundation that can hang on an unresponsive (NAS) file, so it runs on
    /// a utility queue and we give up after `timeout` (the worker thread continues; the
    /// orphaned extract finishes or dies with the process).
    private static func boundedVideoKeyframe(url: URL, maxPixelSize: Int, timeout: TimeInterval) -> CGImage? {
        let box = KeyframeBox()
        let sema = DispatchSemaphore(value: 0)
        DispatchQueue.global(qos: .utility).async {
            box.set(DeepAnalyze.extractVideoKeyframe(url: url, maxPixelSize: maxPixelSize))
            sema.signal()
        }
        return sema.wait(timeout: .now() + timeout) == .timedOut ? nil : box.get()
    }

    private final class KeyframeBox: @unchecked Sendable {
        private let lock = NSLock()
        private var value: CGImage?
        func set(_ v: CGImage?) { lock.lock(); value = v; lock.unlock() }
        func get() -> CGImage? { lock.lock(); defer { lock.unlock() }; return value }
    }

    // MARK: - Document pipeline

    /// Document (docx/pptx/xlsx/txt/md/…) processing — metadata + a BGE content embedding
    /// for restructure's document pass, cached at scan so the plan reads it instead of
    /// re-embedding every run (mirrors the Windows engine). Runs on visionQueue (the
    /// extraction subprocess + ORT inference are blocking).
    private static func processDoc(
        discovered: DiscoveredFile,
        worker: VisionWorker,
        started: CFAbsoluteTime
    ) async -> TaggedFile {
        _ = worker
        return await withCheckedContinuation { (cont: CheckedContinuation<TaggedFile, Never>) in
            visionQueue.async {
                let url = discovered.url
                var tagged = TaggedFile(
                    url: url, kind: "doc", extension: url.pathExtension.lowercased(),
                    sizeBytes: discovered.sizeBytes,
                    createdAt: discovered.creationDate, modifiedAt: discovered.modificationDate,
                    visionTags: ["Doc"],
                    tagsEvaluated: true
                )
                tagged.textEmbeddingBlob = bgeTextEmbeddingBlob(url: url)
                tagged.textStageDone = true   // attempted — stops the backfill carve-out re-walk
                tagged.perFileTotalMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
                cont.resume(returning: tagged)
            }
        }
    }

    /// Extract a document's text + embed it with BGE → a float32-LE blob for `text_embeddings`.
    /// nil if BGE isn't installed or no text could be extracted (the doc then clusters by
    /// filename). Bounded by DocText's readers + BGE's 256-token cap. Call ON visionQueue.
    static func bgeTextEmbeddingBlob(url: URL) -> Data? {
        guard BGETextService.shared.load(modelDir: BGETextService.defaultModelDir),
              let text = DocText.extract(path: url.path),
              let emb = BGETextService.shared.embed(text) else { return nil }
        return MobileCLIPService.embeddingToBlob(emb)
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
                var result = autoreleasepool { () -> TaggedFile in
                    guard let pdf = CGPDFDocument(url as CFURL) else {
                        // R3-13: a transient open failure must be recorded as a
                        // FAILURE (not a success with tagsEvaluated:true) — mirrors
                        // the image-decode branch. failed:true keeps the row out of
                        // the next scan's `failed = 0` skip set so the PDF is
                        // retried, and dropping tagsEvaluated stops the DBWriter
                        // from delete/reinserting auto-tags on this failed pass.
                        JSONLog.shared.warn(ev: "pdf_open_failed", path: redactPathForLog(url.path))
                        return TaggedFile(
                            url: url, kind: "pdf", extension: "pdf",
                            sizeBytes: discovered.sizeBytes,
                            createdAt: discovered.creationDate,
                            modifiedAt: discovered.modificationDate,
                            failed: true,
                            errorMessage: "Could not open PDF (will retry next scan)",
                            perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000
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
                // A PDF is a document — cache its BGE content embedding for restructure
                // (PDFKit text, not the lossy OCR), unless the file failed to open.
                if !result.failed {
                    result.textEmbeddingBlob = bgeTextEmbeddingBlob(url: url)
                    result.textStageDone = true   // attempted — stops the backfill carve-out re-walk
                }
                cont.resume(returning: result)
            }
        }
    }

    // MARK: - Helpers

    // EXIF is read from the SAME CGImageSource as the decode — a separate
    // CGImageSourceCreateWithURL re-opened and re-parsed every file, which
    // on NAS volumes cost ms per image across 14-32 workers.
    // `internal` (not `private`) so the no-redundant-stat property is unit-
    // assertable via @testable — see TaggingLoadSizeTests.
    static func loadImageAndEXIF(
        url: URL,
        sizeBytes: Int64
    ) -> (CGImage, (cameraModel: String?, lat: Double?, lon: Double?))? {
        // Skip files smaller than 256 B — corrupt or zero-byte. Avoids the
        // ImageIO crash mode v1's Session-B-hardening fixed. Reuse the size
        // Discovery (Stage A) already stat'd instead of a second
        // attributesOfItem — one fewer SMB/NFS round-trip per image on NAS.
        //
        // R-13: a 0/absent discovered size means UNKNOWN, not tiny — Discovery's
        // .fileSizeKey can come back empty on some SMB/NFS volumes (which this
        // project targets), leaving sizeBytes 0 on a perfectly valid image. Only
        // re-stat in that case so we don't mark a decodable file decode-failed;
        // the common (size > 0) path keeps the no-redundant-stat win.
        var effectiveSize = sizeBytes
        if effectiveSize <= 0,
           let n = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber {
            effectiveSize = n.int64Value
        }
        if effectiveSize > 0 && effectiveSize < 256 { return nil }
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        // Iteration 5 perf finding: load (NAS I/O + decode) was P95 252ms — by
        // far the dominant per-file cost. Two changes:
        //   - `IfAbsent` (was `Always`): use embedded JPEG thumbnails when
        //     present (every modern camera + iPhone photo embeds one). ~5-10x
        //     faster read on photos-with-thumbs; ImageIO falls back to decoding
        //     the full image only when the file lacks an embedded preview.
        //   - Resolution: CLIP (256) / RAM++ (384) / phash / OCR all downsample
        //     internally, so they're unaffected by the source size — but face
        //     DETECTION is NOT: at 512 px a face that's ~10% of a 4000 px frame is
        //     only ~50 px, right at Vision's limit, so medium / group / background
        //     faces get missed. 1536 catches them (the prior 512 was the dominant
        //     "faces aren't detected" cause). Tunable via FILEID_SCAN_MAX_PIXELS —
        //     lower = faster scan + fewer faces, higher = slower + more faces.
        let maxPixels = ProcessInfo.processInfo.environment["FILEID_SCAN_MAX_PIXELS"]
            .flatMap { Int($0) }.map { max(256, min(4096, $0)) } ?? 1536
        let opts: [CFString: Any] = [
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceCreateThumbnailFromImageIfAbsent: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixels
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
