// VisionWorker wraps the Vision requests (classify, OCR, face rects, quality). One worker per scan task amortizes VNRequest allocation.
// The primary pass runs classify + face rects + quality in one VNImageRequestHandler invocation; OCR runs on demand.
// The pool is an `actor`: a lock-guarded class measured slower.
import Foundation
import Vision
import CoreImage
import CoreText
import ImageIO
import UniformTypeIdentifiers

/// Global concurrency cap for `VNImageRequestHandler.perform`, tied to `Hardware.workerCap` so the Vision/ANE stage scales with the machine.
/// The per-call timeout below is the deadlock safety net.
package let visionConcurrencyGate = DispatchSemaphore(value: Hardware.workerCap)

/// Per-call hard wall-clock timeout for `handler.perform`. Past it the call is abandoned and the worker freed; the orphaned thread runs until Vision unblocks or the engine exits.
package let visionPerformTimeoutSeconds: Double = {
    #if DEBUG
    if let raw = ProcessInfo.processInfo.environment["FILEID_TEST_VISION_TIMEOUT"], let value = Double(raw), value > 0 { return value }
    #endif
    return 10.0
}()

/// At most four accurate text reads in flight: more concurrent reads each reloaded the model and blew the 10 s timeout.
package let accurateOCRGate = DispatchSemaphore(value: 4)

/// Runs `body` on a background queue with a wall-clock timeout. Returns false on timeout; the body keeps running, so callers must not rely on shared state being settled.
/// The Vision request types are non-Sendable; the box is safe because only one thread touches a given call at a time.
package final class _VisionUncheckedBox<T>: @unchecked Sendable {
    package let value: T
    package init(_ v: T) { self.value = v }
}

@inline(__always)
package func runVisionWithTimeout(_ body: @escaping () -> Void) -> Bool {
    let sem = DispatchSemaphore(value: 0)
    let box = _VisionUncheckedBox(body)
    DispatchQueue.global(qos: .userInitiated).async {
        // Drain Vision/CoreImage intermediates here: the global queue's threads never drain a top-level autorelease pool.
        autoreleasepool { box.value() }
        sem.signal()
    }
    return sem.wait(timeout: .now() + visionPerformTimeoutSeconds) == .success
}

/// One reusable bundle of VNRequest objects. The pool guarantees exactly one owning task at a time.
public final class VisionWorker: @unchecked Sendable {

    // VNRequest objects are allocated per call (see runPrimaryPass), never cached on the worker: after a timeout a
    // still-running perform could race the next file's perform on the same instances, crashing or bleeding faces into another file.
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
        /// False iff `handler.perform` timed out or failed. Tagging keys its stage-ran gates on this so a failed pass never wipes persisted tags or faces.
        public var didComplete: Bool
    }

    /// Bundled face/scene Vision pass over `cgImage`. Face feature prints are NOT extracted here: per-face prints inline cause ANE thrash.
    /// `FILEID_VISION_CLASSIFY_DEVICE=gpu` (or `cpu`) moves the classifier off the Neural Engine.
    private static let classifyDevice = ProcessInfo.processInfo.environment["FILEID_VISION_CLASSIFY_DEVICE"]?.lowercased()

    private static func applyClassifyDevice(_ request: VNClassifyImageRequest) {
        guard let wanted = classifyDevice, let stages = try? request.supportedComputeStageDevices else { return }
        for (stage, devices) in stages {
            let match = devices.first { device in
                switch device {
                case .gpu: return wanted == "gpu"
                case .cpu: return wanted == "cpu"
                default: return false
                }
            }
            if let match { request.setComputeDevice(match, for: stage) }
        }
    }

    public func runPrimaryPass(_ cgImage: CGImage) -> PrimaryPass {
        var pass = PrimaryPass(classifyTags: [], faceCount: 0,
                               faceBBoxes: [], faceQualities: [],
                               faceYaws: [], facePitches: [],
                               faceMinDimPx: [],
                               facePrints: [], didComplete: false)

        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        // Per-call request objects, never shared across files.
        let cReq = VNClassifyImageRequest()
        Self.applyClassifyDevice(cReq)
        let fReq = VNDetectFaceRectanglesRequest()
        // Quality + landmark request, run in the same perform call as the face rects.
        let qReq = VNDetectFaceCaptureQualityRequest()
        visionConcurrencyGate.wait()
        // Hard wall-clock timeout so one bad input can't park this worker. The gate is signalled from inside the worker thread, so an orphaned perform keeps its slot until it really finishes.
        let didReturn = runVisionWithTimeout { [handler] in
            defer { visionConcurrencyGate.signal() }
            do { try handler.perform([cReq, fReq, qReq]) } catch { }
        }
        if !didReturn {
            return pass   // timed out — didComplete stays false; file marked for retry downstream
        }
        // A non-timeout error leaves results nil, handled like a timeout so a rescan retries instead of clearing prior tags.
        guard cReq.results != nil else { return pass }
        pass.didComplete = true

        if let results = cReq.results {
            // 0.30 is the recall sweet spot for VNClassifyImageRequest's ~1300 labels: 0.5 left most photos with 0-2 tags.
            // Cap at the top 8; results are sorted by confidence descending.
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
        // Index quality observations by bbox to align them with the face rects (the two requests can return observations in different order).
        let qualityByBBox: [(CGRect, Double)] = (qReq.results ?? []).map { obs in
            (obs.boundingBox, Double(obs.faceCaptureQuality ?? 0))
        }
        if let faces = fReq.results {
            pass.faceCount = faces.count
            // Largest faces first.
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
            // Vision's boundingBox is normalized to the decoded cgImage (long edge capped by FILEID_SCAN_MAX_PIXELS; a different downscale regime than the Windows decoder's, see FACE_MIN_BBOX_MIN_DIM_PX in tagging.rs), so cgImage.width/height give absolute pixels.
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

    /// Finds the quality observation whose bbox center is closest to the rects observation. Returns -1 when none exists: unmeasured, not low quality.
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
        // Demand centers within ~10 % of the frame; beyond that the quality observation is for a different face.
        return bestDist < 0.01 ? bestQ : -1
    }

    /// Fast OCR: `recognitionLevel = .fast`, no language correction. Mangles handwriting.
    public func ocrFast(_ cgImage: CGImage) -> String {
        recognizeText(cgImage, level: .fast)
    }

    /// Accurate OCR: line-level model with language correction and language detection. Several times slower, so only run when `TextReading` asks.
    public func ocrAccurate(_ cgImage: CGImage) -> String {
        ocrAccurateIfAvailable(cgImage) ?? ""
    }

    /// nil when the accurate tier is warming up or paused by its breaker; the read never ran, so the caller records it as owed.
    package func ocrAccurateIfAvailable(_ cgImage: CGImage) -> String? {
        guard AccurateOCRWarmup.shared.isReady, AccurateOCRBreaker.shared.allowed else { return nil }
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
        if level == .accurate { return Self.recognizeAccurately(cgImage, completed: &completed) }
        let req = VNRecognizeTextRequest()
        req.recognitionLevel = level
        req.usesLanguageCorrection = false
        req.recognitionLanguages = ["en-US"]
        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        // Same gate and timeout as runPrimaryPass.
        visionConcurrencyGate.wait()
        let didReturn = runVisionWithTimeout { [handler] in
            defer { visionConcurrencyGate.signal() }
            do { try handler.perform([req]) } catch { /* swallow */ }
        }
        guard didReturn, let results = req.results else { return "" }
        completed = true
        return results
            .compactMap { $0.topCandidates(1).first?.string }
            .joined(separator: "\n")
    }

    /// The accurate tier uses Vision's Swift `RecognizeTextRequest`: the `VNRecognizeTextRequest` path serialises accurate reads in one process (9.5/s vs 25/s at four in parallel).
    /// The fast tier stays on `VNRecognizeTextRequest`.
    package static func recognizeAccurately(_ cgImage: CGImage, completed: inout Bool) -> String {
        accurateOCRGate.wait()
        visionConcurrencyGate.wait()
        let box = ScanSideResult<String?>()
        let didReturn = runVisionWithTimeout {
            defer {
                visionConcurrencyGate.signal()
                accurateOCRGate.signal()
            }
            box.set(performAccurateRead(cgImage))
        }
        guard didReturn, let text = box.get() ?? nil else { return "" }
        completed = true
        return text
    }

    /// Blocks the calling thread (never a Swift concurrency thread) until the read ends.
    package static func performAccurateRead(_ cgImage: CGImage,
                                    languages: CarefulTextRead.Languages = CarefulTextRead.configured) -> String? {
        let box = ScanSideResult<String>()
        let done = DispatchSemaphore(value: 0)
        Task.detached(priority: .userInitiated) {
            if let text = await CarefulTextRead.read(cgImage, languages: languages) { box.set(text) }
            done.signal()
        }
        done.wait()
        return box.get()
    }
}

/// The first accurate read by a new engine binary compiles Vision's model for the Neural Engine (92 s measured on M1 Pro). Run it once at launch, off the scan path, with no timeout.
package final class AccurateOCRWarmup: @unchecked Sendable {
    package static let shared = AccurateOCRWarmup()
    private let lock = NSLock()
    private var started = false
    private var ready = false

    package var isReady: Bool {
        lock.lock(); defer { lock.unlock() }
        return ready
    }

    package static let isEnabled = ProcessInfo.processInfo.environment["FILEID_OCR_WARMUP"] != "0"

    /// The compile shares the Neural Engine with scan model loads, so it waits while a scan is queued or running.
    package func startWhenIdle(after delay: Duration = .seconds(3), busy: @escaping @Sendable () async -> Bool) {
        Task.detached(priority: .utility) {
            try? await Task.sleep(for: delay)
            while await busy() {
                try? await Task.sleep(for: .seconds(1))
            }
            Self.shared.start()
        }
    }

    package func start() {
        lock.lock()
        guard !started else { lock.unlock(); return }
        started = true
        lock.unlock()
        if !Self.isEnabled { return }
        DispatchQueue.global(qos: .utility).async {
            let began = CFAbsoluteTimeGetCurrent()
            // Real text, so the recognizers compile too (a blank image never reaches them).
            if let sample = Self.sampleText() {
                _ = VisionWorker.performAccurateRead(sample)
                let req = VNRecognizeTextRequest()
                req.recognitionLevel = .fast
                req.usesLanguageCorrection = false
                autoreleasepool {
                    try? VNImageRequestHandler(cgImage: sample, options: [:]).perform([req])
                }
            }
            self.lock.lock(); self.ready = true; self.lock.unlock()
            ImagingLog.info("ocr_accurate_prewarmed", metrics: ["ms": (CFAbsoluteTimeGetCurrent() - began) * 1000])
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

/// Pauses the accurate tier for ten minutes after three failures in a row (seen on macOS 27 when the Neural Engine reported no resources). The fast read still stands.
package final class AccurateOCRBreaker: @unchecked Sendable {
    package static let shared = AccurateOCRBreaker()
    package static let failureLimit = 3
    package static let pauseSeconds: CFAbsoluteTime = 600
    private let lock = NSLock()
    private var consecutiveFailures = 0
    private var trippedAt: CFAbsoluteTime = 0
    private var tripHandler: (@Sendable () -> Void)?

    /// Called each time the breaker trips, so the host can tell the user the accurate tier is off.
    package func onTrip(_ handler: @escaping @Sendable () -> Void) {
        lock.lock(); defer { lock.unlock() }
        tripHandler = handler
    }

    package var allowed: Bool {
        lock.lock(); defer { lock.unlock() }
        if consecutiveFailures < Self.failureLimit { return true }
        guard CFAbsoluteTimeGetCurrent() - trippedAt >= Self.pauseSeconds else { return false }
        consecutiveFailures = Self.failureLimit - 1
        return true
    }

    /// Whether the pause is in force now, without ending it the way `allowed` does once the pause is over.
    package var paused: Bool {
        lock.lock(); defer { lock.unlock() }
        return consecutiveFailures >= Self.failureLimit && CFAbsoluteTimeGetCurrent() - trippedAt < Self.pauseSeconds
    }

    package func record(success: Bool) {
        lock.lock()
        consecutiveFailures = success ? 0 : consecutiveFailures + 1
        let tripped = consecutiveFailures == Self.failureLimit
        if tripped { trippedAt = CFAbsoluteTimeGetCurrent() }
        let handler = tripped ? tripHandler : nil
        lock.unlock()
        if tripped {
            handler?()
            ImagingLog.warn("ocr_accurate_disabled",
                            error: "accurate text recognition failed \(Self.failureLimit) times in a row")
        }
    }
}
