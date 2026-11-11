// The scan's careful text read: Vision's accurate tier with automatic language
// detection (default), or with fixed Latin languages when the user turns on
// Settings ▸ Scanning ▸ Faster text reading (`FILEID_CAREFUL_READ=latin`).
import CoreGraphics
import Foundation
import Vision

package enum CarefulTextRead {
    package enum Languages: String, Sendable {
        case automatic
        case latin
    }

    package static let configured = languages(from: ProcessInfo.processInfo.environment["FILEID_CAREFUL_READ"])

    package static func languages(from value: String?) -> Languages {
        value.flatMap(Languages.init(rawValue:)) ?? .automatic
    }

    /// Vision's Latin model also reads Cyrillic; Chinese, Japanese, Korean, Arabic,
    /// Thai and Devanagari need automatic detection.
    package static let latinLanguages = ["en-US", "fr-FR", "de-DE", "es-ES", "pt-BR", "it-IT"].map(Locale.Language.init(identifier:))

    package static func read(_ image: CGImage, languages: Languages) async -> String? {
        var request = RecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = true
        request.automaticallyDetectsLanguage = languages == .automatic
        if languages == .latin { request.recognitionLanguages = latinLanguages }
        guard let observations = try? await request.perform(on: image) else { return nil }
        return observations.compactMap { $0.topCandidates(1).first?.string }.joined(separator: "\n")
    }
}
