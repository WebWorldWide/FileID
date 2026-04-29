import Foundation
import SwiftData

enum JunkScorer {

    struct Weights {
        var tag:       Double = 0.25
        var size:      Double = 0.15
        var age:       Double = 0.10
        var duplicate: Double = 0.30
        var path:      Double = 0.10
        var ext:       Double = 0.05
        var aesthetic: Double = 0.15
        var zeroBytes: Double = 0.50
    }

    static let defaultWeights = Weights()
    static let junkThreshold: Double = 0.45

    static func score(_ file: FileRecord, weights: Weights = defaultWeights) -> (score: Double, reasons: [String]) {
        var reasons: [String] = []
        var score: Double = 0

        let junkTags: Set<String> = ["Screenshot","Cache","Temp","Thumbnail"]
        let tagHits = file.aiTags.filter { junkTags.contains($0) }
        if !tagHits.isEmpty {
            let tagScore = min(Double(tagHits.count) / 3.0, 1.0)
            score += weights.tag * tagScore
            reasons.append("Tagged \(tagHits.joined(separator: ", "))")
        }

        if file.fileSizeMB < 0.05 {
            score += weights.size * 0.9
            reasons.append("Very small file (\(String(format: "%.0f", file.fileSizeMB * 1024)) KB)")
        } else if file.fileSizeMB < 0.5 {
            score += weights.size * 0.4
        }

        let ageYears = Date().timeIntervalSince(file.creationDate) / (365.25 * 24 * 3600)
        if ageYears > 3 {
            score += weights.age * min((ageYears - 3) / 5, 1.0)
            reasons.append("Not modified in \(Int(ageYears)) years")
        }

        if file.duplicateGroupUUID != nil {
            score += weights.duplicate
            reasons.append("Duplicate of another file")
        }

        let path = file.url.path.lowercased()
        let junkDirs = ["/library/caches/", "/tmp/", "/var/folders/", "/.trash/", "/temp/", "/.thumbnails/"]
        for dir in junkDirs where path.contains(dir) {
            score += weights.path
            reasons.append("Located in system cache/temp folder")
            break
        }

        let junkExts: Set<String> = ["tmp","log","bak","cache","ds_store","localized"]
        if junkExts.contains(file.url.pathExtension.lowercased()) {
            score += weights.ext
            reasons.append("Ephemeral file type (.\(file.url.pathExtension))")
        }

        if file.aestheticScore > 0 && file.aestheticScore < 0.25 {
            score += weights.aesthetic
            reasons.append(String(format: "Low aesthetic score (%.2f)", file.aestheticScore))
        } else if file.aestheticScore > 0 && file.aestheticScore < 0.4 {
            score += weights.aesthetic * 0.5
        }

        if file.fileSizeMB == 0 {
            score += weights.zeroBytes
            reasons.append("Empty / unreadable file")
        }

        // Faces soften but don't veto — a blurry crowd screenshot is still junk.
        if file.hasFaces { score *= 0.65 }

        let clamped = min(score, 1.0)
        return (clamped, clamped >= junkThreshold ? reasons : [])
    }

    static func scoreAll(
        store: FileIDDataStore,
        onProgress: (@Sendable (Int, Int) -> Void)? = nil
    ) async {
        await store.scoreJunkAll(
            pageSize: 500,
            scorer: { file in score(file) },
            onProgress: onProgress
        )
    }
}
