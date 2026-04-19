import Foundation
import Vision
import CoreImage
import NaturalLanguage

struct VisionProcessor {
    static let shared = VisionProcessor()
    
    func processImage(at url: URL) async throws -> ([String], [PersonIdentity]) {
        let thumbOpts = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 512 // Massively Pre-Scale ML Buffer directly
        ] as CFDictionary
        
        guard let imageSource = CGImageSourceCreateWithURL(url as CFURL, nil),
              let cgImage = CGImageSourceCreateThumbnailAtIndex(imageSource, 0, thumbOpts) else {
            return (["Error_Loading_Image"], [])
        }
        
        return try await processImage(cgImage: cgImage)
    }
    
    /// Process a direct CGImage and return an array of AI tags and clustered identities
    func processImage(cgImage: CGImage) async throws -> ([String], [PersonIdentity]) {
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
                
                // Instantiate requests per-thread to prevent data races and ensure max parallel saturation
                let classificationRequest = VNClassifyImageRequest()
                classificationRequest.preferBackgroundProcessing = false // MAX OUT CPU/GPU
                if let maxRev = VNClassifyImageRequest.supportedRevisions.max() { classificationRequest.revision = maxRev }
                
                let animalRequest = VNRecognizeAnimalsRequest()
                animalRequest.preferBackgroundProcessing = false
                if let maxRev = VNRecognizeAnimalsRequest.supportedRevisions.max() { animalRequest.revision = maxRev }
                
                // We want to run all three requests in parallel over the same image
                do {
                    try handler.perform([animalRequest, classificationRequest])
                    
                    var tags: [String] = []
                    var localIdentities: [PersonIdentity] = []
                    
                    // 1. Process Faces (Removed double-processing; handled efficiently in generateFacePrints now)
                
                // Animal & Scene Classification...
                if let animals = animalRequest.results {
                    for animal in animals {
                        if let topLabel = animal.labels.first(where: { $0.confidence > 0.5 }) {
                            tags.append(topLabel.identifier.capitalized) // e.g., "Dog", "Cat"
                        }
                    }
                }
                
                // 3. Process Scenes & Objects (Unlimited and Global!)
                if let scenes = classificationRequest.results {
                    // Extract all objects and scenes with a lowered confidence threshold
                    let topScenes = scenes
                        .filter { $0.confidence > 0.70 && !$0.identifier.contains("outdoor") }
                        .map { $0.identifier.replacingOccurrences(of: " ", with: "_").capitalized }
                    
                    tags.append(contentsOf: topScenes)
                }
                
                // Due to synchronous VNImageRequestHandler in continuation, we will capture identical faces.
                // But feature clustering via FaceClusteringService is async. We will let MediaProcessor handle the clustering
                // by returning the raw crops and feature prints.
                
                let uniqueTags = Array(Set(tags))
                continuation.resume(returning: (uniqueTags.isEmpty ? ["Unclassified"] : uniqueTags, localIdentities))
                
            } catch {
                continuation.resume(throwing: error)
            }
            }
        }
    }
    
    // Extracted method to generate feature prints without VNImageRequestHandler concurrency issues
    func generateFacePrints(from cgImage: CGImage) async throws -> [(VNFeaturePrintObservation, CGImage)] {
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let faceRequest = VNDetectFaceRectanglesRequest()
                faceRequest.preferBackgroundProcessing = false
                if let maxRev = VNDetectFaceRectanglesRequest.supportedRevisions.max() { faceRequest.revision = maxRev }
                
                let featurePrintRequest = VNGenerateImageFeaturePrintRequest()
                featurePrintRequest.imageCropAndScaleOption = .scaleFill
                if let maxRev = VNGenerateImageFeaturePrintRequest.supportedRevisions.max() { featurePrintRequest.revision = maxRev }
                
                let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
            do {
                try handler.perform([faceRequest])
                var results: [(VNFeaturePrintObservation, CGImage)] = []
                
                if let faces = faceRequest.results {
                    for face in faces {
                        let width = face.boundingBox.width * CGFloat(cgImage.width)
                        let height = face.boundingBox.height * CGFloat(cgImage.height)
                        let x = face.boundingBox.origin.x * CGFloat(cgImage.width)
                        let y = (1 - face.boundingBox.origin.y - face.boundingBox.height) * CGFloat(cgImage.height)
                        
                        let cropRect = CGRect(x: x, y: y, width: width, height: height)
                        if let faceCrop = cgImage.cropping(to: cropRect) {
                            let printHandler = VNImageRequestHandler(cgImage: faceCrop, options: [:])
                            try? printHandler.perform([featurePrintRequest])
                            if let print = featurePrintRequest.results?.first {
                                results.append((print, faceCrop))
                            }
                        }
                    }
                }
                continuation.resume(returning: results)
            } catch {
                continuation.resume(throwing: error)
            }
            }
        }
    }
    
    // Generates a feature print for the entire scene (used for duplicate detection)
    func generateScenePrint(from image: CGImage) async throws -> VNFeaturePrintObservation {
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let request = VNGenerateImageFeaturePrintRequest { request, error in
                    if let error = error {
                        continuation.resume(throwing: error)
                        return
                    }
                    if let print = request.results?.first as? VNFeaturePrintObservation {
                        continuation.resume(returning: print)
                    } else {
                        continuation.resume(throwing: NSError(domain: "VisionProcessor", code: -3, userInfo: [NSLocalizedDescriptionKey: "No scene print"]))
                    }
                }
                request.imageCropAndScaleOption = .scaleFill
                if let maxRev = VNGenerateImageFeaturePrintRequest.supportedRevisions.max() { request.revision = maxRev }
                // No preferBackgroundProcessing = force full power
                
                let handler = VNImageRequestHandler(cgImage: image, options: [:])
                do {
                    try handler.perform([request])
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
    
    // MARK: - Librarian AI (OCR & NLP)
    
    func extractTextAndEntities(from cgImage: CGImage) async throws -> [String] {
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let request = VNRecognizeTextRequest()
                request.recognitionLevel = .accurate
                
                // Force latest hardware routing
                if #available(macOS 14.0, *) {
                    request.revision = VNRecognizeTextRequestRevision3
                }
                
                let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
                
                do {
                    try handler.perform([request])
                    guard let results = request.results else {
                        continuation.resume(returning: [])
                        return
                    }
                    
                    let fullText = results.compactMap { $0.topCandidates(1).first?.string }.joined(separator: " ")
                    
                    var foundEntities: [String] = []
                    
                    // NLP Named Entity Recognition for Orgs & Names
                    let tagger = NLTagger(tagSchemes: [.nameType])
                    tagger.string = fullText
                    let options: NLTagger.Options = [.omitWhitespace, .omitPunctuation, .joinNames]
                    
                    tagger.enumerateTags(in: fullText.startIndex..<fullText.endIndex, unit: .word, scheme: .nameType, options: options) { tag, tokenRange in
                        if let tag = tag, (tag == .organizationName || tag == .personalName) {
                            foundEntities.append(String(fullText[tokenRange]))
                        }
                        return true
                    }
                    
                    // Simple Regex Extractors
                    if let dateRange = fullText.range(of: "\\d{1,2}[-/]\\d{1,2}[-/]\\d{2,4}", options: .regularExpression) {
                        foundEntities.append(String(fullText[dateRange]).replacingOccurrences(of: "/", with: "-"))
                    }
                    
                    let lower = fullText.lowercased()
                    if lower.contains("invoice") { foundEntities.append("Invoice") }
                    if lower.contains("receipt") { foundEntities.append("Receipt") }
                    if lower.contains("tax") || lower.contains("w-2") { foundEntities.append("Tax_Document") }
                    if lower.contains("confidential") { foundEntities.append("Confidential") }
                    
                    // Remove generic noise
                    let filtered = Array(Set(foundEntities)).filter { $0.count > 2 }
                    
                    continuation.resume(returning: filtered)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
}
