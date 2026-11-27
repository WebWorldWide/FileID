import Foundation

enum AppSettings {
    /// After Cleanup trashes duplicates, optionally tag the keepers "duplicate-resolved" so a Smart Folder can find them.
    static let cleanupAutoTagKey = "cleanup.autoTagKeepers"
    static let cleanupAutoTagDefault: Bool = true
    static let cleanupAutoTagName = "duplicate-resolved"

    /// Restructure folder granularity (loose / normal / tight). Passed to the engine as FILEID_RESTRUCTURE_GRANULARITY only when it is not the calibrated default "normal"; applies on the next engine start.
    static let restructureGranularityKey = "restructure.granularity"
    static let restructureGranularityDefault = "normal"
    static let restructureGranularityValues = ["loose", "normal", "tight"]

    /// Careful text reads use Vision's Latin languages only: faster, but non-Latin scripts are not read. Passed to the engine as FILEID_CAREFUL_READ at spawn.
    static let fastTextReadingKey = "scan.fastTextReading"
    static let fastTextReadingDefault = false
    static let fastTextReadingEngineValue = "latin"

    /// Spotlight gets file names, tags and descriptions after a scan, so ⌘Space
    /// finds them. Off removes what FileID added.
    static let spotlightKey = "privacy.spotlight"
    static let spotlightDefault = true
}
