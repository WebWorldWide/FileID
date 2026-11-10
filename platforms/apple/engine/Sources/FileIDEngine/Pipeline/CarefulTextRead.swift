// The scan's careful text read: Vision's accurate tier, either with automatic
// language detection on every line or with fixed Latin languages first.
import CoreGraphics
import Foundation
import Vision

enum CarefulTextRead {
    enum Languages: String, Sendable {
        case automatic
        case latinFirst
        case latinOnly
    }

    static let configured = Languages(rawValue: ProcessInfo.processInfo.environment["FILEID_CAREFUL_READ"] ?? "") ?? .automatic

    /// Vision's Latin model also reads Cyrillic, so only scripts such as Chinese,
    /// Japanese, Korean, Arabic, Thai and Devanagari need the automatic pass.
    static let latinLanguages = ["en-US", "fr-FR", "de-DE", "es-ES", "pt-BR", "it-IT"].map(Locale.Language.init(identifier:))

    struct Line: Equatable {
        var text: String
        var confidence: Float
        /// Normalized, origin bottom-left (Vision's convention).
        var box: CGRect
    }

    static func read(_ image: CGImage, languages: Languages) async -> String? {
        switch languages {
        case .automatic:
            return await recognize(image, automatic: true).map(joined)
        case .latinOnly:
            return await recognize(image, automatic: false).map(joined)
        case .latinFirst:
            async let textBoxes = detectText(image)
            guard let latin = await recognize(image, automatic: false) else { return nil }
            let unread = unreadRegions(textBoxes: await textBoxes, latin: latin)
            guard let roi = fallbackRegion(unread),
                  let crop = image.cropping(to: pixelRect(roi, width: image.width, height: image.height)),
                  let found = await recognize(crop, automatic: true)
            else { return joined(latin) }
            return joined(merge(latin: latin, fallback: found.map { mapped($0, from: roi) }))
        }
    }

    private static func recognize(_ image: CGImage, automatic: Bool) async -> [Line]? {
        var request = RecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = true
        request.automaticallyDetectsLanguage = automatic
        if !automatic { request.recognitionLanguages = latinLanguages }
        guard let observations = try? await request.perform(on: image) else { return nil }
        return observations.compactMap { observation in
            observation.topCandidates(1).first.map {
                Line(text: $0.string, confidence: $0.confidence, box: observation.boundingBox.cgRect)
            }
        }
    }

    private static func detectText(_ image: CGImage) async -> [CGRect] {
        let request = DetectTextRectanglesRequest()
        return ((try? await request.perform(on: image)) ?? []).map { $0.boundingBox.cgRect }
    }

    static func joined(_ lines: [Line]) -> String {
        lines.map(\.text).joined(separator: "\n")
    }

    /// The Latin model's confidence is 0.3, 0.5 or 1.0; it scores the Latin-looking
    /// strings it invents over Arabic or Chinese lines at 0.3.
    static let trustedConfidence: Float = 0.5
    /// Text boxes shorter than this share of the image are distant print the
    /// automatic pass doesn't read either.
    static let minimumUnreadHeight: CGFloat = 0.02

    /// Text the detector found that no trusted Latin line covers, plus the Latin
    /// lines too unsure to keep.
    static func unreadRegions(textBoxes: [CGRect], latin: [Line]) -> [CGRect] {
        let trusted = latin.filter { $0.confidence >= trustedConfidence }.map(\.box)
        let missed = textBoxes.filter { $0.height >= minimumUnreadHeight && coverage(of: $0, by: trusted) < 0.3 }
        return missed + latin.filter { $0.confidence < trustedConfidence && $0.box.height >= minimumUnreadHeight }.map(\.box)
    }

    /// One region around everything unread, widened along the line because the
    /// detector often boxes only part of a Chinese or Arabic line.
    static func fallbackRegion(_ regions: [CGRect]) -> CGRect? {
        guard var union = regions.first else { return nil }
        for region in regions.dropFirst() { union = union.union(region) }
        let lineHeight = regions.map(\.height).max() ?? union.height
        let widened = union.insetBy(dx: -max(lineHeight * 4, union.width * 0.5), dy: -lineHeight * 0.5)
        let clipped = widened.intersection(CGRect(x: 0, y: 0, width: 1, height: 1))
        return clipped.isEmpty ? nil : clipped
    }

    static func pixelRect(_ normalized: CGRect, width: Int, height: Int) -> CGRect {
        let w = CGFloat(width), h = CGFloat(height)
        return CGRect(x: normalized.minX * w, y: (1 - normalized.maxY) * h,
                      width: normalized.width * w, height: normalized.height * h).integral
    }

    private static func mapped(_ line: Line, from roi: CGRect) -> Line {
        var out = line
        out.box = CGRect(x: roi.minX + line.box.minX * roi.width, y: roi.minY + line.box.minY * roi.height,
                         width: line.box.width * roi.width, height: line.box.height * roi.height)
        return out
    }

    /// Latin lines stay as read; a fallback line is added where no trusted Latin
    /// line already is, and replaces the unsure Latin lines it overlaps.
    static func merge(latin: [Line], fallback: [Line]) -> [Line] {
        let trusted = latin.filter { $0.confidence >= trustedConfidence }.map(\.box)
        let added = fallback.filter {
            !$0.text.trimmingCharacters(in: .whitespaces).isEmpty && coverage(of: $0.box, by: trusted) < 0.5
        }
        let kept = latin.filter { line in
            line.confidence >= trustedConfidence || !added.contains { coverage(of: line.box, by: [$0.box]) >= 0.5 }
        }
        return kept + added
    }

    /// Share of `box` inside the union of `others`, sampled on a 10 × 4 grid.
    static func coverage(of box: CGRect, by others: [CGRect]) -> Double {
        guard box.width > 0, box.height > 0, !others.isEmpty else { return 0 }
        var inside = 0
        for column in 0..<10 {
            for row in 0..<4 {
                let point = CGPoint(x: box.minX + box.width * (CGFloat(column) + 0.5) / 10,
                                    y: box.minY + box.height * (CGFloat(row) + 0.5) / 4)
                if others.contains(where: { $0.contains(point) }) { inside += 1 }
            }
        }
        return Double(inside) / 40
    }
}
