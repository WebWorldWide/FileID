// Centralized @AppStorage keys + defaults.
import Foundation

enum AppSettings {
    /// After Cleanup trashes selected duplicates, optionally tag the
    /// keepers with a "duplicate-resolved" Finder tag so they show up
    /// in a Smart Folder.
    static let cleanupAutoTagKey = "cleanup.autoTagKeepers"
    static let cleanupAutoTagDefault: Bool = true
    static let cleanupAutoTagName = "duplicate-resolved"

    static let detailedScanTagsKey = "scan.detailedRamPlusTags"
    static let detailedScanTagsDefault: Bool = false

    /// Folder-granularity for the Restructure butler's clustering. The engine reads
    /// `FILEID_RESTRUCTURE_GRANULARITY` ∈ {loose, normal, tight} (one knob that shifts
    /// the cluster cosines — HDBSCAN `min_cluster_size` philosophy); `EngineClient`
    /// passes the saved value through at spawn, so it applies on the next engine start.
    /// "normal"/unset is the calibrated default (delta 0), so only a non-default value
    /// is passed to the engine. Lockstep with the Windows app's setting.
    static let restructureGranularityKey = "restructure.granularity"
    static let restructureGranularityDefault = "normal"
    static let restructureGranularityValues = ["loose", "normal", "tight"]
}
