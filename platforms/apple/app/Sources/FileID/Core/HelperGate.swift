import Foundation
import SwiftUI

/// What the Scan, Rescan and title-bar controls do while FileID's helper can't run.
enum HelperGate {
    static let stoppedPillLabel = "Helper stopped"

    static func isDown(_ state: EngineClient.ConnectionState) -> Bool {
        if case .crashed = state { return true }
        return false
    }

    /// The helper cannot run: it failed to start or stopped.
    @MainActor
    static func isDown(_ engine: EngineClient) -> Bool {
        engine.startFailure != nil || isDown(engine.state)
    }

    /// Model downloads are the helper's job, so they stay off while it is down.
    static let downloadsUnavailableNote = "FileID's helper isn't running, so models can't download yet. Restart the helper first."

    static func downloadDisabled(inProgress: Bool, helperDown: Bool) -> Bool {
        inProgress || helperDown
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

private struct HelperDownKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    /// True while FileID's helper can't run; cards read it to turn off what needs the helper.
    var helperDown: Bool {
        get { self[HelperDownKey.self] }
        set { self[HelperDownKey.self] = newValue }
    }
}
