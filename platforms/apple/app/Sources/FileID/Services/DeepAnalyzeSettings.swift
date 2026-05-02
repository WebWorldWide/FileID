import Foundation
import FileIDShared

/// User-facing Deep Analyze model selection. Persists in UserDefaults
/// under `deepAnalyzeActiveModel`; the engine reads the same key when
/// it spawns. Demotes a persisted choice that no longer fits the
/// host's RAM tier, and prefers a model that's actually downloaded
/// over one that would require a fresh fetch.
@Observable
final class DeepAnalyzeSettings: @unchecked Sendable {
    static let shared = DeepAnalyzeSettings()
    private let key = "deepAnalyzeActiveModel"

    var activeKind: AIModelKind {
        didSet { UserDefaults.standard.set(activeKind.rawValue, forKey: key) }
    }

    let systemRAMGB: Double

    private init() {
        let ram = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824
        self.systemRAMGB = ram
        let persisted: AIModelKind? = UserDefaults.standard.string(forKey: "deepAnalyzeActiveModel")
            .flatMap { AIModelKind(rawValue: $0) }
        if let p = persisted, p.fits(ramGB: ram) {
            if ModelInstallStatus.isInstalled(kind: p) {
                self.activeKind = p
            } else {
                let downloaded = AIModelKind.recommendedFor(ramGB: ram)
                    .first(where: { $0.fits(ramGB: ram) && ModelInstallStatus.isInstalled(kind: $0) })
                self.activeKind = downloaded ?? p
            }
        } else {
            self.activeKind = Self.preferredDefault(ramGB: ram)
        }
    }

    /// First downloaded recommendation, else the safest fits-this-Mac pick.
    static func preferredDefault(ramGB: Double) -> AIModelKind {
        for kind in AIModelKind.recommendedFor(ramGB: ramGB) where kind.fits(ramGB: ramGB) {
            if ModelInstallStatus.isInstalled(kind: kind) {
                return kind
            }
        }
        return AIModelKind.safeDefaultFor(ramGB: ramGB)
    }
}
