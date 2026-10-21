import Foundation

/// Combines RAM++ tags with the Apple Vision classifier the primary pass
/// already ran. Measured on 100 hand-checked photos from the owner's library
/// (2026-10-21): RAM++ called baseball dugouts "canoe" and cornhole boards
/// "skateboard"/"frisbee" at 0.89–0.98, while Vision, which has the same
/// labels, gave them under 0.01. Vetoing only those labels took tag precision
/// from 0.929 to 0.948 with no correct tag lost. A broad veto over every shared
/// label was rejected: it also dropped correct "park", "tree" and "baseball".
enum TagAgreement {
    static let vetoable: Set<String> = ["canoe", "skateboard", "frisbee"]
    static let vetoBelow: Float = 0.01

    static func combine(ram: [(tag: String, score: Float)], vision: [String: Float],
                        faceCount: Int) -> [(tag: String, score: Float)] {
        guard !ram.isEmpty else { return ram }
        let visionUsable = !vision.isEmpty
        var out: [(tag: String, score: Float)] = []
        for hit in ram {
            let key = hit.tag.lowercased().replacingOccurrences(of: " ", with: "_")
            let seen = vision[key] ?? 0
            if visionUsable, vetoable.contains(hit.tag.lowercased()), seen < vetoBelow { continue }
            if faceCount == 0, RamPlusService.peopleWords.contains(hit.tag.lowercased()) { continue }
            // Two independent models agreeing: noisy-OR of the two confidences.
            let score = seen >= 0.30 ? 1 - (1 - hit.score) * (1 - seen) : hit.score
            out.append((hit.tag, score))
        }
        return out.sorted { $0.score > $1.score }
    }
}
