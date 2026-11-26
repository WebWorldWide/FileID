import Foundation
import FileIDShared

/// Persists in UserDefaults under `deepAnalyzeActiveModel`, which the engine also reads at spawn. A valid saved choice is never silently replaced.
@Observable
final class DeepAnalyzeSettings: @unchecked Sendable {
    static let shared = DeepAnalyzeSettings()
    private let key = "deepAnalyzeActiveModel"
    private static let excludedFoldersKey = "deepAnalyzeExcludedFolders"
    /// Matches the schema's `deepAnalyzeAll.excludedFolders` maxItems, which the engine's scope check enforces ("can skip at most 100 folders").
    static let maxExcludedFolders = 100

    var activeKind: AIModelKind {
        didSet { if !followingAdvice { UserDefaults.standard.set(activeKind.rawValue, forKey: key) } }
    }

    @ObservationIgnored private var followingAdvice = false

    /// With no saved choice and nothing downloaded, the active model follows the engine's pick for the memory free now, without saving it, so every screen shows the model Welcome offers. Returns whether it changed.
    @discardableResult
    func followAdvice(_ advice: VLMAdvice?, freeDiskBytes: Int64?) -> Bool {
        guard let kind = Self.adoptedKind(
            current: activeKind, persistedRawValue: UserDefaults.standard.string(forKey: key),
            advice: advice, ramGB: systemRAMGB, freeDiskBytes: freeDiskBytes) else { return false }
        followingAdvice = true
        activeKind = kind
        followingAdvice = false
        return true
    }

    static func adoptedKind(
        current: AIModelKind, persistedRawValue: String?, advice: VLMAdvice?, ramGB: Double, freeDiskBytes: Int64?,
        isInstalled: (AIModelKind) -> Bool = ModelInstallStatus.isInstalled(kind:)
    ) -> AIModelKind? {
        guard advice != nil, persistedRawValue?.isEmpty != false, !isInstalled(current) else { return nil }
        let recommended = AIModelKind.recommended(ramGB: ramGB, advice: advice, freeDiskBytes: freeDiskBytes)
        return recommended == current ? nil : recommended
    }

    /// Absolute folder paths skipped by a whole-library Deep Analyze pass. Stored as a plain string array; sent with every deepAnalyzeAll and ignored when an explicit file selection is present.
    var excludedFolders: [String] {
        didSet { UserDefaults.standard.set(excludedFolders, forKey: Self.excludedFoldersKey) }
    }

    let systemRAMGB: Double

    private init() {
        let ram = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824
        self.systemRAMGB = ram
        self.activeKind = Self.startingModel(
            persistedRawValue: UserDefaults.standard.string(forKey: "deepAnalyzeActiveModel"), ramGB: ram, freeDiskBytes: Self.freeDiskBytes())
        self.excludedFolders = Self.sanitizeExcludedFolders(
            UserDefaults.standard.stringArray(forKey: Self.excludedFoldersKey) ?? [])
    }

    static func freeDiskBytes() -> Int64? {
        try? FileManager.default.homeDirectoryForCurrentUser
            .resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]).volumeAvailableCapacityForImportantUsage
    }

    /// The saved choice, else the default. Settings and the Welcome sheet both start from this, so they agree.
    static func startingModel(
        persistedRawValue: String?, ramGB: Double, freeDiskBytes: Int64?,
        isInstalled: (AIModelKind) -> Bool = ModelInstallStatus.isInstalled(kind:)
    ) -> AIModelKind {
        if let persistedRawValue, !persistedRawValue.isEmpty { return AIModelKind.migrated(rawValue: persistedRawValue) }
        return preferredDefault(ramGB: ramGB, freeDiskBytes: freeDiskBytes, isInstalled: isInstalled)
    }

    /// First downloaded recommendation, else the safest fits-this-Mac pick.
    static func preferredDefault(
        ramGB: Double, freeDiskBytes: Int64?, isInstalled: (AIModelKind) -> Bool = ModelInstallStatus.isInstalled(kind:)
    ) -> AIModelKind {
        for kind in AIModelKind.recommendedFor(ramGB: ramGB) where kind.fits(ramGB: ramGB) && isInstalled(kind) {
            return kind
        }
        return AIModelKind.safeDefaultFor(ramGB: ramGB, freeDiskBytes: freeDiskBytes)
    }

    /// Drop blank/relative entries, trim a trailing "/", dedupe, cap the
    /// list. Mirrors the Windows `AppSettings.SanitizeExcludedFolders` gate.
    /// The engine canonicalizes each folder and skips files under it by path prefix.
    static func sanitizeExcludedFolders(_ raw: [String]) -> [String] {
        var seen = Set<String>()
        var result: [String] = []
        for entry in raw {
            if result.count >= maxExcludedFolders { break }
            let trimmed = entry.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed.hasPrefix("/") else { continue }
            var folder = trimmed
            while folder.count > 1, folder.hasSuffix("/") {
                folder.removeLast()
            }
            guard !folder.isEmpty, seen.insert(folder).inserted else { continue }
            result.append(folder)
        }
        return result
    }

    enum AddExcludedFolderResult {
        case added
        case alreadyExcluded
        case listFull
        case invalid
    }

    /// Add a folder to `excludedFolders` via `sanitizeExcludedFolders`,
    /// reporting which of the three outcomes the Settings UI needs to show
    /// distinct feedback for (mirrors the Windows Settings card's
    /// already-excluded / invalid / success handling).
    @discardableResult
    func addExcludedFolder(_ path: String) -> AddExcludedFolderResult {
        let trimmed = path.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix("/") else { return .invalid }
        var folder = trimmed
        while folder.count > 1, folder.hasSuffix("/") {
            folder.removeLast()
        }
        guard !folder.isEmpty else { return .invalid }
        guard !excludedFolders.contains(folder) else { return .alreadyExcluded }
        guard excludedFolders.count < Self.maxExcludedFolders else { return .listFull }
        excludedFolders = Self.sanitizeExcludedFolders(excludedFolders + [folder])
        return .added
    }

    func removeExcludedFolder(_ path: String) {
        excludedFolders.removeAll { $0 == path }
    }
}

extension AIModelKind {
    /// The engine's pick for this Mac and the memory free right now, else the mirrored rule on the installed memory alone.
    static func recommended(ramGB: Double, advice: VLMAdvice?, freeDiskBytes: Int64?) -> AIModelKind {
        if let id = advice?.recommendedID, let kind = AIModelKind(rawValue: id), kind.fits(freeDiskBytes: freeDiskBytes) {
            return kind
        }
        return safeDefaultFor(ramGB: ramGB, availableMB: advice.map { UInt64(max(0, $0.availableMB)) }, freeDiskBytes: freeDiskBytes)
    }
}
