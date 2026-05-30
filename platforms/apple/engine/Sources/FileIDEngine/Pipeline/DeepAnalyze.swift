// Local VLM inference (Qwen / Gemma / PaliGemma) via MLX.
// Caches the loaded ModelContainer across calls so a batch pass
// doesn't re-load weights per file; switching models in Settings
// costs ~10s on M1 to load the new container.
import Foundation
import AVFoundation
import AppKit
import CoreImage
import CommonCrypto
import ImageIO
import QuickLookThumbnailing
import MLX
import MLXLMCommon
import MLXVLM
import Hub
import FileIDShared

public actor DeepAnalyze {
    public static let shared = DeepAnalyze()

    public enum LoadState: Sendable {
        case notLoaded
        case loading(progress: Double, message: String)
        case ready(AIModelKind)
        case failed(String)
    }

    public private(set) var loadState: LoadState = .notLoaded
    private var container: ModelContainer?
    private var loadedKind: AIModelKind?
    private var cancelRequested: Bool = false
    private var prewarmTask: Task<Void, Never>?
    /// Honored by setPrewarmTask if a Cancel arrives before the
    /// JobQueue dispatches the work.
    private var prewarmCancelPending: Bool = false

    private let generateParams = MLXLMCommon.GenerateParameters(
        maxTokens: 320,
        temperature: 0.3,
        topP: 0.9
    )

    private init() {}

    // MARK: - Cancellation

    public func requestCancel() { cancelRequested = true }
    public func clearCancel()   { cancelRequested = false }
    public func isCancelled() -> Bool { cancelRequested }

    public func cancelPrewarm() {
        if let task = prewarmTask {
            task.cancel()
        } else {
            prewarmCancelPending = true
        }
    }

    public func setPrewarmTask(_ task: Task<Void, Never>?) {
        self.prewarmTask = task
        if let task, prewarmCancelPending {
            prewarmCancelPending = false
            task.cancel()
        }
        if task == nil { prewarmCancelPending = false }
    }

    // MARK: - Model lifecycle

    /// Map AIModelKind → MLX ModelConfiguration.
    nonisolated static func vlmConfig(for kind: AIModelKind) -> ModelConfiguration {
        switch kind {
        // Qwen2.5-VL 7B shares the registered 3B's architecture, so a repo-id
        // ModelConfiguration resolves it. Mistral-Small-3.2 is mapped by repo
        // id too; if this MLX-VLM build lacks its architecture, `ensureLoaded`
        // surfaces a load error rather than crashing (verify on-device).
        case .qwen2VL7B:      return ModelConfiguration(id: kind.sourceRepo)
        case .qwen3VL4B:      return VLMRegistry.qwen3VL4BInstruct4Bit
        case .gemma3_4B:      return VLMRegistry.gemma3_4B_qat_4bit
        case .gemma3_12B:     return VLMRegistry.gemma3_12B_qat_4bit
        case .mistralSmall32: return ModelConfiguration(id: kind.sourceRepo)
        case .paligemma3B:    return VLMRegistry.paligemma3bMix448_8bit
        }
    }

    nonisolated static func gpuCacheBudgetMB(for kind: AIModelKind) -> Int {
        switch kind {
        case .gemma3_12B, .mistralSmall32:      return 8_192
        case .qwen2VL7B:                        return 4_096
        case .qwen3VL4B, .gemma3_4B,
             .paligemma3B:                      return 3_072
        }
    }

    /// Idempotent. Progress callback receives (fraction, message,
    /// bytesDone, totalBytes) — last two are swift-transformers'
    /// per-file Progress unit counts (see WelcomeSheet for the
    /// per-file vs aggregate caveat).
    public func ensureLoaded(
        kind: AIModelKind,
        progress: (@Sendable (Double, String, Int64, Int64) -> Void)? = nil
    ) async throws {
        if container != nil, loadedKind == kind {
            loadState = .ready(kind)
            return
        }
        if container != nil {
            container = nil
            loadedKind = nil
            MLX.GPU.clearCache()
        }
        loadState = .loading(progress: 0, message: "Preparing \(kind.displayName)…")
        // Avoid MLX.GPU.set(cacheLimit:) — calling it from the engine's
        // CLI context terminates the process silently.
        JSONLog.shared.info(ev: "deep_load_about_to_loadcontainer",
                            extra: ["kind": AnyCodable(kind.rawValue),
                                    "repo": AnyCodable(kind.sourceRepo)])
        JSONLog.shared.flush()

        do {
            let config = Self.vlmConfig(for: kind)
            let documentsHF = FileManager.default
                .urls(for: .documentDirectory, in: .userDomainMask).first!
                .appending(component: "huggingface")

            // 1. Pre-fetch every file in the repo via 12-way parallel
            //    range GETs. swift-transformers' built-in Hub is
            //    single-stream and dies at ~500 KB/s on per-IP-throttled
            //    CDNs; doing it ourselves multiplies effective throughput.
            let throttle = ProgressThrottle()
            try await VLMDownloader.shared.fetchRepo(
                repo: kind.sourceRepo,
                documentsHF: documentsHF
            ) { frac, done, total in
                let isBoundary = frac <= 0.0 || frac >= 1.0
                guard throttle.shouldPass(boundary: isBoundary) else { return }
                progress?(frac,
                         "Downloading \(kind.displayName) (\(Int(frac * 100))%)",
                         done, total)
            }

            // 2. Files are confirmed on disk. Write the install
            //    sentinel NOW — before any subsequent step (metadata
            //    synthesis, MLX load) that could throw. The user has
            //    paid the cost of the multi-GB download; the install
            //    flow is "done" from their perspective. A later
            //    failure to load into MLX will be retried on first
            //    actual Deep Analyze use, with a focused error
            //    message in the right context.
            Self.writeInstalledSentinel(kind: kind, documentsHF: documentsHF)

            // 3. v1 model dirs may be missing .metadata sidecars (e.g.
            //    Qwen 2.5-VL 3B lacks merges.txt.metadata) which the
            //    newer Hub refuses to load without.
            Self.synthesizeMissingMetadata(
                modelDir: documentsHF.appending(component: "models")
                    .appending(component: kind.sourceRepo)
            )

            // 4. Files are local; HubApi.useOfflineMode = true skips
            //    swift-transformers' slow single-stream fetcher.
            let hub = HubApi(downloadBase: documentsHF, useOfflineMode: true)
            let loaded = try await VLMModelFactory.shared.loadContainer(
                hub: hub,
                configuration: config
            ) { _ in
                // Loading from local files; no remote download.
            }
            JSONLog.shared.info(ev: "deep_loadcontainer_returned",
                                extra: ["kind": AnyCodable(kind.rawValue)])
            JSONLog.shared.flush()
            container = loaded
            loadedKind = kind
            loadState = .ready(kind)
            JSONLog.shared.info(ev: "deep_model_loaded",
                                extra: ["kind": AnyCodable(kind.rawValue)])
            JSONLog.shared.flush()
        } catch {
            JSONLog.shared.error(ev: "deep_loadcontainer_threw",
                                 error: "\(error)")
            JSONLog.shared.flush()
            loadState = .failed("\(error.localizedDescription)")
            throw error
        }
    }

    /// Free GPU weights. Called when the user changes models or shuts
    /// the engine down. Reload costs ~10 s.
    public func unload() {
        container = nil
        loadedKind = nil
        loadState = .notLoaded
        MLX.GPU.clearCache()
    }

    /// Mark a model dir "fully installed" by writing a sentinel file.
    /// Called as soon as VLMDownloader.fetchRepo confirms every file
    /// is on disk — i.e. the user has finished paying the multi-GB
    /// download cost. A later failure inside `loadContainer` doesn't
    /// invalidate the install; first Deep Analyze use will retry the
    /// MLX load and surface the error in context.
    public func markInstalledSentinel(kind: AIModelKind) {
        let documentsHF = FileManager.default
            .urls(for: .documentDirectory, in: .userDomainMask).first!
            .appending(component: "huggingface")
        Self.writeInstalledSentinel(kind: kind, documentsHF: documentsHF)
    }

    /// Static, nonisolated variant — callable from inside ensureLoaded
    /// without an actor hop. Behavior identical to markInstalledSentinel.
    nonisolated static func writeInstalledSentinel(kind: AIModelKind, documentsHF: URL) {
        let modelDir = documentsHF.appending(component: "models")
            .appending(component: kind.sourceRepo)
        let sentinel = modelDir.appendingPathComponent(".fileid-installed")
        try? FileManager.default.createDirectory(
            at: modelDir, withIntermediateDirectories: true)
        try? Data().write(to: sentinel)
    }

    // MARK: - Inference

    public struct AnalysisResult: Sendable {
        public let description: String
        public let proposedName: String?
    }

    public struct FaceComparison: Sendable {
        public let sameClass: Bool
        public let confidence: Float    // 0.0 – 1.0
    }

    /// Ask the VLM whether two face crops show the same person. Used by
    /// the post-clustering pass to resolve the borderline L2 band that
    /// the bootstrap face-print clustering can't reliably classify.
    public func compareFaces(cropA: URL, cropB: URL) async -> FaceComparison {
        guard let container else {
            return FaceComparison(sameClass: false, confidence: 0)
        }
        let ciA: CIImage? = autoreleasepool {
            guard let cg = Self.loadCGImage(url: cropA, maxPixelSize: 256) else { return nil }
            return CIImage(cgImage: cg)
        }
        let ciB: CIImage? = autoreleasepool {
            guard let cg = Self.loadCGImage(url: cropB, maxPixelSize: 256) else { return nil }
            return CIImage(cgImage: cg)
        }
        guard let ciA, let ciB else {
            return FaceComparison(sameClass: false, confidence: 0)
        }

        let systemPrompt = """
        You are a face-matching assistant. You will see two cropped face photos. Answer in EXACTLY this format on two lines:

        VERDICT: SAME or DIFFERENT
        CONFIDENCE: a single number 0.0 to 1.0

        Only reply with those two lines. Lighting, angle, glasses, age, and hairstyle differences are normal and should not by themselves justify DIFFERENT — focus on facial structure.
        """

        let collector = TokenCollector()
        let params = generateParams
        do {
            try await container.perform { (context: ModelContext) -> Void in
                let chat: [Chat.Message] = [
                    .system(systemPrompt),
                    .user("Are these two cropped face photos of the same person?",
                          images: [.ciImage(ciA), .ciImage(ciB)], videos: [])
                ]
                var userInput = UserInput(chat: chat)
                userInput.processing.resize = .init(width: 256, height: 256)
                let lmInput = try await context.processor.prepare(input: userInput)
                let stream = try MLXLMCommon.generate(
                    input: lmInput, parameters: params, context: context
                )
                for await item in stream {
                    if let chunk = item.chunk { collector.append(chunk) }
                }
            }
        } catch {
            JSONLog.shared.warn(ev: "vlm_compare_failed", error: "\(error)")
            return FaceComparison(sameClass: false, confidence: 0)
        }
        // Clear MLX cache every 50 calls. Per-call clearing thrashes
        // the scratch allocator.
        Self.compareCallsSinceClear &+= 1
        if Self.compareCallsSinceClear >= 50 {
            MLX.GPU.clearCache()
            Self.compareCallsSinceClear = 0
        }
        let raw = collector.snapshot()
        // Sample the raw VLM output for the first 10 calls so we can
        // diagnose model output formats without re-running the pass.
        Self.compareSampleLogged &+= 1
        if Self.compareSampleLogged <= 10 {
            let sample = raw.prefix(200).replacingOccurrences(of: "\n", with: " | ")
            JSONLog.shared.info(ev: "vlm_compare_raw_sample",
                                extra: ["call": AnyCodable(Self.compareSampleLogged),
                                        "raw": AnyCodable(String(sample))])
        }
        return Self.parseFaceComparison(raw)
    }

    nonisolated(unsafe) private static var compareSampleLogged: Int = 0

    nonisolated(unsafe) private static var compareCallsSinceClear: Int = 0

    /// Parse the VLM's response into a typed result. Robust against
    /// models that drop the structured `VERDICT:` / `CONFIDENCE:`
    /// prefixes — without a confidence default, loosely-formatted SAME
    /// verdicts would never clear the auto-merge threshold.
    private static func parseFaceComparison(_ raw: String) -> FaceComparison {
        let upper = raw.uppercased()
        let saidDifferent = upper.contains("DIFFERENT")
        let saidSame = upper.contains("VERDICT: SAME")
            || (!saidDifferent && upper.contains("SAME"))
        let same = saidSame && !saidDifferent

        var conf: Float = 0
        var explicitlyParsed = false
        if let r = upper.range(of: "CONFIDENCE:") {
            let after = String(upper[r.upperBound...])
            let scanner = Scanner(string: after)
            scanner.charactersToBeSkipped = .whitespacesAndNewlines.union(.letters)
            if let parsed = scanner.scanDouble() {
                // Normalize percent-form (e.g. `92`) to fraction.
                let normalized = parsed > 1 ? parsed / 100 : parsed
                conf = Float(max(0, min(1, normalized)))
                explicitlyParsed = true
            }
        }
        // Default to 0.80 when the verdict is clear but no confidence
        // number was returned. Clears the 0.75 auto-merge threshold;
        // explicit numbers from compliant models still take precedence.
        if !explicitlyParsed && (same || saidDifferent) {
            conf = 0.80
        }
        return FaceComparison(sameClass: same, confidence: conf)
    }

    /// Run the VLM on a single image URL. Returns description + a
    /// suggested human-readable filename. Caller must `ensureLoaded`
    /// first (cheap if already loaded).
    ///
    /// V14.9-L1: optional `onToken` callback fires once per MLX-emitted
    /// chunk so a streaming UI can render the partial caption as the
    /// model generates it. Callbacks are awaited inline; throttle on
    /// the caller side if the consumer is slow (caller throttles to 4 Hz
    /// in DeepAnalyzeRunner so the IPC sink isn't flooded).
    public func analyze(imageURL: URL, faceNames: [String] = [], onToken: (@Sendable (String) async -> Void)? = nil) async -> AnalysisResult {
        guard let container else {
            return AnalysisResult(description: "Model not loaded.", proposedName: nil)
        }
        // Decode image at 768 px max — good detail for the 448-input VLM
        // without blowing memory on RAW or huge JPEGs.
        let ciImage: CIImage? = autoreleasepool {
            guard let cg = Self.loadCGImage(url: imageURL, maxPixelSize: 768) else { return nil }
            return CIImage(cgImage: cg)
        }
        guard let ciImage else {
            return AnalysisResult(description: "Could not decode image.", proposedName: nil)
        }

        // Build the prompt. Face names (if face clustering has run) are
        // injected as context so the VLM can reference people by their
        // assigned name instead of "the person on the left".
        let nameContext: String
        if faceNames.isEmpty {
            nameContext = ""
        } else {
            let list = faceNames.joined(separator: ", ")
            nameContext = "\nKnown people in this photo: \(list). Use these names if appropriate."
        }
        let systemPrompt = """
        You are a concise image-understanding assistant for a personal photo organizer.
        Given an image, reply with EXACTLY two sections:

        DESCRIPTION: A 1-2 sentence natural description in plain English. Mention people by name if known.
        FILENAME: A short human-readable filename (no extension). Lowercase words separated by underscores. 4-9 words. Avoid generic terms like "image" or "photo". Examples: "mom_playing_piano_living_room", "adam_at_grand_canyon_2019", "wedding_first_dance_venue".

        Do NOT speculate about identities of people not listed.\(nameContext)
        """

        let collector = TokenCollector()
        let params = generateParams
        do {
            try await container.perform { (context: ModelContext) -> Void in
                let chat: [Chat.Message] = [
                    .system(systemPrompt),
                    .user("Describe this image and propose a filename.",
                          images: [.ciImage(ciImage)], videos: [])
                ]
                var userInput = UserInput(chat: chat)
                userInput.processing.resize = .init(width: 448, height: 448)
                let lmInput = try await context.processor.prepare(input: userInput)
                let stream = try MLXLMCommon.generate(
                    input: lmInput, parameters: params, context: context
                )
                for await item in stream {
                    if let chunk = item.chunk {
                        collector.append(chunk)
                        // V14.9-L1: per-token callback for live caption streaming.
                        if let onToken { await onToken(chunk) }
                    }
                }
            }
        } catch {
            return AnalysisResult(description: "Inference failed: \(error.localizedDescription)",
                                   proposedName: nil)
        }
        let raw = collector.snapshot().trimmingCharacters(in: .whitespacesAndNewlines)
        let parsed = Self.parse(rawOutput: raw)
        // Drain MLX scratch periodically — keeps weights resident, drops
        // per-image temporary tensors.
        MLX.GPU.clearCache()
        return parsed
    }

    /// Parse the strict-format VLM output into description + filename.
    /// Defensive: if the model deviates from the format, fall back to
    /// using the whole reply as the description and skipping the name.
    private static func parse(rawOutput: String) -> AnalysisResult {
        var description = rawOutput
        var name: String? = nil
        // Look for "DESCRIPTION:" + "FILENAME:" markers.
        if let dRange = rawOutput.range(of: "DESCRIPTION:", options: .caseInsensitive) {
            let afterD = rawOutput[dRange.upperBound...]
            if let fRange = afterD.range(of: "FILENAME:", options: .caseInsensitive) {
                description = String(afterD[..<fRange.lowerBound])
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                let afterF = afterD[fRange.upperBound...]
                let firstLine = afterF
                    .split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: true)
                    .first.map(String.init)?
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                name = firstLine.flatMap { sanitize(filename: $0) }
            } else {
                description = String(afterD).trimmingCharacters(in: .whitespacesAndNewlines)
            }
        }
        return AnalysisResult(description: description, proposedName: name)
    }

    /// Strip extension, slugify, cap length.
    private static func sanitize(filename raw: String) -> String? {
        var s = raw
        // Strip any trailing extension the model added.
        if let dot = s.lastIndex(of: ".") {
            let ext = s[s.index(after: dot)...]
            if ext.count <= 5 { s = String(s[..<dot]) }
        }
        // Slugify.
        let allowed = Set("abcdefghijklmnopqrstuvwxyz0123456789_-")
        let lower = s.lowercased()
            .replacingOccurrences(of: " ", with: "_")
            .replacingOccurrences(of: "-", with: "_")
        let cleaned = String(lower.unicodeScalars.compactMap { sc -> Character? in
            let c = Character(sc)
            return allowed.contains(c) ? c : nil
        })
        guard cleaned.count >= 3, cleaned.count <= 80 else { return nil }
        return cleaned
    }

    // MARK: - Self-heal old model dirs

    /// Walk a model dir and generate `.cache/huggingface/download/<file>.metadata`
    /// sidecars for any top-level files that don't have one yet. The Hub
    /// in offline mode refuses to load without these. Format is 3 lines:
    ///   line 1: commit hash (any 40-hex; we copy from a peer if found,
    ///           else use a placeholder of zeros which Hub treats as
    ///           "any version")
    ///   line 2: git blob hash of the file content
    ///   line 3: timestamp
    nonisolated static func synthesizeMissingMetadata(modelDir: URL) {
        let fm = FileManager.default
        let dlDir = modelDir.appending(component: ".cache/huggingface/download", directoryHint: .isDirectory)
        guard fm.fileExists(atPath: modelDir.path) else { return }
        try? fm.createDirectory(at: dlDir, withIntermediateDirectories: true)
        // Find a representative commit hash from any existing metadata
        // sidecar; fall back to all-zeros if none exist yet.
        var commitHash = String(repeating: "0", count: 40)
        if let entries = try? fm.contentsOfDirectory(at: dlDir, includingPropertiesForKeys: nil) {
            for e in entries where e.lastPathComponent.hasSuffix(".metadata") {
                if let s = try? String(contentsOf: e, encoding: .utf8),
                   let first = s.split(separator: "\n").first,
                   first.count == 40 {
                    commitHash = String(first)
                    break
                }
            }
        }
        guard let topLevel = try? fm.contentsOfDirectory(at: modelDir,
                                                          includingPropertiesForKeys: [.isRegularFileKey]) else {
            return
        }
        var synthesized: [String] = []
        for file in topLevel {
            // Skip directories + dotfiles + .cache itself.
            if file.lastPathComponent.hasPrefix(".") { continue }
            var isDir: ObjCBool = false
            guard fm.fileExists(atPath: file.path, isDirectory: &isDir), !isDir.boolValue else { continue }
            let metaURL = dlDir.appending(component: file.lastPathComponent + ".metadata")
            if fm.fileExists(atPath: metaURL.path) { continue }
            // Compute git-style blob hash: sha1("blob \(size)\0" + content).
            guard let blobHash = gitBlobHash(of: file) else { continue }
            let now = Date().timeIntervalSince1970
            let body = "\(commitHash)\n\(blobHash)\n\(now)\n"
            do {
                try body.write(to: metaURL, atomically: true, encoding: .utf8)
                synthesized.append(file.lastPathComponent)
            } catch {
                JSONLog.shared.warn(ev: "metadata_synth_failed",
                                    path: redactPathForLog(metaURL.path), error: "\(error)")
            }
        }
        if !synthesized.isEmpty {
            JSONLog.shared.info(ev: "metadata_synthesized",
                                extra: ["dir": AnyCodable(redactPathForLog(modelDir.path)),
                                        "files": AnyCodable(synthesized.joined(separator: ","))])
        }
    }

    /// Git's blob hash: sha1 of the literal bytes "blob \(size)\0<content>".
    private static func gitBlobHash(of url: URL) -> String? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        let header = "blob \(data.count)\0"
        var combined = Data(header.utf8)
        combined.append(data)
        return sha1Hex(combined)
    }

    /// SHA-1 hex digest using CommonCrypto (avoids the deprecation
    /// warning from Insecure.SHA1 on more recent SDKs but does the
    /// same thing — git uses SHA-1 for blob hashes by spec).
    private static func sha1Hex(_ data: Data) -> String {
        var digest = [UInt8](repeating: 0, count: 20)
        data.withUnsafeBytes { (ptr: UnsafeRawBufferPointer) in
            _ = CC_SHA1(ptr.baseAddress, CC_LONG(data.count), &digest)
        }
        return digest.map { String(format: "%02x", $0) }.joined()
    }

    // MARK: - Image loader (engine-local, no Vision dependency)

    /// Loads a CGImage from any supported source. For PDFs renders
    /// the first page; for videos extracts a keyframe at ~25% in; for
    /// everything else uses ImageIO thumbnails. Single entry point so
    /// Deep Analyze can caption images, PDFs, and videos through the
    /// same code path.
    nonisolated static func loadCGImage(url: URL, maxPixelSize: Int) -> CGImage? {
        let ext = url.pathExtension.lowercased()
        if ext == "pdf" {
            return renderFirstPDFPage(url: url, maxPixelSize: maxPixelSize)
        }
        if isVideoExtension(ext) {
            return extractVideoKeyframe(url: url, maxPixelSize: maxPixelSize)
        }
        // Try ImageIO first — fast for images via thumbnail decode.
        if let src = CGImageSourceCreateWithURL(url as CFURL, nil) {
            let opts: [CFString: Any] = [
                kCGImageSourceShouldCacheImmediately: true,
                kCGImageSourceCreateThumbnailFromImageIfAbsent: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: maxPixelSize
            ]
            if let cg = CGImageSourceCreateThumbnailAtIndex(src, 0, opts as CFDictionary) {
                return cg
            }
        }
        // Quick Look fallback — handles .docx / .pages / .txt / .md /
        // .key / .numbers / etc. Anything macOS can render a preview
        // for, the VLM can caption. Returns nil if QL doesn't have a
        // generator for this UTI; Deep Analyze silently skips.
        return quickLookThumbnail(url: url, maxPixelSize: maxPixelSize)
    }

    /// Synchronous wrapper around QLThumbnailGenerator. The Quick Look
    /// API is callback-based, but Deep Analyze's loader is synchronous —
    /// so we bridge with a DispatchSemaphore. Only the engine's serial
    /// VLM-prep stage calls this, so blocking briefly is fine.
    nonisolated static func quickLookThumbnail(url: URL, maxPixelSize: Int) -> CGImage? {
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        let req = QLThumbnailGenerator.Request(
            fileAt: url,
            size: CGSize(width: maxPixelSize, height: maxPixelSize),
            scale: 1.0,
            representationTypes: .thumbnail
        )
        let sema = DispatchSemaphore(value: 0)
        // Sendable box for Swift 6 strict-concurrency capture rules —
        // QL's completion runs on a non-actor queue, so we can't mutate
        // a stack var directly.
        let box = ImageBox()
        QLThumbnailGenerator.shared.generateBestRepresentation(for: req) { rep, _ in
            if let rep {
                box.set(rep.cgImage)
            }
            sema.signal()
        }
        // 8-second hard cap. QL can hang on network volumes or
        // unresponsive previewers; we'd rather skip than wedge the
        // whole batch. ImageIO's thumbnail timeout doesn't apply here.
        _ = sema.wait(timeout: .now() + .seconds(8))
        return box.get()
    }

    /// Sendable wrapper for the QL completion handler.
    private final class ImageBox: @unchecked Sendable {
        private let lock = NSLock()
        private var value: CGImage?
        func set(_ v: CGImage?) { lock.lock(); value = v; lock.unlock() }
        func get() -> CGImage? { lock.lock(); defer { lock.unlock() }; return value }
    }

    /// Common video container extensions. Mirrors FileTypes.kind.
    nonisolated static func isVideoExtension(_ ext: String) -> Bool {
        switch ext {
        case "mp4", "m4v", "mov", "avi", "mkv", "webm", "mpg", "mpeg",
             "3gp", "3g2", "wmv", "flv":
            return true
        default:
            return false
        }
    }

    /// Pull a representative keyframe out of a video at ~25% of its
    /// duration. AVAssetImageGenerator handles the I/O + decode and
    /// caps the output to maxPixelSize so RAW 4K frames don't blow
    /// memory. Returns nil if the asset is unreadable (DRM, partial
    /// download, codec we can't decode, etc.) — Deep Analyze then
    /// silently skips the file just like it would for a missing PDF.
    nonisolated static func extractVideoKeyframe(url: URL, maxPixelSize: Int) -> CGImage? {
        let asset = AVURLAsset(url: url, options: [
            AVURLAssetPreferPreciseDurationAndTimingKey: false
        ])
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.requestedTimeToleranceBefore = CMTime(seconds: 0.5, preferredTimescale: 600)
        generator.requestedTimeToleranceAfter  = CMTime(seconds: 0.5, preferredTimescale: 600)
        generator.maximumSize = CGSize(width: maxPixelSize, height: maxPixelSize)

        // Try 25% in first; fall back to 0s if that fails (very short
        // clip or unseekable asset).
        let durationSeconds = CMTimeGetSeconds(asset.duration)
        let target: CMTime
        if durationSeconds.isFinite, durationSeconds > 0 {
            target = CMTime(seconds: durationSeconds * 0.25, preferredTimescale: 600)
        } else {
            target = .zero
        }
        if let cg = try? generator.copyCGImage(at: target, actualTime: nil) {
            return cg
        }
        return try? generator.copyCGImage(at: .zero, actualTime: nil)
    }

    nonisolated static func renderFirstPDFPage(url: URL, maxPixelSize: Int) -> CGImage? {
        guard let pdf = CGPDFDocument(url as CFURL),
              let page = pdf.page(at: 1) else { return nil }
        let bounds = page.getBoxRect(.mediaBox)
        guard bounds.width > 0, bounds.height > 0 else { return nil }
        // Scale so the longer side ≈ maxPixelSize. PDFs are vector;
        // we just need enough resolution for the VLM to read text.
        let longSide = max(bounds.width, bounds.height)
        let scale = CGFloat(maxPixelSize) / longSide
        let w = Int(bounds.width * scale)
        let h = Int(bounds.height * scale)
        guard w > 0, h > 0 else { return nil }
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let ctx = CGContext(
            data: nil, width: w, height: h, bitsPerComponent: 8,
            bytesPerRow: 0, space: cs,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return nil }
        ctx.setFillColor(CGColor(gray: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
        ctx.scaleBy(x: scale, y: scale)
        ctx.drawPDFPage(page)
        return ctx.makeImage()
    }
}

// MARK: - Sendable string accumulator

private final class TokenCollector: @unchecked Sendable {
    private var buffer = ""
    private let lock = NSLock()
    func append(_ s: String) { lock.lock(); buffer += s; lock.unlock() }
    func snapshot() -> String { lock.lock(); defer { lock.unlock() }; return buffer }
}

/// 100ms gate. Caps progress emit rate at ~10 Hz; boundary events
/// (frac == 0 or >= 1) always pass.
private final class ProgressThrottle: @unchecked Sendable {
    private let lock = NSLock()
    private var lastEmitAt: TimeInterval = 0
    private static let intervalSec: TimeInterval = 0.1

    func shouldPass(boundary: Bool) -> Bool {
        let now = Date().timeIntervalSinceReferenceDate
        lock.lock(); defer { lock.unlock() }
        if boundary || (now - lastEmitAt) >= Self.intervalSec {
            lastEmitAt = now
            return true
        }
        return false
    }
}

