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
    private static let excludedFoldersKey = "deepAnalyzeExcludedFolders"
    /// Tamper bound for `excludedFolders` — matches the schema's
    /// `deepAnalyzeAll.excludedFolders` maxItems (also mirrored by the
    /// Windows `AppSettings.MaxExcludedFolders`).
    private static let maxExcludedFolders = 256

    var activeKind: AIModelKind {
        didSet { UserDefaults.standard.set(activeKind.rawValue, forKey: key) }
    }

    /// Absolute folder paths to skip during a whole-library Deep Analyze
    /// pass — separate from any scan exclusion: a folder can be fine to
    /// catalog/tag/search but too slow or private to run the VLM over.
    /// Persists as a plain string array in UserDefaults (mirrors `activeKind`'s
    /// persistence pattern); sent fresh with every deepAnalyzeAll and ignored
    /// whenever an explicit file selection (fileIDs) is present.
    var excludedFolders: [String] {
        didSet { UserDefaults.standard.set(excludedFolders, forKey: Self.excludedFoldersKey) }
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
        self.excludedFolders = Self.sanitizeExcludedFolders(
            UserDefaults.standard.stringArray(forKey: Self.excludedFoldersKey) ?? [])
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

    /// Drop blank/relative entries, trim a trailing "/", dedupe, cap the
    /// list. Mirrors the Windows `AppSettings.SanitizeExcludedFolders` gate.
    /// Case-sensitive dedupe/compare — unlike Windows' NTFS-driven
    /// case-insensitive comparison, macOS paths are compared exactly the
    /// same way the engine's SQL exclusion match does (plain `LIKE`, no
    /// `COLLATE NOCASE`, matching `DeepAnalyzeRunner`'s existing
    /// `.folder(prefix:)` scope convention).
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
        excludedFolders = Self.sanitizeExcludedFolders(excludedFolders + [folder])
        return .added
    }

    func removeExcludedFolder(_ path: String) {
        excludedFolders.removeAll { $0 == path }
    }
}
