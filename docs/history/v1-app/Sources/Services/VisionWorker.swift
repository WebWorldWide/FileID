import Foundation
import Vision
import NaturalLanguage

// MARK: - Cached Vision request revisions

// `VNRequest.supportedRevisions.max()` hits disk — cache once at process start.
private enum VisionRevisions {
    static let classify              = VNClassifyImageRequest.supportedRevisions.max() ?? VNClassifyImageRequestRevision1
    static let animals               = VNRecognizeAnimalsRequest.supportedRevisions.max() ?? VNRecognizeAnimalsRequestRevision1
    static let faceRect              = VNDetectFaceRectanglesRequest.supportedRevisions.max() ?? VNDetectFaceRectanglesRequestRevision2
    static let featurePrint          = VNGenerateImageFeaturePrintRequest.supportedRevisions.max() ?? VNGenerateImageFeaturePrintRequestRevision1
}

// MARK: - VisionPass

// One image -> one VNImageRequestHandler -> bundled requests. Construction of
// the handler decodes the image and allocates GPU textures, so doing it once
// (instead of once per request kind, plus once per detected face) is the
// single biggest scan-throughput win.
//
// Not Sendable: contains Vision observations (CoreFoundation-bridged classes
// that aren't declared Sendable). Kept local to the visionQueue dispatch
// closure that built it; only `Data`/`String` derived from it crosses actor
// boundaries via the continuation in MediaProcessor.
struct VisionPass {
    let classifications: [(label: String, confidence: Float)]
    let animals:         [(label: String, confidence: Float)]
    let faceObservations:[VNFaceObservation]
    let facePrints:      [(VNFeaturePrintObservation, CGRect)]
}

extension VisionPass {
    // Same shape as the legacy `VisionWorker.classify` return: scenes + animals,
    // capitalized, deduped. Used by Session A to keep MediaProcessor's tag
    // path unchanged while the underlying handler usage gets fixed. Session B
    // will replace this with `TagTaxonomy.collapse` and emit typed tags.
    func legacyTagStrings() -> [String] {
        var tags = animals.map(\.label)
        tags += classifications.map(\.label)
        return Array(Set(tags))
    }
}

// MARK: - VisionWorker

// Reusable VNRequest objects. `@unchecked Sendable` because `VisionWorkerPool`
// guarantees exactly one owning task at a time — VNRequest mutates `.results`
// per perform() and isn't safe to share across concurrent calls.
final class VisionWorker: @unchecked Sendable {

    // MARK: - Reusable requests

    private let classifyReq:    VNClassifyImageRequest
    private let animalReq:      VNRecognizeAnimalsRequest
    private let faceRectReq:    VNDetectFaceRectanglesRequest
    private let ocrReq:         VNRecognizeTextRequest
    private let ocrFastReq:     VNRecognizeTextRequest

    // Snapshotted at scan start; rebuild the pool to pick up Settings changes.
    private let classificationMinConfidence: Float

    init() {
        classifyReq = VNClassifyImageRequest()
        classifyReq.preferBackgroundProcessing = false
        classifyReq.revision = VisionRevisions.classify

        animalReq = VNRecognizeAnimalsRequest()
        animalReq.preferBackgroundProcessing = false
        animalReq.revision = VisionRevisions.animals

        faceRectReq = VNDetectFaceRectanglesRequest()
        faceRectReq.revision = VisionRevisions.faceRect

        ocrReq = VNRecognizeTextRequest()
        ocrReq.recognitionLevel = .accurate
        ocrReq.usesLanguageCorrection = true
        if #available(macOS 14.0, *) { ocrReq.revision = VNRecognizeTextRequestRevision3 }

        // ~200 ms/page vs ~3 s for the accurate path. Right choice for PDF
        // tag extraction — we want "invoice"/"receipt"/"taxes" keywords, not
        // verbatim reproduction.
        ocrFastReq = VNRecognizeTextRequest()
        ocrFastReq.recognitionLevel = .fast
        ocrFastReq.usesLanguageCorrection = false
        if #available(macOS 14.0, *) { ocrFastReq.revision = VNRecognizeTextRequestRevision3 }

        // 0.50 default (was 0.30) — below ~0.5 Vision classifications tend to
        // be near-random ("object"/"indoor"/etc), dragging tag quality down.
        let stored = UserDefaults.standard.double(forKey: "classificationConfidence")
        classificationMinConfidence = Float(stored > 0 ? stored : 0.50)
    }

    // MARK: - Primary pass (bundled)

    // Two perform() calls on a SINGLE VNImageRequestHandler:
    //   1) classify + animals + face rectangles
    //   2) per-face VNGenerateImageFeaturePrintRequest with regionOfInterest
    // Replaces 3+N independent handlers (classify, faceRect, N face crops)
    // with one. Per-face handlers were the dominant per-file cost on
    // multi-face photos.
    func runPrimaryPass(_ cgImage: CGImage) -> VisionPass {
        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        try? handler.perform([classifyReq, animalReq, faceRectReq])

        let minConf = classificationMinConfidence
        let rejected: Set<String> = ["object","item","thing","other","background"]

        var scenes: [(String, Float)] = []
        if let results = classifyReq.results {
            scenes = results
                .filter { $0.confidence > minConf
                       && !rejected.contains($0.identifier.lowercased())
                       && $0.identifier.count > 2 }
                .prefix(8)
                .map { ($0.identifier.replacingOccurrences(of: " ", with: "_").capitalized,
                        $0.confidence) }
        }

        var animals: [(String, Float)] = []
        if let results = animalReq.results {
            for obs in results {
                if let lbl = obs.labels.first(where: { $0.confidence > minConf }) {
                    animals.append((lbl.identifier.capitalized, lbl.confidence))
                }
            }
        }

        let faces = faceRectReq.results ?? []
        let facePrints = extractFacePrints(handler: handler, faces: faces)

        return VisionPass(
            classifications:  scenes,
            animals:          animals,
            faceObservations: faces,
            facePrints:       facePrints
        )
    }

    // ROI-per-face on the same handler — no per-face VNImageRequestHandler
    // allocation, no per-face CGImage cropping. The padded ROI matches the
    // 15% padding the legacy crop path used so feature prints remain
    // comparable in shape (FaceClusteringService.l2 returns .infinity on
    // dimension mismatch as a safety net).
    private func extractFacePrints(
        handler: VNImageRequestHandler,
        faces: [VNFaceObservation],
        minAreaFraction: Float = 0.03
    ) -> [(VNFeaturePrintObservation, CGRect)] {
        guard !faces.isEmpty else { return [] }
        let pad: CGFloat = 0.15

        var requests: [VNGenerateImageFeaturePrintRequest] = []
        var boxes:    [CGRect] = []
        for face in faces {
            let bb = face.boundingBox
            guard Float(bb.width * bb.height) >= minAreaFraction else { continue }
            let x = max(0, bb.minX - bb.width  * pad)
            let y = max(0, bb.minY - bb.height * pad)
            let w = min(1 - x, bb.width  * (1 + 2 * pad))
            let h = min(1 - y, bb.height * (1 + 2 * pad))
            let req = VNGenerateImageFeaturePrintRequest()
            req.imageCropAndScaleOption = .scaleFill
            req.revision = VisionRevisions.featurePrint
            req.regionOfInterest = CGRect(x: x, y: y, width: w, height: h)
            requests.append(req)
            boxes.append(bb)
        }
        guard !requests.isEmpty else { return [] }
        try? handler.perform(requests)

        var out: [(VNFeaturePrintObservation, CGRect)] = []
        out.reserveCapacity(requests.count)
        for (i, req) in requests.enumerated() {
            if let fp = req.results?.first {
                out.append((fp, boxes[i]))
            }
        }
        return out
    }

    // MARK: - Lightweight classify (video / minimal callers)

    // Video frames don't need face detection. Kept as a thin wrapper around
    // a single handler with classify+animals only. No "Unclassified" fallback —
    // callers handle empty arrays.
    func classify(_ cgImage: CGImage) -> [String] {
        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        try? handler.perform([classifyReq, animalReq])

        let minConf = classificationMinConfidence
        var tags: [String] = []
        if let animals = animalReq.results {
            tags += animals.compactMap {
                $0.labels.first(where: { $0.confidence > minConf })?.identifier.capitalized
            }
        }
        if let scenes = classifyReq.results {
            let rejected: Set<String> = ["object","item","thing","other","background"]
            tags += scenes
                .filter { $0.confidence > minConf
                       && !rejected.contains($0.identifier.lowercased())
                       && $0.identifier.count > 2 }
                .prefix(8)
                .map { $0.identifier.replacingOccurrences(of: " ", with: "_").capitalized }
        }
        return Array(Set(tags))
    }

    // MARK: - OCR

    func ocrText(_ cgImage: CGImage) -> String {
        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        try? handler.perform([ocrReq])
        return (ocrReq.results ?? []).compactMap { $0.topCandidates(1).first?.string }.joined(separator: " ")
    }

    func ocrFast(_ cgImage: CGImage) -> String {
        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        try? handler.perform([ocrFastReq])
        return (ocrFastReq.results ?? []).compactMap { $0.topCandidates(1).first?.string }.joined(separator: " ")
    }
}

// MARK: - TextTagger

enum TextTagger {

    static func tagsFromText(_ fullText: String) -> [String] {
        guard !fullText.isEmpty else { return [] }
        var entities: [String] = []

        let tagger = NLTagger(tagSchemes: [.nameType])
        tagger.string = fullText
        tagger.enumerateTags(
            in: fullText.startIndex..<fullText.endIndex,
            unit: .word,
            scheme: .nameType,
            options: [.omitWhitespace, .omitPunctuation, .joinNames]
        ) { tag, r in
            if tag == .organizationName || tag == .personalName {
                entities.append(String(fullText[r]))
            }
            return true
        }

        let lower = fullText.lowercased()
        if lower.contains("invoice")                                  { entities.append("Invoice") }
        if lower.contains("receipt")                                  { entities.append("Receipt") }
        if lower.contains("tax") || lower.contains("w-2")             { entities.append("Tax_Document") }
        if lower.contains("confidential")                             { entities.append("Confidential") }
        if lower.contains("resume") || lower.contains("curriculum vitae") { entities.append("Resume") }
        if lower.contains("contract")                                 { entities.append("Contract") }
        if lower.contains("agenda") || lower.contains("slide ")       { entities.append("Presentation") }
        if lower.contains("budget") || lower.contains("forecast")     { entities.append("Spreadsheet") }

        return Array(Set(entities)).filter { $0.count > 2 }
    }
}

// MARK: - VisionWorkerPool

actor VisionWorkerPool {
    private var available: [VisionWorker]
    private var waiters: [CheckedContinuation<VisionWorker, Never>] = []

    init(count: Int) {
        self.available = (0..<max(1, count)).map { _ in VisionWorker() }
    }

    func acquire() async -> VisionWorker {
        if let worker = available.popLast() { return worker }
        return await withCheckedContinuation { waiters.append($0) }
    }

    func release(_ worker: VisionWorker) {
        if let waiter = waiters.first {
            waiters.removeFirst()
            waiter.resume(returning: worker)
        } else {
            available.append(worker)
        }
    }

    func with<T: Sendable>(_ body: @Sendable (VisionWorker) async throws -> T) async rethrows -> T {
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

