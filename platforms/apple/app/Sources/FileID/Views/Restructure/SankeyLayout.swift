import SwiftUI

/// Nested value types for SankeyFlowView's layout. Lives in a sibling
/// extension file so the main `SankeyFlowView.swift` focuses on the View
/// body + drawing + hit-testing, and the layout primitives are easy to
/// find. Public access shape (`SankeyFlowView.Layout`, `SankeyFlowView.Node`,
/// `SankeyFlowView.Flow`, `SankeyFlowView.Slot`) is unchanged — Swift
/// resolves nested types across all extensions of the parent type.
extension SankeyFlowView {
    struct Layout {
        var sources: [Node]
        var destinations: [Node]
        var flows: [Flow]
        var flowsByID: [String: Flow]
        var srcSlots: [String: Slot]
        var dstSlots: [String: Slot]
        var totalFlowCount: Int
        var destinationsForSource: [String: Set<String>]
        var sourcesForDestination: [String: Set<String>]
        var nodesByOutcome: [RestructureOutcome: Set<String>]
        /// Long-tail source folder paths that were collapsed into the
        /// "+ N more folders" rollup node. Tapping the rollup drills
        /// down into these.
        var rollupSourceFolders: [String]
        var rollupDestBuckets: [String]
        var isPopulated: Bool

        static let empty = Layout(
            sources: [], destinations: [], flows: [],
            flowsByID: [:], srcSlots: [:], dstSlots: [:],
            totalFlowCount: 0,
            destinationsForSource: [:],
            sourcesForDestination: [:],
            nodesByOutcome: [:],
            rollupSourceFolders: [],
            rollupDestBuckets: [],
            isPopulated: false
        )
    }

    struct Node: Identifiable, Hashable {
        let id: String
        let label: String
        let identityKey: String
        let count: Int
        let icon: String
        let tint: Color
        let isSource: Bool
    }

    struct Flow: Identifiable, Hashable {
        var id: String { "\(sourceID)→\(destID)" }
        let sourceID: String
        let destID: String
        let sourceFolder: String
        let destBucket: String
        let tint: Color
        let count: Int
    }

    struct Slot: Hashable {
        let topY: CGFloat
        let height: CGFloat
        var midY: CGFloat { topY + height / 2 }
    }
}
