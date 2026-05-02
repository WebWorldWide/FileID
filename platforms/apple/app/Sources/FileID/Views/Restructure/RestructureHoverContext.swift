import SwiftUI

/// One outcome class for a Restructure proposal. Drives icons,
/// tints, and recommendation card copy.
enum RestructureOutcome: String, CaseIterable, Identifiable {
    case keep
    case tidy
    case reorganize

    var id: String { rawValue }

    var icon: String {
        switch self {
        case .keep:       return "lock.fill"
        case .tidy:       return "tray.and.arrow.up.fill"
        case .reorganize: return "arrow.triangle.branch"
        }
    }

    var tint: Color {
        switch self {
        case .keep:       return .green
        case .tidy:       return .orange
        case .reorganize: return Theme.gold
        }
    }
}

/// What the user is currently hovering — drives cross-highlight
/// across the Sankey, recommendation rows, tree, and staysPut
/// disclosure. Owned by `RestructureView` via `RestructureHoverBus`.
enum RestructureHoverContext: Equatable, Hashable {
    case sourceFolder(String)
    case destBucket(String)
    case outcome(RestructureOutcome)
    case flow(sourceFolder: String, destBucket: String)
}

@MainActor
@Observable
final class RestructureHoverBus {
    var context: RestructureHoverContext?

    init(context: RestructureHoverContext? = nil) {
        self.context = context
    }

    /// No-op when the value is unchanged — without this, mouse-move
    /// events fire the @Observable hot path many times per frame.
    func set(_ new: RestructureHoverContext?) {
        if context != new { context = new }
    }

    func touchesSource(_ folder: String) -> Bool {
        switch context {
        case .sourceFolder(let f):       return f == folder
        case .flow(let f, _):            return f == folder
        case .destBucket, .outcome, .none: return false
        }
    }

    func touchesDest(_ bucket: String) -> Bool {
        switch context {
        case .destBucket(let b):         return b == bucket
        case .flow(_, let b):            return b == bucket
        case .sourceFolder, .outcome, .none: return false
        }
    }

    func touchesOutcome(_ outcome: RestructureOutcome) -> Bool {
        if case .outcome(let o) = context { return o == outcome }
        return false
    }
}
