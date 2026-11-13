// Tagging stage — Stage B of the pipeline.
//
// Per-file work: load CGImage, run Vision primary pass, optionally OCR,
// compute dHash, read EXIF, build a TaggedFile struct. Each model service
// bounds its own concurrency; the worker pool bounds the Vision passes.
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

    static let hashTimeoutSeconds: TimeInterval = 30

    // Hot-loop constants — allocated once, not per file.
    private static let gregorianCalendar = Calendar(identifier: .gregorian)
    static let docHints: Set<String> = [
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
        await processFile(discovered: discovered, worker: worker, twins: TwinReuse.enabled ? TwinReuse.shared : nil,
                          catalog: TwinReuse.catalog)
    }

    /// `twins` nil analyzes every image itself; tests pass their own registry
    /// and their own `options` (tagger, RAM++ mode, careful reader).
    static func processFile(
        discovered: DiscoveredFile,
        worker: VisionWorker,
        twins: TwinRegistry?,
        catalog: CatalogTwins? = nil,
        options: ScanCatchUp.ScanOptions = .live
    ) async -> TaggedFile {
        let url = discovered.url
        let kind = discovered.kind.rawValue
        let ext = url.pathExtension.lowercased()
        let started = CFAbsoluteTimeGetCurrent()

        if discovered.movedFromKnownRow {
            var moved = TaggedFile(
                url: url, kind: kind, extension: ext, sizeBytes: discovered.sizeBytes,
                createdAt: discovered.creationDate, modifiedAt: discovered.modificationDate,
                fileRef: discovered.fileRef)
            moved.healOnly = true
            return moved
        }

        var tagged: TaggedFile
        switch discovered.kind {
        case .image:
            tagged = await processImage(
                discovered: discovered, worker: worker, started: started, twins: twins, catalog: catalog, options: options)
        case .video:
            tagged = await processVideo(discovered: discovered, worker: worker, started: started)
        case .pdf:
            tagged = await processPDF(discovered: discovered, worker: worker, started: started, options: options)
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
            if ext.lowercased() == "zip" {
                // The names inside a zip go into doc_text, so searching for a file finds its archive.
                tagged.docText = await withCheckedContinuation { continuation in
                    visionQueue.async { continuation.resume(returning: ArchiveNames.list(url: url)) }
                }
                tagged.textStageDone = true
            }
        }
        await finalize(&tagged, discovered: discovered)
        return tagged
    }

    private static func finalize(_ tagged: inout TaggedFile, discovered: DiscoveredFile) async {
        let url = discovered.url
        if tagged.contentHash == nil && !Task.isCancelled {
            let contentSize = UInt64(max(0, discovered.sizeBytes))
            let state = OnceContinuation<Data?>()
            let giveUp = DispatchWorkItem { state.finish(nil) }
            tagged.contentHash = await withTaskCancellationHandler {
                await withCheckedContinuation { continuation in
                    state.install(continuation, ifFinished: nil)
                    visionQueue.async {
                        state.finish(ContentHash.compute(url: url, size: contentSize))
                    }
                    visionQueue.asyncAfter(deadline: .now() + hashTimeoutSeconds, execute: giveUp)
                }
            } onCancel: {
                state.finish(nil)
            }
            giveUp.cancel()
        }
        tagged.fileRef = discovered.fileRef
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
                        .map({ FloatBlob.encode($0) }) {
                    tagged.clipEmbeddingBlob = blob
                }
                // The content-derivation stage (render attempt) ran — stops the model
                // CLIP-backfill carve-out re-walking an un-renderable .obj forever.
                tagged.textStageDone = true
                tagged.perFileTotalMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
                cont.resume(returning: tagged)
            }
        }
    }

    // MARK: - Image pipeline

    /// Hashes an image before it is decoded, so a byte-identical copy of one
    /// already analyzed (or being analyzed) in this scan, or analyzed by an
    /// earlier scan, takes that result.
    private static func processImage(
        discovered: DiscoveredFile,
        worker: VisionWorker,
        started: CFAbsoluteTime,
        twins: TwinRegistry?,
        catalog: CatalogTwins?,
        options: ScanCatchUp.ScanOptions
    ) async -> TaggedFile {
        let plan = await planImage(discovered: discovered, started: started, twins: twins, catalog: catalog)
        return await finishImage(plan, discovered: discovered, worker: worker, started: started, twins: twins, options: options)
    }

    struct TwinOwnership: Sendable {
        let key: TwinKey
        let models: String
    }

    /// What is left to do for an image once its hash and twin claim are known.
    enum ImagePlan: Sendable {
        case done(TaggedFile)
        case analyze(hash: ImageHash, heif: HEIFDecoder.FileBytes?, owner: TwinOwnership?)
    }

    static func planImage(
        discovered: DiscoveredFile,
        started: CFAbsoluteTime,
        twins: TwinRegistry?,
        catalog: CatalogTwins?
    ) async -> ImagePlan {
        guard let twins, TwinReuse.eligible(sizeBytes: discovered.sizeBytes), !Task.isCancelled else {
            return .analyze(hash: .alongside, heif: nil, owner: nil)
        }
        let early = await hashBeforeDecode(discovered)
        guard let hash = early.hash else {
            return .analyze(hash: .known(nil), heif: nil, owner: nil)
        }
        let key = TwinKey(hash: hash, size: discovered.sizeBytes)
        let models = TwinReuse.modelFingerprint()
        let hashMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
        switch twins.claim(key, models: models) {
        case .ready(let twin):
            twins.noteReuse()
            return .done(twinCopy(of: twin, for: discovered, contentHash: hash, elapsedMs: hashMs))
        case .pending:
            if let twin = await twins.wait(key, timeout: TwinReuse.waitSeconds), twin.models == models {
                twins.noteReuse()
                return .done(twinCopy(of: twin, for: discovered, contentHash: hash, elapsedMs: hashMs))
            }
            return .analyze(hash: .known(hash), heif: nil, owner: nil)
        case .owner:
            if let catalog, catalog.contains(key),
               let found = await catalog.analysis(for: key, discovered: discovered, models: models) {
                twins.publish(key, found.twin, analyzedHere: false)
                twins.noteCatalogReuse()
                let elapsedMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
                return .done(found.ownRow
                    ? unchangedContent(of: found.twin, for: discovered, contentHash: hash, elapsedMs: elapsedMs)
                    : twinCopy(of: found.twin, for: discovered, contentHash: hash, elapsedMs: elapsedMs))
            }
            return .analyze(hash: .known(hash), heif: early.heif, owner: TwinOwnership(key: key, models: models))
        }
    }

    static func finishImage(
        _ plan: ImagePlan,
        discovered: DiscoveredFile,
        worker: VisionWorker,
        started: CFAbsoluteTime,
        twins: TwinRegistry?,
        options: ScanCatchUp.ScanOptions,
        predecoded: PredecodedImage? = nil
    ) async -> TaggedFile {
        switch plan {
        case .done(let tagged):
            return tagged
        case .analyze(let hash, let heif, let owner):
            let analysis = await analyzeImage(
                discovered: discovered, worker: worker, started: started, hash: hash, heif: heif,
                predecoded: predecoded, options: options)
            if let owner, let twins {
                // A model that loaded while this file was analyzed may have missed it.
                let reusable = !analysis.tagged.failed && analysis.tagged.tagsEvaluated
                    && TwinReuse.modelFingerprint() == owner.models
                twins.publish(owner.key, reusable
                    ? TwinAnalysis(tagged: analysis.tagged, modelTags: analysis.modelTags, models: owner.models) : nil)
            }
            return analysis.tagged
        }
    }

    // MARK: - Decode-ahead

    /// A scan image decoded before an inference worker was free for it.
    struct PredecodedImage: @unchecked Sendable {
        let image: CGImage?
        let exif: (cameraModel: String?, lat: Double?, lon: Double?, capturedAt: Date?)
        let loadMs: Double

        var bytes: Int { image.map { $0.bytesPerRow * $0.height } ?? 0 }
    }

    /// A file after the decode-ahead stage: hashed, its twin claim settled and,
    /// when it needs analysis, decoded.
    struct PreparedFile: Sendable {
        enum Stage: Sendable {
            /// Not an image: the worker runs the whole file as it always has.
            case whole
            case image(ImagePlan, PredecodedImage?)
        }
        let discovered: DiscoveredFile
        let stage: Stage
        /// Time spent hashing and decoding; waiting in the queue is not counted.
        let prepSeconds: Double

        var decodedBytes: Int {
            if case .image(_, let decoded?) = stage { return decoded.bytes }
            return 0
        }

        /// A copy that reused a twin: nothing is left for a worker to do.
        var finishedCopy: TaggedFile? {
            guard case .image(.done(var tagged), _) = stage else { return nil }
            tagged.fileRef = discovered.fileRef
            return tagged
        }
    }

    static var scanMaxPixels: Int {
        ProcessInfo.processInfo.environment["FILEID_SCAN_MAX_PIXELS"]
            .flatMap { Int($0) }.map { max(256, min(4096, $0)) } ?? 1536
    }

    /// Hash, twin claim and (when `decode`) the bounded decode, with the same
    /// calls a worker makes, so a prepared file finishes with the same result.
    static func prepare(
        discovered: DiscoveredFile, twins: TwinRegistry?, catalog: CatalogTwins?, decode: Bool
    ) async -> PreparedFile {
        guard discovered.kind == .image, !discovered.movedFromKnownRow else {
            return PreparedFile(discovered: discovered, stage: .whole, prepSeconds: 0)
        }
        let started = CFAbsoluteTimeGetCurrent()
        let plan = await planImage(discovered: discovered, started: started, twins: twins, catalog: catalog)
        guard case .analyze(let hash, let heif, let owner) = plan else {
            return PreparedFile(discovered: discovered, stage: .image(plan, nil),
                                prepSeconds: CFAbsoluteTimeGetCurrent() - started)
        }
        guard decode, !Task.isCancelled else {
            return PreparedFile(discovered: discovered, stage: .image(.analyze(hash: hash, heif: nil, owner: owner), nil),
                                prepSeconds: CFAbsoluteTimeGetCurrent() - started)
        }
        let heifBox = ScanSideResult<HEIFDecoder.FileBytes>()
        if let heif { heifBox.set(heif) }
        let decoded: PredecodedImage = await withCheckedContinuation { continuation in
            visionQueue.async {
                continuation.resume(returning: autoreleasepool {
                    let loadStart = CFAbsoluteTimeGetCurrent()
                    let loaded = loadImageAndEXIF(
                        url: discovered.url, sizeBytes: discovered.sizeBytes, heifFile: heifBox.take())
                    return PredecodedImage(
                        image: loaded?.0, exif: loaded?.1 ?? (nil, nil, nil, nil),
                        loadMs: (CFAbsoluteTimeGetCurrent() - loadStart) * 1000)
                })
            }
        }
        return PreparedFile(discovered: discovered, stage: .image(.analyze(hash: hash, heif: nil, owner: owner), decoded),
                            prepSeconds: CFAbsoluteTimeGetCurrent() - started)
    }

    static func processFile(
        prepared: PreparedFile,
        worker: VisionWorker,
        twins: TwinRegistry?,
        catalog: CatalogTwins? = nil,
        options: ScanCatchUp.ScanOptions = .live
    ) async -> TaggedFile {
        let discovered = prepared.discovered
        guard case .image(let plan, let predecoded) = prepared.stage else {
            return await processFile(discovered: discovered, worker: worker, twins: twins, catalog: catalog, options: options)
        }
        var tagged = await finishImage(
            plan, discovered: discovered, worker: worker,
            started: CFAbsoluteTimeGetCurrent() - prepared.prepSeconds,
            twins: twins, options: options, predecoded: predecoded)
        await finalize(&tagged, discovered: discovered)
        return tagged
    }

    /// Releases the twin claim of a prepared file that will not be analyzed
    /// (cancelled scan), so copies waiting on it stop waiting.
    static func discard(_ prepared: PreparedFile, twins: TwinRegistry?) {
        if case .image(.analyze(_, _, let owner?), _) = prepared.stage {
            twins?.publish(owner.key, nil)
        }
    }

    private struct EarlyHash: Sendable {
        var hash: Data?
        var heif: HEIFDecoder.FileBytes?
    }

    /// The full-file hash, bounded like the later one. A HEIC is read once
    /// here and the same bytes go to the decoder.
    private static func hashBeforeDecode(_ discovered: DiscoveredFile) async -> EarlyHash {
        let url = discovered.url
        let size = UInt64(max(0, discovered.sizeBytes))
        let isHEIC = ["heic", "heif"].contains(url.pathExtension.lowercased())
        let state = OnceContinuation<EarlyHash>()
        let giveUp = DispatchWorkItem { state.finish(EarlyHash()) }
        let result = await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                state.install(continuation, ifFinished: EarlyHash())
                visionQueue.async {
                    if isHEIC, let file = HEIFDecoder.load(path: url.path) {
                        state.finish(EarlyHash(hash: ContentHash.compute(bytes: file.raw), heif: file))
                    } else {
                        state.finish(EarlyHash(hash: ContentHash.compute(url: url, size: size)))
                    }
                }
                visionQueue.asyncAfter(deadline: .now() + hashTimeoutSeconds, execute: giveUp)
            }
        } onCancel: {
            state.finish(EarlyHash())
        }
        giveUp.cancel()
        return result
    }

    enum ImageHash: Sendable {
        /// Read the file for its hash while the models run.
        case alongside
        /// Already attempted before decode; nil when that read failed.
        case known(Data?)
    }

    private static func analyzeImage(
        discovered: DiscoveredFile,
        worker: VisionWorker,
        started: CFAbsoluteTime,
        hash: ImageHash,
        heif: HEIFDecoder.FileBytes? = nil,
        predecoded: PredecodedImage? = nil,
        options: ScanCatchUp.ScanOptions
    ) async -> (tagged: TaggedFile, modelTags: [String]) {
        let heifBox = ScanSideResult<HEIFDecoder.FileBytes>()
        if let heif { heifBox.set(heif) }
        return await withCheckedContinuation { (cont: CheckedContinuation<(tagged: TaggedFile, modelTags: [String]), Never>) in
            visionQueue.async {
                let result = autoreleasepool { () -> (tagged: TaggedFile, modelTags: [String]) in
                    let url = discovered.url
                    let ext = url.pathExtension.lowercased()
                    let sizeMB = Double(discovered.sizeBytes) / 1_048_576
                    var knownHash: Data? = nil
                    if case .known(let value) = hash { knownHash = value }

                    let loadStart = CFAbsoluteTimeGetCurrent()
                    let loaded: (CGImage, (cameraModel: String?, lat: Double?, lon: Double?, capturedAt: Date?))?
                    if let predecoded {
                        loaded = predecoded.image.map { ($0, predecoded.exif) }
                    } else {
                        loaded = loadImageAndEXIF(url: url, sizeBytes: discovered.sizeBytes, heifFile: heifBox.take())
                    }
                    guard let (cgImage, exif) = loaded else {
                        JSONLog.shared.warn(ev: "image_decode_failed", path: redactPathForLog(url.path))
                        return (TaggedFile(
                            url: url, kind: "image", extension: ext,
                            sizeBytes: discovered.sizeBytes,
                            createdAt: discovered.creationDate,
                            modifiedAt: discovered.modificationDate,
                            contentHash: knownHash,
                            failed: true,
                            errorMessage: "Could not decode image",
                            perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000
                        ), [])
                    }
                    let loadMs = predecoded?.loadMs ?? (CFAbsoluteTimeGetCurrent() - loadStart) * 1000

                    // CLIP runs on the GPU and the file hash on the disk while
                    // Vision uses the Neural Engine below; join before building
                    // the result. All three only read `cgImage`.
                    let sideWork = DispatchGroup()
                    let hashWork = DispatchGroup()
                    let clipBox = ScanSideResult<(blob: Data?, ms: Double)>()
                    let hashBox = ScanSideResult<Data?>()
                    DispatchQueue.global(qos: .userInitiated).async(group: sideWork) {
                        let clipStart = CFAbsoluteTimeGetCurrent()
                        let blob = MobileCLIPService.shared.embedImage(cgImage).map { FloatBlob.encode($0) }
                        clipBox.set((blob, (CFAbsoluteTimeGetCurrent() - clipStart) * 1000))
                    }
                    // The worker blocks on this join, so the read must not run below its priority.
                    if case .alongside = hash {
                        DispatchQueue.global(qos: .userInitiated).async(group: hashWork) {
                            hashBox.set(ContentHash.compute(url: url, size: UInt64(max(0, discovered.sizeBytes))))
                        }
                    } else {
                        hashBox.set(knownHash)
                    }

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
                    // mirroring the Windows per-file-timeout row.
                    if !pass.didComplete {
                        sideWork.wait()
                        _ = hashWork.wait(timeout: .now() + hashTimeoutSeconds)
                        JSONLog.shared.warn(ev: "vision_pass_timeout", path: redactPathForLog(url.path))
                        return (TaggedFile(
                            url: url, kind: "image", extension: ext,
                            sizeBytes: discovered.sizeBytes,
                            createdAt: discovered.creationDate,
                            modifiedAt: discovered.modificationDate,
                            contentHash: hashBox.get() ?? nil,
                            failed: true,
                            errorMessage: FileIDEngineMain.visionTimeoutMessage,
                            perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000
                        ), [])
                    }

                    // RAM++ primary tagger (4585-class, Apache-2.0) — the macOS
                    // lockstep mirror of the Windows stack. Falls back to the
                    // Vision classifier's narrow scene vocabulary when the RAM++
                    // ONNX isn't installed (tag() returns [] until load() succeeds),
                    // so tagging always works. RAM++ carries per-tag confidences →
                    // tags.score; the Vision fallback has none. Runs before OCR so
                    // its paper/handwriting tags can gate the text read.
                    // Deferred mode skips it here: the row gets the classifier's
                    // tags now and ScanCatchUp adds the RAM++ ones after the main
                    // pass, through the same `imageTags`/`readText` rules.
                    let deferred = options.mode == .deferred
                    let ramStart = CFAbsoluteTimeGetCurrent()
                    let imageTags = ScanCatchUp.imageTags(
                        ram: deferred ? [] : options.tag(cgImage),
                        classifyTags: pass.classifyTags, vision: pass.classifyConfidences,
                        faceCount: pass.faceCount, cameraModel: exif.cameraModel,
                        creationDate: exif.capturedAt ?? discovered.creationDate)
                    let ramMs = deferred ? 0 : (CFAbsoluteTimeGetCurrent() - ramStart) * 1000

                    // OCR — fast read when the classifiers or the page shape suggest
                    // text, then Vision's accurate tier only when TextReading says the
                    // fast read missed or mangled it (handwriting). The OCR stage "ran"
                    // iff we entered this branch; the DBWriter gates its ocr_text
                    // delete/reinsert on ocrStageRan so a photo we never OCR'd (or a
                    // primary-pass timeout) can't wipe valid prior OCR text. Mirrors
                    // the Windows `ocr_stage_ran` gate.
                    let textTags = imageTags.textTags
                    var read = ScanCatchUp.TextOutcome()
                    var ocrStageRan = false
                    let documentShape = TextReading.isDocumentShape(
                        width: cgImage.width, height: cgImage.height, hasCamera: exif.cameraModel != nil
                    )
                    if ScanCatchUp.textGate(classifyTags: pass.classifyTags, textTags: textTags, documentShape: documentShape) {
                        ocrStageRan = true
                        read = ScanCatchUp.readText(
                            cgImage, textTags: textTags, documentShape: documentShape,
                            hasCamera: exif.cameraModel != nil,
                            fast: { worker.ocrFast($0) },
                            careful: options.carefulRead ?? { worker.ocrAccurateIfAvailable($0) })
                    }
                    let ocr = read.text
                    let ocrMs = read.fastMs
                    let ocrCarefulMs = read.carefulMs
                    let ocrHandwritten = read.handwritten
                    var owed = (deferred ? ScanCatchUp.tagsOwed : 0) | (read.carefulOwed ? ScanCatchUp.carefulTextOwed : 0)
                    if owed != 0, ocrStageRan { owed |= ScanCatchUp.textReadAtScan }

                    let phash = computeDHash(cgImage)
                    // Byte-exact identity for Cleanup's literal-duplicate detection
                    // (item 1) — computed from the file bytes, lockstep in structure
                    // with the Windows engine's content_hash. nil on read error, in
                    // which case the file simply won't participate in dedup.
                    let aesthetic = lightweightAesthetic(cgImage: cgImage, fileSizeMB: sizeMB)
                    let tagScores = imageTags.scores
                    let primaryTags = imageTags.primary
                    // Primary tags plus the free EXIF-derived ones (Year, camera
                    // family), deduplicated and capped; those carry no score.
                    let dedupedTags = imageTags.stored

                    sideWork.wait()
                    // A stalled network volume must not park this worker for good.
                    _ = hashWork.wait(timeout: .now() + hashTimeoutSeconds)
                    let contentHash = hashBox.get() ?? nil
                    let clipBlob = clipBox.get()?.blob ?? nil
                    let clipMs = clipBox.get()?.ms ?? 0

                    var tagged = TaggedFile(
                        url: url, kind: "image", extension: ext,
                        sizeBytes: discovered.sizeBytes,
                        createdAt: exif.capturedAt ?? discovered.creationDate,
                        modifiedAt: discovered.modificationDate,
                        visionTags: dedupedTags,
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
                        faceMinDimPx: pass.faceMinDimPx,
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
                    tagged.ramMs = ramMs
                    tagged.ocrMs = ocrMs
                    tagged.ocrCarefulMs = ocrCarefulMs
                    tagged.ocrHandwritten = ocrHandwritten
                    tagged.capturedFromEXIF = exif.capturedAt != nil
                    if owed != 0 {
                        tagged.catchUp = ScanCatchUp.Marker(
                            stages: owed,
                            vision: ScanCatchUp.VisionContext(
                                classifyTags: pass.classifyTags, confidences: pass.classifyConfidences,
                                textTags: deferred ? nil : textTags).encoded,
                            faceCount: pass.faceCount)
                    }
                    return (tagged, primaryTags)
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
                let keyframe = boundedVideoKeyframe(url: url, maxPixelSize: 512, timeout: 6)
                if let cg = keyframe.image,
                   let blob = MobileCLIPService.shared.embedImage(cg)
                        .map({ FloatBlob.encode($0) }) {
                    tagged.clipEmbeddingBlob = blob
                }
                // A video with no usable keyframe can never get its embedding; record the
                // attempt so the CLIP carve-out stops sending it through every scan. Not
                // on a timeout (the volume may be slow today) or before CLIP can run.
                if tagged.clipEmbeddingBlob == nil, !keyframe.timedOut, MobileCLIPService.shared.canEmbed {
                    tagged.textStageDone = true
                }
                tagged.perFileTotalMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
                cont.resume(returning: tagged)
            }
        }
    }

    /// Extract a video keyframe with a hard wall-clock bound. AVFoundation duration/decode
    /// can hang on an unresponsive NAS file, so it runs in a detached task and is cancelled
    /// when the caller's timeout expires.
    private static func boundedVideoKeyframe(url: URL, maxPixelSize: Int, timeout: TimeInterval) -> (image: CGImage?, timedOut: Bool) {
        let box = KeyframeBox()
        let sema = DispatchSemaphore(value: 0)
        let task = Task.detached(priority: .utility) {
            box.set(await DeepAnalyze.extractVideoKeyframe(url: url, maxPixelSize: maxPixelSize))
            sema.signal()
        }
        if sema.wait(timeout: .now() + timeout) == .timedOut {
            task.cancel()
            return (nil, true)
        }
        return (box.get(), false)
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
                let extracted = bgeTextAndEmbedding(url: url)
                tagged.docText = extracted.text
                tagged.textEmbeddingBlob = extracted.blob
                if let text = extracted.text {
                    let keywords = DocumentKeywords.extract(text)
                    tagged.visionTags.append(contentsOf: keywords.map(\.label))
                    tagged.tagScores = Dictionary(
                        uniqueKeysWithValues: keywords.map { ($0.label, $0.score) }
                    )
                }
                tagged.textStageDone = true   // attempted — stops the backfill carve-out re-walk
                tagged.perFileTotalMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
                cont.resume(returning: tagged)
            }
        }
    }

    /// Extract a document's text ONCE and return both the raw text (for `doc_text` /
    /// doc_fts full-text search) and its BGE semantic embedding blob (for `text_embeddings`).
    /// Extraction — PDFKit on a large PDF especially — is the expensive step, so the two
    /// consumers share a single `DocText.extract` (never extract twice). The text is captured
    /// even when BGE isn't installed (blob nil), so doc_fts keyword search still works —
    /// mirrors the Windows decoder-thread extract + optional BGE embed (tagging.rs). Bounded
    /// by DocText's readers + BGE's 256-token cap. Call ON visionQueue.
    static let storedTextChars = 256 * 1024

    static func bgeTextAndEmbedding(url: URL) -> (text: String?, blob: Data?) {
        guard let text = DocText.extract(path: url.path, maxChars: storedTextChars),
              !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return (nil, nil)
        }
        guard BGETextService.shared.load(modelDir: BGETextService.defaultModelDir),
              let emb = BGETextService.shared.embed(String(text.prefix(DocText.embeddingChars))) else {
            return (text, nil)
        }
        return (text, FloatBlob.encode(emb))
    }

    // MARK: - PDF pipeline

    private static let maxPDFRenderPixels: CGFloat = 50_000_000

    /// Large PDFs take the metadata-only fast path before PDFKit. Smaller PDFs extract
    /// a text layer first, then use OCR for up to three pages when no text is available.
    private static func processPDF(
        discovered: DiscoveredFile,
        worker: VisionWorker,
        started: CFAbsoluteTime,
        options: ScanCatchUp.ScanOptions
    ) async -> TaggedFile {
        let url = discovered.url
        let sizeMB = Double(discovered.sizeBytes) / 1_048_576
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
                // De-dup the NAS read: extract the PDF's text layer ONCE (PDFKit via
                // DocText) and reuse it for both doc_fts and the BGE embedding. A
                // text-layer PDF — the common case — yields text here, so the
                // CGPDFDocument rasterize + OCR pass is SKIPPED: that pass exists only
                // to recover text from image-only/scanned PDFs, so re-opening the file
                // to OCR pages whose text we already have is pure redundant I/O (the
                // user's 1039-PDF NAS corpus). Mirrors the Windows engine, which gets
                // PDF text via pdfium and never raster-OCRs a PDF.
                let extracted = bgeTextAndEmbedding(url: url)
                var result = autoreleasepool { () -> TaggedFile in
                    if let docText = extracted.text, !docText.isEmpty {
                        let keywords = DocumentKeywords.extract(docText)
                        return TaggedFile(
                            url: url, kind: "pdf", extension: "pdf",
                            sizeBytes: discovered.sizeBytes,
                            createdAt: discovered.creationDate,
                            modifiedAt: discovered.modificationDate,
                            visionTags: ["PDF"] + keywords.map(\.label),
                            tagScores: Dictionary(
                                uniqueKeysWithValues: keywords.map { ($0.label, $0.score) }
                            ),
                            perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000,
                            tagsEvaluated: true
                        )
                    }
                    // No extractable text → scanned/image-only PDF: fall back to the
                    // CGPDFDocument rasterize + first-pages OCR (the macOS recovery the
                    // Windows pdfium path lacks).
                    guard let pdf = CGPDFDocument(url as CFURL) else {
                        // A transient open failure must be recorded as a
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
                    let read = readScannedPDF(
                        pdf, fast: { worker.ocrFast($0) },
                        careful: options.carefulRead ?? { worker.ocrAccurateIfAvailable($0) })
                    var scanned = TaggedFile(
                        url: url, kind: "pdf", extension: "pdf",
                        sizeBytes: discovered.sizeBytes,
                        createdAt: discovered.creationDate,
                        modifiedAt: discovered.modificationDate,
                        visionTags: ["PDF"],
                        ocrText: read.text,
                        perFileTotalMs: (CFAbsoluteTimeGetCurrent() - started) * 1000,
                        tagsEvaluated: true,
                        ocrStageRan: true
                    )
                    scanned.ocrCarefulMs = read.carefulMs
                    scanned.ocrHandwritten = read.handwritten
                    if read.carefulOwed {
                        scanned.catchUp = ScanCatchUp.Marker(
                            stages: ScanCatchUp.carefulTextOwed | ScanCatchUp.textReadAtScan, vision: nil, faceCount: 0)
                    }
                    return scanned
                }
                // Cache the BGE content embedding for restructure (PDFKit text, not the
                // lossy OCR) — reusing the single extraction above — unless the file
                // failed to open. textStageDone records the attempt (stops the backfill
                // carve-out re-walk).
                if !result.failed {
                    result.docText = extracted.text
                    result.textEmbeddingBlob = extracted.blob
                    result.textStageDone = true
                }
                cont.resume(returning: result)
            }
        }
    }

    /// Text of an image-only PDF: up to three pages rendered and read. `careful`
    /// returns nil when the accurate tier is not available; the fast read then
    /// stands and the careful one is owed.
    static func readScannedPDF(_ pdf: CGPDFDocument, fast: (CGImage) -> String,
                               careful: (CGImage) -> String?) -> ScanCatchUp.TextOutcome {
        var outcome = ScanCatchUp.TextOutcome()
        let pageCount = min(pdf.numberOfPages, 3)
        var fullText: [String] = []
        var handwritten = false
        for pageNum in 1..<(pageCount + 1) {
            guard let page = pdf.page(at: pageNum) else { continue }
            let bounds = page.getBoxRect(.mediaBox)
            guard bounds.width > 0, bounds.height > 0 else { continue }
            // The 20 MB gate is byte-size only — a tiny vector PDF with a
            // plotter/poster-size mediaBox (CAD, GIS) would render a multi-GB
            // bitmap at a fixed 2x (data:nil commits w*h*4 bytes), times up to
            // workerCap concurrent files. Clamp the scale so each page stays
            // under the same 50 MP ceiling the Windows engine enforces
            // (MAX_DECODED_PIXELS, tagging.rs).
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
                // An image-only page is a scan, and at most three are read:
                // always take the accurate read too. Handwriting a fast read
                // only half-mangles ("Meeting noteg") would slip the
                // garbled-word gate.
                let fastStart = CFAbsoluteTimeGetCurrent()
                let fastText = fast(cg)
                outcome.fastMs += (CFAbsoluteTimeGetCurrent() - fastStart) * 1000
                let carefulStart = CFAbsoluteTimeGetCurrent()
                let read = careful(cg)
                outcome.carefulMs += (CFAbsoluteTimeGetCurrent() - carefulStart) * 1000
                if read == nil { outcome.carefulOwed = true }
                let carefulText = read ?? ""
                if TextReading.isHandwritten(fastText: fastText, carefulText: carefulText, tags: [], paperLike: true) {
                    handwritten = true
                }
                let text = TextReading.preferred(fast: fastText, careful: carefulText)
                if !text.isEmpty { fullText.append(text) }
            }
        }
        let joined = fullText.joined(separator: "\n\n")
        outcome.text = joined.isEmpty ? nil : joined
        outcome.handwritten = handwritten && !joined.isEmpty
        return outcome
    }

    // MARK: - Helpers

    final class ScanSideResult<T>: @unchecked Sendable {
        private let lock = NSLock()
        private var value: T?
        func set(_ v: T) { lock.lock(); value = v; lock.unlock() }
        func get() -> T? { lock.lock(); defer { lock.unlock() }; return value }
        func take() -> T? { lock.lock(); defer { value = nil; lock.unlock() }; return value }
    }

    // EXIF is read from the SAME CGImageSource as the decode — a separate
    // CGImageSourceCreateWithURL re-opened and re-parsed every file, which
    // on NAS volumes cost ms per image across 14-32 workers.
    // `internal` (not `private`) so the no-redundant-stat property is unit-
    // assertable via @testable — see TaggingLoadSizeTests.
    static func loadImageAndEXIF(
        url: URL,
        sizeBytes: Int64,
        heifFile: HEIFDecoder.FileBytes? = nil
    ) -> (CGImage, (cameraModel: String?, lat: Double?, lon: Double?, capturedAt: Date?))? {
        // Skip files smaller than 256 B — corrupt or zero-byte. Avoids the
        // ImageIO crash mode v1's Session-B-hardening fixed. Reuse the size
        // Discovery (Stage A) already stat'd instead of a second
        // attributesOfItem — one fewer SMB/NFS round-trip per image on NAS.
        //
        // A 0/absent discovered size means UNKNOWN, not tiny — Discovery's
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
        // far the dominant per-file cost. CLIP (256) / RAM++ (384) / phash / OCR all downsample
        //     internally, so they're unaffected by the source size — but face
        //     DETECTION is NOT: at 512 px a face that's ~10% of a 4000 px frame is
        //     only ~50 px, right at Vision's limit, so medium / group / background
        //     faces get missed. 1536 catches them (the prior 512 was the dominant
        //     "faces aren't detected" cause). Tunable via FILEID_SCAN_MAX_PIXELS —
        //     lower = faster scan + fewer faces, higher = slower + more faces.
        // Always generate the bounded decode from the source: embedded JPEG
        // previews are commonly only 160×120 and are too small for face gating.
        let maxPixels = scanMaxPixels
        // With the file already in memory, the same order as decodeScanImage for a
        // HEIC (hardware decoder, then ImageIO) without reading it a second time.
        let decoded: CGImage?
        if let heifFile, isHEIF(src) {
            decoded = HEIFDecoder.decode(file: heifFile, maxPixelSize: maxPixels)
                ?? decodeBoundedImage(src, maxPixelSize: maxPixels)
        } else {
            decoded = decodeScanImage(src, path: url.path, maxPixelSize: maxPixels)
        }
        guard let img = decoded else { return nil }
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
    static func extraTags(cameraModel: String?, creationDate: Date?) -> [String] {
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

    /// Case-insensitive dedup (first-seen casing kept), capped at 16: mirrors the
    /// Windows engine's retain(seen)+truncate(16) so stored tag sets match.
    static func cappedTags(_ tags: [String]) -> [String] {
        var seen = Set<String>()
        var out: [String] = []
        for tag in tags {
            guard seen.insert(tag.lowercased()).inserted else { continue }
            out.append(tag)
            if out.count >= 16 { break }
        }
        return out
    }

    /// Read EXIF camera model, GPS coords and capture time from an already-open source.
    /// The EXIF capture time of an image without decoding it; nil when it has none.
    static func exifCaptureTime(url: URL) -> Date? {
        guard let src = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary) else { return nil }
        return readEXIF(from: src).capturedAt
    }

    private static func readEXIF(from src: CGImageSource) -> (cameraModel: String?, lat: Double?, lon: Double?, capturedAt: Date?) {
        guard let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any]
        else {
            return (nil, nil, nil, nil)
        }
        let exifDict = props[kCGImagePropertyExifDictionary] as? [CFString: Any]
        let capturedAt = [(kCGImagePropertyExifDateTimeOriginal, kCGImagePropertyExifOffsetTimeOriginal, kCGImagePropertyExifSubsecTimeOriginal),
                          (kCGImagePropertyExifDateTimeDigitized, kCGImagePropertyExifOffsetTimeDigitized, kCGImagePropertyExifSubsecTimeDigitized)]
            .lazy.compactMap { key, offsetKey, subsecKey in
                (exifDict?[key] as? String).flatMap { exifCaptureDate($0, offset: exifDict?[offsetKey] as? String) }
                    .map { $0.addingTimeInterval(exifSubsecFraction(exifDict?[subsecKey] as? String)) }
            }.first
        let tiff = props[kCGImagePropertyTIFFDictionary] as? [CFString: Any]
        let cameraModel = tiff?[kCGImagePropertyTIFFModel] as? String
        let gps = props[kCGImagePropertyGPSDictionary] as? [CFString: Any]
        var lat = gps?[kCGImagePropertyGPSLatitude] as? Double
        var lon = gps?[kCGImagePropertyGPSLongitude] as? Double
        if let latRef = gps?[kCGImagePropertyGPSLatitudeRef] as? String, latRef == "S",
           let l = lat { lat = -l }
        if let lonRef = gps?[kCGImagePropertyGPSLongitudeRef] as? String, lonRef == "W",
           let l = lon { lon = -l }
        return (cameraModel, lat, lon, capturedAt)
    }

    /// Mirrors the Rust `exif_datetime_to_unix`: the OffsetTime* companion when
    /// present, else the machine's zone; pre-1990 (reset clocks), future and
    /// placeholder values are rejected.
    /// SubSecTime* digits as a fraction of a second ("5" → 0.5, "123" → 0.123), so
    /// burst frames shot within one second keep their order. Mirrors the Rust
    /// `exif_subsec_fraction`.
    static func exifSubsecFraction(_ text: String?) -> Double {
        let digits = String((text ?? "").trimmingCharacters(in: .whitespaces).prefix { $0.isASCII && $0.isNumber }.prefix(9))
        guard !digits.isEmpty, let value = Double(digits) else { return 0 }
        return value / pow(10, Double(digits.count))
    }

    static func exifCaptureDate(_ text: String, offset: String?, now: Date = Date()) -> Date? {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy:MM:dd HH:mm:ss"
        formatter.timeZone = .current
        if let offset = offset?.trimmingCharacters(in: .whitespaces), offset.count == 6,
           let sign = offset.first, sign == "+" || sign == "-",
           let hours = Int(offset.dropFirst().prefix(2)), let minutes = Int(offset.suffix(2)),
           let zone = TimeZone(secondsFromGMT: (sign == "-" ? -1 : 1) * (hours * 3600 + minutes * 60)) {
            formatter.timeZone = zone
        }
        guard let date = formatter.date(from: text.trimmingCharacters(in: CharacterSet(charactersIn: "\0 "))) else { return nil }
        let unix = date.timeIntervalSince1970
        return unix >= 631_152_000 && unix <= now.timeIntervalSince1970 + 86_400 ? date : nil
    }
}

private final class OnceContinuation<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Value, Never>?
    private var finished = false

    func install(_ continuation: CheckedContinuation<Value, Never>, ifFinished fallback: Value) {
        lock.lock()
        if finished {
            lock.unlock()
            continuation.resume(returning: fallback)
            return
        }
        self.continuation = continuation
        lock.unlock()
    }

    func finish(_ value: Value) {
        lock.lock()
        guard !finished else {
            lock.unlock()
            return
        }
        finished = true
        let continuation = continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume(returning: value)
    }
}
