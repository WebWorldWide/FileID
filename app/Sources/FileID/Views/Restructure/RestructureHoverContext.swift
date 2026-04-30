// V8 — shared hover bus for the Restructure tab.
//
// Hover state used to live inside SankeyFlowView as private @State,
// which meant hovering a ribbon brightened the ribbon but didn't
// reach the recommendation cards or the staysPut disclosure. Lifting
// it into an @Observable bus shared by Sankey ↔ cards ↔ tree ↔
// staysPut makes hover a first-class control surface: hover any
// folder anywhere, and every connected ribbon, card, and row lights
// up gold.
//
// Writes are coalesced (set only when the value differs) so a mouse
// move across a row doesn't churn the entire view tree at 60Hz.
import SwiftUI

/// What the user is currently hovering. The four cases cover every
/// surface that participates in cross-highlighting:
///   - sourceFolder: a left-column folder in Sankey/Tree/staysPut
///   - destBucket:   a right-column bucket in Sankey/Tree
///   - outcome:      a recommendation card
///   - flow:         a ribbon (carries both endpoints)
enum RestructureHoverContext: Equatable, Hashable {
    case sourceFolder(String)
    case destBucket(String)
    case outcome(RestructureOutcome)
    case flow(sourceFolder: String, destBucket: String)
}

/// Single source of truth for the Restructure tab's hover state.
/// Owned by `RestructureView`, passed by reference to every
/// participating subview so they all read + write the same context.
@MainActor
@Observable
final class RestructureHoverBus {
    var context: RestructureHoverContext?

    init(context: RestructureHoverContext? = nil) {
        self.context = context
    }

    /// Coalesced setter — no-op when the value is unchanged. Without
    /// this, an `.onHover { ... }` that fires on every mouse-move event
    /// would re-publish the @Observable on every frame, forcing the
    /// entire Sankey to re-render at 60Hz.
    func set(_ new: RestructureHoverContext?) {
        if context != new { context = new }
    }

    /// True when the bus identifies any folder/bucket/outcome whose
    /// participation matches `folder`. Used by left-column rows to
    /// know when to glow.
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
