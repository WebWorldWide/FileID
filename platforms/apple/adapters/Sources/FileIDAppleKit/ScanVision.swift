// Stage 5: Vision for the scan. The primary pass and the OCR tiers run the Swift
// engine's VisionWorker and TextReading (ScanShared/), and the text read and
// the scanned-PDF read follow Tagging.processImage / processPDF step for step,
// so the Rust host stores the same tags, faces and text as the Swift engine.
import CoreGraphics
import Foundation

enum ScanVision {
    static let docHints: Set<String> = ["document", "text", "screenshot", "receipt", "presentation", "menu", "sign"]
    private static let maxPDFRenderPixels: CGFloat = 50_000_000

    /// `DBWriter.bboxArea` over the stored "x,y,w,h" text.
    static func bboxArea(_ s: String) -> Double {
        let parts = s.split(separator: ",").compactMap { Double($0) }
        guard parts.count == 4 else { return 0 }
        return parts[2] * parts[3]
    }

    static func primaryPass(_ image: CGImage) -> [String: Any] {
        let pass = autoreleasepool { VisionWorker().runPrimaryPass(image) }
        var faces: [[String: Any]] = []
        for (i, bbox) in pass.faceBBoxes.enumerated() {
            let quality: Double? = i < pass.faceQualities.count ? (pass.faceQualities[i] >= 0 ? pass.faceQualities[i] : nil) : nil
            let yaw = i < pass.faceYaws.count ? pass.faceYaws[i] : nil
            let pitch = i < pass.facePitches.count ? pass.facePitches[i] : nil
            let minDim = i < pass.faceMinDimPx.count ? pass.faceMinDimPx[i] : Double.greatestFiniteMagnitude
            let excluded = FaceQualityGate.holdReason(quality: quality, yaw: yaw, pitch: pitch,
                                                      bboxArea: bboxArea(bbox), minDimPx: minDim) != nil
            var face: [String: Any] = ["bbox": bbox, "excluded": excluded]
            face["quality"] = quality
            faces.append(face)
        }
        return [
            "complete": pass.didComplete,
            "classifyTags": pass.classifyTags,
            "classifyConfidences": pass.classifyConfidences.mapValues { Double($0) },
            "faceCount": pass.faceCount,
            "faces": faces,
        ]
    }

    /// The text read of `Tagging.processImage`: a fast read when the classifier,
    /// the tags or the page shape suggest text, then the accurate tier when
    /// `TextReading` says the fast read missed or mangled it.
    static func textRead(_ image: CGImage, classifyTags: [String], tags: [String], hasCamera: Bool) -> [String: Any] {
        let worker = VisionWorker()
        let documentShape = TextReading.isDocumentShape(width: image.width, height: image.height, hasCamera: hasCamera)
        guard classifyTags.contains(where: { docHints.contains($0.lowercased()) })
            || TextReading.isTextBearing(tags) || documentShape else {
            return ["ran": false, "handwritten": false, "fastMs": 0.0, "carefulMs": 0.0]
        }
        let started = CFAbsoluteTimeGetCurrent()
        let fast = autoreleasepool { worker.ocrFast(image) }
        let fastMs = (CFAbsoluteTimeGetCurrent() - started) * 1000
        var text = fast
        var carefulMs = 0.0
        var handwritten = false
        if TextReading.needsCarefulRead(fastText: fast, tags: tags, textBearing: documentShape) {
            let carefulStart = CFAbsoluteTimeGetCurrent()
            let careful = autoreleasepool { worker.ocrAccurate(image) }
            carefulMs = (CFAbsoluteTimeGetCurrent() - carefulStart) * 1000
            handwritten = TextReading.isHandwritten(fastText: fast, carefulText: careful, tags: tags,
                                                    paperLike: TextReading.isPaperLike(tags: tags, hasCamera: hasCamera))
            text = TextReading.preferred(fast: fast, careful: careful)
        }
        var result: [String: Any] = ["ran": true, "handwritten": handwritten && !text.isEmpty,
                                     "fastMs": fastMs, "carefulMs": carefulMs]
        if !text.isEmpty { result["text"] = text }
        return result
    }

    /// The image-only PDF branch of `Tagging.processPDF`: up to three pages
    /// rendered at 2x (clamped to 50 MP) and read with both tiers.
    static func scannedPDF(path: String) -> [String: Any] {
        guard let pdf = CGPDFDocument(URL(fileURLWithPath: path) as CFURL) else { return ["opened": false] }
        let worker = VisionWorker()
        let pageCount = min(pdf.numberOfPages, 3)
        var fullText: [String] = []
        var carefulMs = 0.0
        var handwritten = false
        for pageNum in 1..<(pageCount + 1) {
            autoreleasepool {
                guard let page = pdf.page(at: pageNum) else { return }
                let bounds = page.getBoxRect(.mediaBox)
                guard bounds.width > 0, bounds.height > 0 else { return }
                var scale: CGFloat = 2.0
                let scaledPixels = bounds.width * bounds.height * scale * scale
                if scaledPixels > maxPDFRenderPixels { scale *= sqrt(maxPDFRenderPixels / scaledPixels) }
                let w = Int(bounds.width * scale)
                let h = Int(bounds.height * scale)
                guard w > 0, h > 0,
                      let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                                          space: CGColorSpaceCreateDeviceRGB(),
                                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return }
                ctx.setFillColor(CGColor(gray: 1, alpha: 1))
                ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
                ctx.scaleBy(x: scale, y: scale)
                ctx.drawPDFPage(page)
                guard let cg = ctx.makeImage() else { return }
                let fast = worker.ocrFast(cg)
                let carefulStart = CFAbsoluteTimeGetCurrent()
                let careful = worker.ocrAccurate(cg)
                carefulMs += (CFAbsoluteTimeGetCurrent() - carefulStart) * 1000
                if TextReading.isHandwritten(fastText: fast, carefulText: careful, tags: [], paperLike: true) {
                    handwritten = true
                }
                let text = TextReading.preferred(fast: fast, careful: careful)
                if !text.isEmpty { fullText.append(text) }
            }
        }
        let ocr = fullText.joined(separator: "\n\n")
        var result: [String: Any] = ["opened": true, "handwritten": handwritten && !ocr.isEmpty, "carefulMs": carefulMs]
        if !ocr.isEmpty { result["text"] = ocr }
        return result
    }
}

private func scanStrings(_ value: Any?) -> [String] {
    (value as? [Any])?.compactMap { $0 as? String } ?? []
}

@_cdecl("fileid_apple_vision_primary")
public func fileid_apple_vision_primary(_ image: UInt64) -> UnsafeMutablePointer<CChar>? {
    guard let cgImage = ScanImages.shared.image(image) else { return nil }
    return scanJSON(ScanVision.primaryPass(cgImage))
}

@_cdecl("fileid_apple_text_read")
public func fileid_apple_text_read(_ image: UInt64, _ request: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    guard let cgImage = ScanImages.shared.image(image),
          let data = scanString(request)?.data(using: .utf8),
          let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return nil }
    return scanJSON(ScanVision.textRead(cgImage, classifyTags: scanStrings(object["classifyTags"]),
                                        tags: scanStrings(object["tags"]),
                                        hasCamera: object["hasCamera"] as? Bool ?? false))
}

@_cdecl("fileid_apple_pdf_ocr")
public func fileid_apple_pdf_ocr(_ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    guard let path = scanString(path) else { return nil }
    return scanJSON(autoreleasepool { ScanVision.scannedPDF(path: path) })
}

@_cdecl("fileid_apple_ocr_warmup")
public func fileid_apple_ocr_warmup() {
    AccurateOCRWarmup.shared.start()
}

@_cdecl("fileid_apple_ocr_ready")
public func fileid_apple_ocr_ready() -> Bool {
    AccurateOCRWarmup.shared.isReady
}
