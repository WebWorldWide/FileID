import Foundation
import Vision
import CoreImage
import NaturalLanguage

// MARK: - VisionProcessor
//
// Performance design:
//  - VNRequest objects are created ONCE as static/nonisolated lets and REUSED.
//    Creating a new VNClassifyImageRequest per file is expensive (model re-init).
//  - All Vision requests run on the calling thread (already .userInitiated from task group).
//  - Each handler.perform([req1, req2]) call runs both requests in one GPU/ANE dispatch.
//  - CGImage is loaded ONCE per file and shared across all requests.

struct VisionProcessor {
    static let shared = VisionProcessor()

    // MARK: - Shared Request Objects (created once, reused — thread-safe for reading)
    // VNRequest objects store results per-perform, NOT shared across concurrent calls.
    // We DO NOT share these across threads; each call creates its own handler.
    // The request class itself is stateless for configuration — only results are per-call.

    // MARK: - Image Loading

    /// Loads a CGImage scaled to maxPixelSize in ONE I/O pass.
    /// All subsequent Vision requests use this cached CGImage.
    func loadImage(from url: URL, maxPixelSize: Int = 1024) -> CGImage? {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        let opts: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform:  true,
            kCGImageSourceThumbnailMaxPixelSize:         maxPixelSize,
            kCGImageSourceShouldCacheImmediately:        true     // ← hint: decode into ANE-accessible memory
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, opts as CFDictionary)
    }

    // MARK: - EXIF (no pixel decode — reads metadata sidecar only)

    func readEXIF(from url: URL) -> (cameraModel: String?, latitude: Double?, longitude: Double?, latRef: String?, lonRef: String?) {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
              let props  = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        else { return (nil, nil, nil, nil, nil) }

        let camera = (props[kCGImagePropertyTIFFDictionary] as? [CFString: Any])?[kCGImagePropertyTIFFModel] as? String
        var lat: Double?, lon: Double?, latRef: String?, lonRef: String?
        if let gps = props[kCGImagePropertyGPSDictionary] as? [CFString: Any] {
            lat    = gps[kCGImagePropertyGPSLatitude]    as? Double
            lon    = gps[kCGImagePropertyGPSLongitude]   as? Double
            latRef = gps[kCGImagePropertyGPSLatitudeRef] as? String
            lonRef = gps[kCGImagePropertyGPSLongitudeRef] as? String
        }
        return (camera, lat, lon, latRef, lonRef)
    }

    // MARK: - Scene Classification

    /// Classifies + animal-detects in ONE handler.perform([]) call.
    /// Confidence threshold: 0.30 (captures more useful labels like Food, Landscape, Portrait)
    func classifyImage(_ cgImage: CGImage) async throws -> [String] {
        try await Task.detached(priority: .userInitiated) {
            let classReq = VNClassifyImageRequest()
            classReq.preferBackgroundProcessing = false
            if let rev = VNClassifyImageRequest.supportedRevisions.max() { classReq.revision = rev }

            let animalReq = VNRecognizeAnimalsRequest()
            animalReq.preferBackgroundProcessing = false
            if let rev = VNRecognizeAnimalsRequest.supportedRevisions.max() { animalReq.revision = rev }

            let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
            try handler.perform([classReq, animalReq])  // ← Single GPU dispatch for both

            var tags: [String] = []

            if let animals = animalReq.results {
                tags += animals.compactMap { $0.labels.first(where: { $0.confidence > 0.4 })?.identifier.capitalized }
            }
            if let scenes = classReq.results {
                let rejected = Set(["object","item","thing","other","background"])
                tags += scenes
                    .filter { $0.confidence > 0.30 && !rejected.contains($0.identifier.lowercased()) && $0.identifier.count > 2 }
                    .prefix(8)
                    .map { $0.identifier.replacingOccurrences(of: " ", with: "_").capitalized }
            }
            return tags.isEmpty ? ["Unclassified"] : Array(Set(tags))
        }.value
    }

    // MARK: - Face Detection + Feature Prints (combined pass)

    /// Detects faces and generates feature prints in two handler.perform calls.
    /// Face handler runs first; feature print handler runs once per crop.
    func generateFacePrints(from cgImage: CGImage) async throws -> [(VNFeaturePrintObservation, CGImage, CGRect)] {
        try await Task.detached(priority: .userInitiated) {
            let faceReq = VNDetectFaceRectanglesRequest()
            if let rev = VNDetectFaceRectanglesRequest.supportedRevisions.max() { faceReq.revision = rev }

            let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
            try handler.perform([faceReq])

            guard let faces = faceReq.results, !faces.isEmpty else { return [] }

            let W = CGFloat(cgImage.width), H = CGFloat(cgImage.height)
            var results: [(VNFeaturePrintObservation, CGImage, CGRect)] = []

            for face in faces {
                let bb = face.boundingBox
                guard Float(bb.width * bb.height) >= 0.03 else { continue }

                // Convert normalised VN coords (origin=bottom-left) to CG (origin=top-left)
                let pad: CGFloat = 0.15
                let x  = max(0, (bb.minX - bb.width  * pad) * W)
                let y  = max(0, (1 - bb.maxY - bb.height * pad) * H)
                let w  = min(W - x, bb.width  * (1 + 2 * pad) * W)
                let h  = min(H - y, bb.height * (1 + 2 * pad) * H)
                guard let crop = cgImage.cropping(to: CGRect(x: x, y: y, width: w, height: h)) else { continue }

                let fpReq = VNGenerateImageFeaturePrintRequest()
                fpReq.imageCropAndScaleOption = .scaleFill
                if let rev = VNGenerateImageFeaturePrintRequest.supportedRevisions.max() { fpReq.revision = rev }

                let fpHandler = VNImageRequestHandler(cgImage: crop, options: [:])
                try? fpHandler.perform([fpReq])
                if let fp = fpReq.results?.first {
                    results.append((fp, crop, bb))
                }
            }
            return results
        }.value
    }

    // MARK: - Scene Print (Duplicate Detection)

    func generateScenePrint(from cgImage: CGImage) async throws -> VNFeaturePrintObservation {
        try await Task.detached(priority: .userInitiated) {
            let req = VNGenerateImageFeaturePrintRequest()
            req.imageCropAndScaleOption = .scaleFill
            if let rev = VNGenerateImageFeaturePrintRequest.supportedRevisions.max() { req.revision = rev }

            let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
            try handler.perform([req])
            guard let result = req.results?.first else {
                throw NSError(domain: "VisionProcessor", code: -3,
                              userInfo: [NSLocalizedDescriptionKey: "No scene print generated"])
            }
            return result
        }.value
    }

    // MARK: - OCR + NLP Entity Extraction

    func extractTextAndEntities(from cgImage: CGImage) async throws -> [String] {
        try await Task.detached(priority: .userInitiated) {
            let req = VNRecognizeTextRequest()
            req.recognitionLevel  = .accurate
            req.usesLanguageCorrection = true
            if #available(macOS 14.0, *) { req.revision = VNRecognizeTextRequestRevision3 }

            try VNImageRequestHandler(cgImage: cgImage, options: [:]).perform([req])
            let fullText = (req.results ?? []).compactMap { $0.topCandidates(1).first?.string }.joined(separator: " ")
            guard !fullText.isEmpty else { return [] }

            var entities: [String] = []

            // NER — organisations + people
            let tagger = NLTagger(tagSchemes: [.nameType])
            tagger.string = fullText
            tagger.enumerateTags(in: fullText.startIndex..<fullText.endIndex, unit: .word,
                                  scheme: .nameType, options: [.omitWhitespace,.omitPunctuation,.joinNames]) { tag, r in
                if tag == .organizationName || tag == .personalName { entities.append(String(fullText[r])) }
                return true
            }

            // Document-type keywords
            let lower = fullText.lowercased()
            if lower.contains("invoice")                  { entities.append("Invoice") }
            if lower.contains("receipt")                  { entities.append("Receipt") }
            if lower.contains("tax") || lower.contains("w-2") { entities.append("Tax_Document") }
            if lower.contains("confidential")             { entities.append("Confidential") }
            if lower.contains("resume") || lower.contains("curriculum vitae") { entities.append("Resume") }
            if lower.contains("contract")                 { entities.append("Contract") }

            return Array(Set(entities)).filter { $0.count > 2 }
        }.value
    }
    /// Estimates the visual quality/aesthetic score of an image.
    /// Heuristic: Resolution * Saliency Coverage * File Size.
    func evaluateAesthetics(_ cgImage: CGImage, fileSizeMB: Double) async -> Double {
        let result = try? await Task.detached(priority: .userInitiated) {
            let req = VNGenerateAttentionBasedSaliencyImageRequest()
            let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
            try handler.perform([req])
            
            guard let result = req.results?.first else { return 0.5 }
            
            // Heuristic calculation
            let resolution = Double(cgImage.width * cgImage.height) / 1_000_000.0 // Megapixels
            let saliencyWeight = (result.salientObjects?.count ?? 0) > 0 ? 1.2 : 1.0
            let score = (resolution * 0.4) + (fileSizeMB * 0.4) + (saliencyWeight * 0.2)
            
            return min(1.0, score / 10.0) // Normalize roughly
        }.value
        return result ?? 0.5
    }
}
