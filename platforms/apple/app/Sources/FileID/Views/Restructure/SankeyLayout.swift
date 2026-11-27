import SwiftUI

/// Nested layout value types for SankeyFlowView, kept in this extension file so the view file stays on body, drawing and hit-testing.
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
        /// Long-tail source folder paths collapsed into the "+ N more folders" node.
        var rollupSourceFolders: [String]
        var rollupDestBuckets: [String]
        var isPopulated: Bool

        /// Folders in a column: the "+ N more" rollup node stands for N folders, not one.
        static func folderCount(nodes: Int, rolledUp: Int) -> Int { rolledUp == 0 ? nodes : nodes - 1 + rolledUp }
        var sourceFolderCount: Int { Self.folderCount(nodes: sources.count, rolledUp: rollupSourceFolders.count) }
        var destinationFolderCount: Int { Self.folderCount(nodes: destinations.count, rolledUp: rollupDestBuckets.count) }

        /// The node a source folder is drawn in: its own, or the rollup when it fell in the long tail.
        func sourceNodeID(forFolder folder: String) -> String { srcSlots["src:\(folder)"] != nil ? "src:\(folder)" : SankeyFlowView.otherSourceID }
        func destNodeID(forBucket bucket: String) -> String { dstSlots["dst:\(bucket)"] != nil ? "dst:\(bucket)" : SankeyFlowView.otherDestID }

        /// The ribbons a hover traces: the folder's or bucket's own flows, the flows of an outcome, or the single hovered ribbon.
        func highlightedFlowIDs(for context: RestructureHoverContext?) -> Set<String> {
            switch context {
            case .none:
                return []
            case .flow(let folder, let bucket)?:
                return Set(flows.filter { $0.sourceFolder == folder && $0.destBucket == bucket }.map(\.id))
            case .sourceFolder(let folder)?:
                let node = sourceNodeID(forFolder: folder)
                return Set(flows.filter { $0.sourceID == node }.map(\.id))
            case .destBucket(let bucket)?:
                let node = destNodeID(forBucket: bucket)
                return Set(flows.filter { $0.destID == node }.map(\.id))
            case .outcome(let outcome)?:
                return Set(flows.filter { $0.outcomes.contains(outcome) }.map(\.id))
            }
        }

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
        /// What the ribbon's ends are called: the folder, or "N other folders" when an end is the rolled-up long tail.
        var sourceName: String = ""
        var destName: String = ""
        var outcomes: Set<RestructureOutcome> = []
    }

    struct Slot: Hashable {
        let topY: CGFloat
        let height: CGFloat
        var midY: CGFloat { topY + height / 2 }
    }
}
