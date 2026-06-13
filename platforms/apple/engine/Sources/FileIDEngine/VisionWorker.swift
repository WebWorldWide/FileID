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
import ImageIO
import UniformTypeIdentifiers

/// Global concurrency cap for `VNImageRequestHandler.perform`. Set to the
/// full worker count: throughput-first, with the per-call timeout below as
/// the safety net for deadlock. (Earlier runs at gate=6 halved throughput
/// from 150 → 75 files/s; the watchdog already handles the stuck-call case
/// that the gate was previously protecting against.)
let visionConcurrencyGate = DispatchSemaphore(value: 14)

/// Per-call hard wall-clock timeout for any `handler.perform` invocation.
/// If Vision hasn't returned by `visionPerformTimeoutSeconds`, we abandon
/// the call and free the worker. The orphaned background thread keeps
/// running until Vision's internal queue eventually unblocks (or the
/// engine exits). Bounded leak, unbounded responsiveness.
let visionPerformTimeoutSeconds: Double = 10.0

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
        box.value()
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
        public var faceCount: Int
        public var faceBBoxes: [String]        // "x,y,w,h" normalized
        public var faceQualities: [Double]     // 0..1, parallel to faceBBoxes; -1 if not measured
        public var faceYaws: [Double?]         // radians, parallel to faceBBoxes; nil if missing
        public var facePitches: [Double?]      // radians, parallel to faceBBoxes; nil if missing
        public var facePrints: [Data]          // EMPTY here — extracted lazily in Stage D
        /// False iff `handler.perform` exceeded the wall-clock timeout and was
        /// abandoned (empty result). The tagging stage keys its stage-ran gates
        /// (tagsEvaluated / facesEvaluated) on this so a timed-out pass never
        /// wipes previously-persisted auto-tags or faces (incl. manual
        /// person_id) on a rescan, and marks the file for retry. (F-C3-001/036)
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
            do { try handler.perform([cReq, fReq, qReq]) } catch { /* swallow */ }
        }
        if !didReturn {
            return pass   // timed out — didComplete stays false; file marked for retry downstream
        }
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
            for obs in sortedFaces {
                let r = obs.boundingBox
                pass.faceBBoxes.append(String(format: "%.4f,%.4f,%.4f,%.4f",
                                              r.origin.x, r.origin.y, r.width, r.height))
                pass.faceQualities.append(closestQuality(for: r, in: qualityByBBox))
                pass.faceYaws.append(obs.yaw?.doubleValue)
                pass.facePitches.append(obs.pitch?.doubleValue)
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

    /// Fast OCR — `recognitionLevel = .fast`, no language correction.
    /// ~200 ms/page on M1 vs ~3 s/page for `.accurate`. Used for inline
    /// per-image OCR (documents, screenshots, signs). Whiteboard photos
    /// where accuracy matters get re-OCR'd lazily in M3 if user asks.
    public func ocrFast(_ cgImage: CGImage) -> String {
        let req = VNRecognizeTextRequest()
        req.recognitionLevel = .fast
        req.usesLanguageCorrection = false
        req.recognitionLanguages = ["en-US"]
        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        // Same gate + timeout as runPrimaryPass — OCR also goes through ANE.
        // Gate released inside the worker thread so a timed-out perform holds
        // its slot until it actually finishes.
        visionConcurrencyGate.wait()
        let didReturn = runVisionWithTimeout { [handler] in
            defer { visionConcurrencyGate.signal() }
            do { try handler.perform([req]) } catch { /* swallow */ }
        }
        guard didReturn, let results = req.results else { return "" }
        return results
            .compactMap { $0.topCandidates(1).first?.string }
            .joined(separator: "\n")
    }
}

// MARK: - Pool

public actor VisionWorkerPool {
    private var available: [VisionWorker]
    private var waiters: [CheckedContinuation<VisionWorker, Never>] = []

    public init(count: Int) {
        self.available = (0..<max(1, count)).map { _ in VisionWorker() }
    }

    public func acquire() async -> VisionWorker {
        if let worker = available.popLast() { return worker }
        return await withCheckedContinuation { waiters.append($0) }
    }

    public func release(_ worker: VisionWorker) {
        if !waiters.isEmpty {
            let waiter = waiters.removeFirst()
            waiter.resume(returning: worker)
        } else {
            available.append(worker)
        }
    }

    public func with<T: Sendable>(_ body: @Sendable (VisionWorker) async throws -> T) async rethrows -> T {
        let worker = await acquire()
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
