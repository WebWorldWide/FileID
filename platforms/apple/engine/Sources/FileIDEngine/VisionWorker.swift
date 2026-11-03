// VisionWorker — wraps Vision request objects (classify, OCR, face rects,
// face feature prints). Reusing one worker per scan task amortizes the
// VNRequest allocation cost; the pool guarantees one owning task at a time.
//
// Same structural concept as v1's VisionWorker, rewritten cleanly:
//  - One bundled "primary pass" handler runs classify + face rects + face
//    prints + saliency in a single VNImageRequestHandler invocation.
//  - OCR runs on demand (only when classify suggests it's a document).
//  - The pool stays an `actor` (the v1 lock-guarded class regression we
//    did in Batch 12 broke perf; reverting to actor was the right call).
import Foundation
import Vision
import CoreImage
import CoreText
import ImageIO
import UniformTypeIdentifiers

/// Global concurrency cap for `VNImageRequestHandler.perform`. Tied to the
/// full worker count (`Hardware.workerCap`) so the Vision/ANE stage scales
/// with the machine: throughput-first, with the per-call timeout below as the
/// safety net for deadlock. (Earlier runs at gate=6 halved throughput from
/// 150 → 75 files/s; the watchdog already handles the stuck-call case the gate
/// was previously protecting against.) Was hardcoded 14 (= M1 Pro's workerCap);
/// hardcoding silently throttled Vision/ANE on higher-core Macs (M-Ultra
/// workerCap 32) — feeding the bigger ANE only 14-wide. Now it scales.
let visionConcurrencyGate = DispatchSemaphore(value: Hardware.workerCap)

/// Per-call hard wall-clock timeout for any `handler.perform` invocation.
/// If Vision hasn't returned by `visionPerformTimeoutSeconds`, we abandon
/// the call and free the worker. The orphaned background thread keeps
/// running until Vision's internal queue eventually unblocks (or the
/// engine exits). Bounded leak, unbounded responsiveness.
let visionPerformTimeoutSeconds: Double = 10.0

/// At most two accurate text reads in flight. Measured on an M1 Pro: fourteen
/// cold concurrent reads each loaded the model and took 70 s wall (every one
/// past the 10 s timeout), two took 3 s, and warm throughput is the same
/// (~210 ms per image) at one, two or fourteen.
let accurateOCRGate = DispatchSemaphore(value: 2)

/// Run `body` on a background queue with a wall-clock timeout. Returns
/// true if body completed before the deadline, false on timeout. On
/// timeout, the body keeps running on its own thread — caller must NOT
/// rely on shared state being settled.
///
/// Box wrapper exists because Vision request types (`VNImageRequestHandler`,
/// `VNRequest` subclasses) are non-Sendable but we need to ferry them to a
/// background dispatch queue. Each Vision call is single-threaded by
/// construction (one box per call, only one thread touches it at a time
/// — the dispatcher OR the caller, never both), so the unchecked-Sendable
/// hop is safe in practice even though the compiler can't prove it.
final class _VisionUncheckedBox<T>: @unchecked Sendable {
    let value: T
    init(_ v: T) { self.value = v }
}

@inline(__always)
func runVisionWithTimeout(_ body: @escaping () -> Void) -> Bool {
    let sem = DispatchSemaphore(value: 0)
    let box = _VisionUncheckedBox(body)
    DispatchQueue.global(qos: .userInitiated).async {
        // Drain this perform's Vision/CoreImage intermediates here: the global
        // concurrent queue's root-queue threads never drain a top-level
        // autorelease pool, and the per-file Tagging pool runs on the caller's
        // visionQueue thread — a different thread — so without this the
        // autoreleased intermediates accumulate for the engine's lifetime.
        autoreleasepool { box.value() }
        sem.signal()
    }
    return sem.wait(timeout: .now() + visionPerformTimeoutSeconds) == .success
}

/// One reusable bundle of VNRequest objects. Not Sendable across concurrent
/// tasks — the pool guarantees exactly one owning task at a time.
public final class VisionWorker: @unchecked Sendable {

    // Reusable requests — created once per worker, reused per file.
    //
    // Iteration 7 trim: removed VNGenerateImageFeaturePrintRequest and
    // VNGenerateAttentionBasedSaliencyImageRequest from the per-file bundle.
    //   - The former generates a SCENE feature print for the whole image (NOT
    //     per-face), which we mislabeled as `facePrints` and don't actually use
    //     downstream. People clustering needs PER-FACE prints, which require a
    //     separate cropped-image pass per face — that lands when we wire the
    //     People tab. For now: drop, save ~25-30 ms ANE per file.
    //   - Saliency was only setting a `hasSalientObject` flag we never read.
    //     Drop entirely.
    // Result: Vision pass shrinks from 4 requests to 2, expected ~50 % cut on
    // the ANE-bound stage.
    // VNRequest objects are allocated PER CALL (see runPrimaryPass), not
    // cached on the worker. Caching them was a data race: the 10s timeout
    // abandons a still-running `handler.perform` on a background thread, the
    // worker is recycled to the next file, and the next perform on the SAME
    // request instances raced the orphaned one — crashing inside Vision or
    // bleeding one image's faces/labels into another file. Allocation cost is
    // trivial next to perform (ocrFast already allocates per call).
    public init() {}

    // Result of the bundled primary pass.
    public struct PrimaryPass: Sendable {
        public var classifyTags: [String]      // labels with confidence >= 0.30, top-8 by confidence
        /// Every classifier label at >= 0.01, for agreement with RAM++.
        public var classifyConfidences: [String: Float] = [:]
        public var faceCount: Int
        public var faceBBoxes: [String]        // "x,y,w,h" normalized
        public var faceQualities: [Double]     // 0..1, parallel to faceBBoxes; -1 if not measured
        public var faceYaws: [Double?]         // radians, parallel to faceBBoxes; nil if missing
        public var facePitches: [Double?]      // radians, parallel to faceBBoxes; nil if missing
        public var faceMinDimPx: [Double]      // min(w,h) in absolute source pixels, parallel to faceBBoxes
        public var facePrints: [Data]          // EMPTY here — extracted lazily in Stage D
        /// False iff `handler.perform` exceeded the wall-clock timeout and was
        /// abandoned (empty result). The tagging stage keys its stage-ran gates
        /// (tagsEvaluated / facesEvaluated) on this so a timed-out pass never
        /// wipes previously-persisted auto-tags or faces (incl. manual
        /// person_id) on a rescan, and marks the file for retry.
        public var didComplete: Bool
    }

    /// Bundled face/scene/saliency Vision pass over `cgImage`.
    ///
    /// Face feature prints are NOT extracted here — running per-face
    /// `VNGenerateImageFeaturePrintRequest` inline causes ANE thrash
    /// when many workers are in flight. We persist only the bbox; the
    /// FaceClustering job extracts prints lazily, one file at a time.
    public func runPrimaryPass(_ cgImage: CGImage) -> PrimaryPass {
        var pass = PrimaryPass(classifyTags: [], faceCount: 0,
                               faceBBoxes: [], faceQualities: [],
                               faceYaws: [], facePitches: [],
                               faceMinDimPx: [],
                               facePrints: [], didComplete: false)

        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        // Per-call request objects — never shared across files (see init note).
        let cReq = VNClassifyImageRequest()
        let fReq = VNDetectFaceRectanglesRequest()
        // Quality + landmark request — runs after fReq in the same perform call.
        let qReq = VNDetectFaceCaptureQualityRequest()
        visionConcurrencyGate.wait()
        // Run Vision with a hard wall-clock timeout so a single bad input
        // can't permanently park this worker. Signal the concurrency gate from
        // INSIDE the worker thread (after perform) so a timed-out, orphaned
        // perform keeps its ANE slot accounted until it actually finishes —
        // the cap stays honest instead of over-admitting while threads stall.
        let didReturn = runVisionWithTimeout { [handler] in
            defer { visionConcurrencyGate.signal() }
            do { try handler.perform([cReq, fReq, qReq]) } catch { }
        }
        if !didReturn {
            return pass   // timed out — didComplete stays false; file marked for retry downstream
        }
        // If perform() threw a non-timeout error, results remain nil — treat
        // identically to a timeout so a rescan retries rather than marking this
        // file evaluated with zero results and silently clearing prior tags.
        guard cReq.results != nil else { return pass }
        pass.didComplete = true

        if let results = cReq.results {
            // 0.30 confidence floor: VNClassifyImageRequest emits ~1300
            // hierarchical labels; at 0.5 most photos cleared 0-2 tags
            // (the user complaint was "tagging seems pointless"). 0.30
            // is the typical recall sweet spot — surfaces multiple
            // useful labels per image without the noise floor of 0.20.
            // Cap at top 8 to keep the per-file payload bounded; results
            // are pre-sorted by confidence descending.
            pass.classifyTags = Array(
                results
                    .filter { $0.confidence >= 0.30 }
                    .prefix(8)
                    .map { $0.identifier }
            )
            for observation in results where observation.confidence >= 0.01 {
                pass.classifyConfidences[observation.identifier] = observation.confidence
            }
        }
        // Index quality observations by bbox so we can align them with the
        // face-rects observations after sorting (the two requests detect
        // independently and may differ in observation order; bbox proximity
        // is the most reliable join key).
        let qualityByBBox: [(CGRect, Double)] = (qReq.results ?? []).map { obs in
            (obs.boundingBox, Double(obs.faceCaptureQuality ?? 0))
        }
        if let faces = fReq.results {
            pass.faceCount = faces.count
            // Sort by area descending so the largest faces come first.
            let sortedFaces = faces.sorted {
                let aArea = $0.boundingBox.width * $0.boundingBox.height
                let bArea = $1.boundingBox.width * $1.boundingBox.height
                return aArea > bArea
            }
            pass.faceBBoxes.reserveCapacity(sortedFaces.count)
            pass.faceQualities.reserveCapacity(sortedFaces.count)
            pass.faceYaws.reserveCapacity(sortedFaces.count)
            pass.facePitches.reserveCapacity(sortedFaces.count)
            pass.faceMinDimPx.reserveCapacity(sortedFaces.count)
            // Vision's boundingBox is normalized to THIS cgImage, which is
            // already the decoded image (see loadImageAndEXIF's
            // FILEID_SCAN_MAX_PIXELS cap, default 1536px long edge — a
            // different downscale regime than the Windows decoder's, see the
            // caveat on DBWriter.isExcluded.minBBoxMinDimPx). cgImage.width/
            // height convert the normalized bbox to that decoded image's
            // pixels for the absolute-size clustering gate.
            let imgWidthPx = Double(cgImage.width)
            let imgHeightPx = Double(cgImage.height)
            for obs in sortedFaces {
                let r = obs.boundingBox
                pass.faceBBoxes.append(String(format: "%.4f,%.4f,%.4f,%.4f",
                                              r.origin.x, r.origin.y, r.width, r.height))
                pass.faceQualities.append(closestQuality(for: r, in: qualityByBBox))
                pass.faceYaws.append(obs.yaw?.doubleValue)
                pass.facePitches.append(obs.pitch?.doubleValue)
                pass.faceMinDimPx.append(min(r.width * imgWidthPx, r.height * imgHeightPx))
            }
        }
        return pass
    }

    /// Find the quality observation whose bbox center is closest to the
    /// rects-request observation. Quality and detection requests run as
    /// independent detections inside the same perform call; their result
    /// order isn't guaranteed to match. Returns -1 if no quality result
    /// exists (caller treats -1 as "unmeasured", not "low quality").
    private func closestQuality(for box: CGRect, in qualities: [(CGRect, Double)]) -> Double {
        guard !qualities.isEmpty else { return -1 }
        let cx = box.midX; let cy = box.midY
        var bestDist = CGFloat.infinity
        var bestQ: Double = -1
        for (qBox, q) in qualities {
            let dx = qBox.midX - cx; let dy = qBox.midY - cy
            let d = dx * dx + dy * dy
            if d < bestDist {
                bestDist = d
                bestQ = q
            }
        }
        // Demand reasonable proximity (centers within ~10 % of frame).
        // Beyond that, the quality observation is for a different face.
        return bestDist < 0.01 ? bestQ : -1
    }

    /// Fast OCR — `recognitionLevel = .fast`, no language correction. Cheap
    /// enough to run inline on every text-bearing image; it reads character by
    /// character, so it mangles handwriting.
    public func ocrFast(_ cgImage: CGImage) -> String {
        recognizeText(cgImage, level: .fast)
    }

    /// Accurate OCR — line-level model with language correction and automatic
    /// language detection. Reads handwriting the fast tier can't; several times
    /// slower, so the tagging stage runs it only when `TextReading` asks.
    public func ocrAccurate(_ cgImage: CGImage) -> String {
        guard AccurateOCRWarmup.shared.isReady, AccurateOCRBreaker.shared.allowed else { return "" }
        var completed = false
        let text = recognizeText(cgImage, level: .accurate, completed: &completed)
        AccurateOCRBreaker.shared.record(success: completed)
        return text
    }

    private func recognizeText(_ cgImage: CGImage, level: VNRequestTextRecognitionLevel) -> String {
        var completed = false
        return recognizeText(cgImage, level: level, completed: &completed)
    }

    private func recognizeText(_ cgImage: CGImage, level: VNRequestTextRecognitionLevel, completed: inout Bool) -> String {
        let req = VNRecognizeTextRequest()
        req.recognitionLevel = level
        if level == .accurate {
            req.usesLanguageCorrection = true
            req.automaticallyDetectsLanguage = true
        } else {
            req.usesLanguageCorrection = false
            req.recognitionLanguages = ["en-US"]
        }
        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        // Same gate + timeout as runPrimaryPass — OCR also goes through ANE.
        // Gates released inside the worker thread so a timed-out perform holds
        // its slot until it actually finishes.
        let accurate = level == .accurate
        if accurate { accurateOCRGate.wait() }
        visionConcurrencyGate.wait()
        let didReturn = runVisionWithTimeout { [handler] in
            defer {
                visionConcurrencyGate.signal()
                if accurate { accurateOCRGate.signal() }
            }
            do { try handler.perform([req]) } catch { /* swallow */ }
        }
        guard didReturn, let results = req.results else { return "" }
        completed = true
        return results
            .compactMap { $0.topCandidates(1).first?.string }
            .joined(separator: "\n")
    }
}

/// The first accurate read by a newly installed engine binary compiles Vision's
/// model for the Neural Engine: 92 s measured on an M1 Pro (later processes:
/// ~0.4 s cold, ~0.23 s warm). Run that once at engine launch, off the scan path
/// and with no timeout; until it finishes the scan keeps its fast reads.
final class AccurateOCRWarmup: @unchecked Sendable {
    static let shared = AccurateOCRWarmup()
    private let lock = NSLock()
    private var started = false
    private var ready = false

    var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return ready
    }

    func start() {
        lock.lock()
        guard !started else { lock.unlock(); return }
        started = true
        lock.unlock()
        DispatchQueue.global(qos: .utility).async {
            let began = CFAbsoluteTimeGetCurrent()
            // Real text, so the recognizers compile too, not just the detector
            // (a blank image never reaches them). Both tiers: the fast one also
            // compiles on first use.
            if let sample = Self.sampleText() {
                for level in [VNRequestTextRecognitionLevel.accurate, .fast] {
                    let req = VNRecognizeTextRequest()
                    req.recognitionLevel = level
                    req.usesLanguageCorrection = level == .accurate
                    req.automaticallyDetectsLanguage = level == .accurate
                    autoreleasepool {
                        try? VNImageRequestHandler(cgImage: sample, options: [:]).perform([req])
                    }
                }
            }
            self.lock.lock(); self.ready = true; self.lock.unlock()
            JSONLog.shared.info(ev: "ocr_accurate_prewarmed",
                                extra: ["ms": AnyCodable((CFAbsoluteTimeGetCurrent() - began) * 1000)])
        }
    }

    private static func sampleText() -> CGImage? {
        guard let ctx = CGContext(data: nil, width: 480, height: 120, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        ctx.setFillColor(CGColor(gray: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: 480, height: 120))
        let font = CTFontCreateWithName("Helvetica" as CFString, 40, nil)
        let attributes: [NSAttributedString.Key: Any] = [NSAttributedString.Key(kCTFontAttributeName as String): font,
                          NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(gray: 0, alpha: 1)]
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: "Warm up text 2026", attributes: attributes))
        ctx.textPosition = CGPoint(x: 20, y: 44)
        CTLineDraw(line, ctx)
        return ctx.makeImage()
    }
}

/// Pauses the accurate tier for ten minutes after three failures in a row. Seen
/// on macOS 27 while the Neural Engine reported no resources (e5rt error 13):
/// every candidate image would otherwise pay a failed model load or a 10 s
/// timeout. The fast read still stands, and Describe Files can transcribe what
/// it missed. After the pause one more attempt runs; another failure re-trips.
final class AccurateOCRBreaker: @unchecked Sendable {
    static let shared = AccurateOCRBreaker()
    static let failureLimit = 3
    static let pauseSeconds: CFAbsoluteTime = 600
    private let lock = NSLock()
    private var consecutiveFailures = 0
    private var trippedAt: CFAbsoluteTime = 0

    var allowed: Bool {
        lock.lock(); defer { lock.unlock() }
        if consecutiveFailures < Self.failureLimit { return true }
        guard CFAbsoluteTimeGetCurrent() - trippedAt >= Self.pauseSeconds else { return false }
        consecutiveFailures = Self.failureLimit - 1
        return true
    }

    func record(success: Bool) {
        lock.lock()
        consecutiveFailures = success ? 0 : consecutiveFailures + 1
        let tripped = consecutiveFailures == Self.failureLimit
        if tripped { trippedAt = CFAbsoluteTimeGetCurrent() }
        lock.unlock()
        if tripped {
            JSONLog.shared.warn(ev: "ocr_accurate_disabled",
                                error: "accurate text recognition failed \(Self.failureLimit) times in a row")
        }
    }
}

// MARK: - Pool

public actor VisionWorkerPool {
    private var available: [VisionWorker]
    private struct Waiter { let id: Int; let cont: CheckedContinuation<VisionWorker?, Never> }
    private var waiters: [Waiter] = []
    private var nextWaiterID = 0

    public init(count: Int) {
        self.available = (0..<max(1, count)).map { _ in VisionWorker() }
    }

    /// Returns nil when the awaiting task is cancelled while parked. Previously
    /// a cancelled waiter parked forever — `withCheckedContinuation` ignores
    /// cancellation, so the continuation was never resumed and never removed,
    /// growing `waiters` (and stranding the awaiting task) across every
    /// cancelled scan. Now the cancellation handler removes the waiter and
    /// resumes it with nil, so each continuation resumes exactly once and a
    /// cancelled acquirer frees its slot cleanly.
    public func acquire() async -> VisionWorker? {
        if let worker = available.popLast() { return worker }
        let id = nextWaiterID
        nextWaiterID &+= 1
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (cont: CheckedContinuation<VisionWorker?, Never>) in
                if Task.isCancelled {
                    cont.resume(returning: nil)
                } else {
                    waiters.append(Waiter(id: id, cont: cont))
                }
            }
        } onCancel: {
            Task { await self.cancelWaiter(id) }
        }
    }

    private func cancelWaiter(_ id: Int) {
        if let idx = waiters.firstIndex(where: { $0.id == id }) {
            let w = waiters.remove(at: idx)
            w.cont.resume(returning: nil)
        }
    }

    public func release(_ worker: VisionWorker) {
        if !waiters.isEmpty {
            let waiter = waiters.removeFirst()
            waiter.cont.resume(returning: worker)
        } else {
            available.append(worker)
        }
    }

    /// Returns nil if no worker could be acquired because the caller was
    /// cancelled — the body is then skipped and no worker is consumed.
    public func with<T: Sendable>(_ body: @Sendable (VisionWorker) async throws -> T) async rethrows -> T? {
        guard let worker = await acquire() else { return nil }
        do {
            let result = try await body(worker)
            release(worker)
            return result
        } catch {
            release(worker)
            throw error
        }
    }
}
