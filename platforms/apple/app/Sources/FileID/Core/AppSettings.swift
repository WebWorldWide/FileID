// Centralized @AppStorage keys + defaults.
import Foundation

enum AppSettings {
    /// After Cleanup trashes selected duplicates, optionally tag the
    /// keepers with a "duplicate-resolved" Finder tag so they show up
    /// in a Smart Folder.
    static let cleanupAutoTagKey = "cleanup.autoTagKeepers"
    static let cleanupAutoTagDefault: Bool = true
    static let cleanupAutoTagName = "duplicate-resolved"
}
