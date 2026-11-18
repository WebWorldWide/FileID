import Foundation

/// What the Scan, Rescan and title-bar controls do while FileID's helper can't run.
enum HelperGate {
    static let stoppedPillLabel = "Helper stopped"

    static func isDown(_ state: EngineClient.ConnectionState) -> Bool {
        if case .crashed = state { return true }
        return false
    }

    static func scanDisabled(startRequested: Bool, helperDown: Bool) -> Bool {
        startRequested || helperDown
    }

    static func rescanDisabled(startRequested: Bool, captionsRunning: Bool, faceClusteringInFlight: Bool, helperDown: Bool) -> Bool {
        startRequested || captionsRunning || faceClusteringInFlight || helperDown
    }

    /// The title-bar pill's label when the helper is down; nil means the usual activity labels apply.
    static func pillLabel(for state: EngineClient.ConnectionState) -> String? {
        isDown(state) ? stoppedPillLabel : nil
    }
}

/// The listed rows can run ahead of the counters mid-scan; this says when to ask for the counters again.
enum LibraryCounters {
    static func needsRefresh(isSimilaritySeed: Bool, kind: String?, query: String, listedRows: Int, scopedTotalFiles: Int) -> Bool {
        !isSimilaritySeed && kind == nil && query.isEmpty && listedRows > scopedTotalFiles
    }
}
