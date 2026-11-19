import Foundation
import FileIDShared

/// The description model the Welcome sheet and Settings both recommend: the engine's pick for the memory free right now, with the reason when it is not the best model this Mac could run.
struct VLMRecommendation: Equatable {
    let kind: AIModelKind
    let bestForThisMac: AIModelKind
    let freeMemoryGB: Double?

    init(ramGB: Double, advice: VLMAdvice?, freeDiskBytes: Int64?) {
        kind = AIModelKind.recommended(ramGB: ramGB, advice: advice, freeDiskBytes: freeDiskBytes)
        bestForThisMac = AIModelKind.safeDefaultFor(ramGB: ramGB, freeDiskBytes: freeDiskBytes)
        freeMemoryGB = advice.map { Double(max(0, $0.availableMB)) / 1024 }
    }

    var reason: String? {
        guard kind != bestForThisMac, let freeMemoryGB else { return nil }
        let free = freeMemoryGB.formatted(.number.precision(.fractionLength(1)))
        return "\(bestForThisMac.shortName) fits this Mac; \(kind.shortName) is recommended right now because only \(free) GB is free."
    }
}

/// Which description model a finished run used, spoken when the engine loaded a smaller one than the chosen model.
enum DescribeRunSummary {
    static func modelNote(ran: String, chosen: AIModelKind) -> String {
        let ranName = AIModelKind(rawValue: ran)?.displayName ?? ran
        guard ran != chosen.rawValue else { return "with \(ranName)" }
        return "with \(ranName), because not enough memory was free for \(chosen.displayName)"
    }

    static func toast(processed: Int, ran: String, chosen: AIModelKind) -> String {
        let files = "\(processed.formatted()) file\(processed == 1 ? "" : "s")"
        guard ran != chosen.rawValue else { return "Descriptions ready for \(files)" }
        return "Descriptions ready for \(files), written \(modelNote(ran: ran, chosen: chosen))"
    }
}

extension AIModelKind {
    var shortName: String {
        displayName.replacingOccurrences(of: #"\s*\([^)]*bit\)$"#, with: "", options: .regularExpression)
    }
}
