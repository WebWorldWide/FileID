import Foundation
import FileIDShared

/// User-facing Deep Analyze model selection. Persists in UserDefaults
/// under `deepAnalyzeActiveModel`; the engine reads the same key when it
/// spawns. A valid persisted choice remains authoritative. The model picker
/// and engine gate unsafe runs instead of silently replacing the user's pick.
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
        if let persisted = UserDefaults.standard.string(forKey: "deepAnalyzeActiveModel"),
           !persisted.isEmpty {
            self.activeKind = AIModelKind.migrated(rawValue: persisted)
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
