import Testing
@testable import FileID

@Suite("Folder map counts")
struct SankeyFolderCountTests {
    @Test func rollupNodeStandsForItsFolders() {
        #expect(SankeyFlowView.Layout.folderCount(nodes: 9, rolledUp: 2) == 10)
        #expect(SankeyFlowView.Layout.folderCount(nodes: 3, rolledUp: 0) == 3)
        #expect(SankeyFlowView.Layout.folderCount(nodes: 0, rolledUp: 0) == 0)
        #expect(SankeyFlowView.Layout.folderCount(nodes: 9, rolledUp: 1) == 9)
    }
}

@MainActor @Suite("Folder map slots")
struct SankeySlotTests {
    private func nodes(_ counts: [Int]) -> [SankeyFlowView.Node] {
        counts.enumerated().map { index, count in
            SankeyFlowView.Node(id: "n\(index)", label: "Folder \(index)", identityKey: "n\(index)", count: count, icon: "folder", tint: .gray, isSource: false)
        }
    }

    @Test func everyFolderKeepsTwoLabelLinesAndAGapAtTheHeightTheMapAsks() {
        for counts in [[1, 2, 1], [40, 1, 1, 1, 1], [90, 40, 3, 2, 1, 1, 1, 1, 1], [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5]] {
            let list = nodes(counts)
            let slots = SankeyFlowView.layoutSlots(nodes: list, totalHeight: SankeyFlowView.mapHeight(nodeCount: list.count))
            let ordered = list.compactMap { slots[$0.id] }.sorted { $0.topY < $1.topY }
            #expect(ordered.count == list.count)
            #expect(ordered.allSatisfy { $0.height >= 44 }, "\(counts): a node is shorter than its two label lines")
            for (above, below) in zip(ordered, ordered.dropFirst()) {
                #expect(below.topY - (above.topY + above.height) >= 14 - 0.001, "\(counts): nodes touch")
            }
        }
    }
}
