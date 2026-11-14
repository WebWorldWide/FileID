// Centralized @AppStorage keys + defaults.
import Foundation

enum AppSettings {
    /// After Cleanup trashes selected duplicates, optionally tag the
    /// keepers with a "duplicate-resolved" Finder tag so they show up
    /// in a Smart Folder.
    static let cleanupAutoTagKey = "cleanup.autoTagKeepers"
    static let cleanupAutoTagDefault: Bool = true
    static let cleanupAutoTagName = "duplicate-resolved"

    /// Folder-granularity for the Restructure butler's clustering. The engine reads
    /// `FILEID_RESTRUCTURE_GRANULARITY` ∈ {loose, normal, tight} (one knob that shifts
    /// the cluster cosines — HDBSCAN `min_cluster_size` philosophy); `EngineClient`
    /// passes the saved value through at spawn, so it applies on the next engine start.
    /// "normal"/unset is the calibrated default (delta 0), so only a non-default value
    /// is passed to the engine. Lockstep with the Windows app's setting.
    static let restructureGranularityKey = "restructure.granularity"
    static let restructureGranularityDefault = "normal"
    static let restructureGranularityValues = ["loose", "normal", "tight"]

    /// Scan setting: careful text reads use Vision's Latin languages instead of automatic
    /// language detection (faster; Chinese, Japanese, Korean, Arabic and other non-Latin
    /// scripts are not read). `EngineClient` passes it as `FILEID_CAREFUL_READ` at spawn.
    static let fastTextReadingKey = "scan.fastTextReading"
    static let fastTextReadingDefault = false
    static let fastTextReadingEngineValue = "latin"

    /// Spotlight gets file names, tags and descriptions after a scan, so ⌘Space
    /// finds them. Off removes what FileID added.
    static let spotlightKey = "privacy.spotlight"
    static let spotlightDefault = true
}
