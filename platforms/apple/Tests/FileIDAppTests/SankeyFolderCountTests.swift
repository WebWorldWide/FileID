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

@MainActor @Suite("Folder map hover")
struct SankeyHoverTests {
    private func proposal(_ id: Int64, from folder: String, to bucket: String, kind: RestructureView.ProposalKind = .movedOutAsOutlier) -> RestructureView.Proposal {
        RestructureView.Proposal(fileID: id, oldPath: "\(folder)/\(id).jpg", newPath: "\(bucket)/\(id).jpg", bucket: bucket, sourceFolder: folder,
                                 filename: "\(id).jpg", sourceName: folder, kind: kind)
    }

    private func crowded() -> SankeyFlowView.Layout {
        var proposals: [RestructureView.Proposal] = []
        var id: Int64 = 0
        for folder in 0..<12 {
            for _ in 0..<(40 - folder) {
                id += 1
                proposals.append(proposal(id, from: "/lib/Folder\(folder)", to: "Bucket\(folder)", kind: folder.isMultiple(of: 2) ? .dissolved : .movedOutAsOutlier))
            }
        }
        return SankeyFlowView.computeLayout(proposals: proposals, width: 900, height: 600)
    }

    @Test func aRibbonIntoTheRolledUpGroupNamesTheGroupNotOneFolder() throws {
        let layout = crowded()
        let rolled = try #require(layout.flows.first { $0.sourceID == SankeyFlowView.otherSourceID && $0.destID == SankeyFlowView.otherDestID })
        #expect(rolled.sourceName == SankeyFlowView.otherFoldersName(layout.rollupSourceFolders.count))
        #expect(rolled.destName == SankeyFlowView.otherFoldersName(layout.rollupDestBuckets.count))
        let own = try #require(layout.flows.first { $0.sourceID != SankeyFlowView.otherSourceID })
        #expect(own.sourceName == "Folder0" || own.sourceName.hasPrefix("Folder"))
    }

    @Test func hoveringAFolderOrBucketOrOutcomeTracesItsRibbons() throws {
        let layout = crowded()
        let visibleFolder = try #require(layout.sources.first { $0.id != SankeyFlowView.otherSourceID })
        let traced = layout.highlightedFlowIDs(for: .sourceFolder(visibleFolder.identityKey))
        #expect(!traced.isEmpty)
        #expect(traced.allSatisfy { layout.flowsByID[$0]?.sourceID == visibleFolder.id })
        let bucket = try #require(layout.destinations.first { $0.id != SankeyFlowView.otherDestID })
        let into = layout.highlightedFlowIDs(for: .destBucket(bucket.identityKey))
        #expect(!into.isEmpty && into.allSatisfy { layout.flowsByID[$0]?.destID == bucket.id })
        let tail = try #require(layout.rollupSourceFolders.first)
        #expect(!layout.highlightedFlowIDs(for: .sourceFolder(tail)).isEmpty)
        #expect(!layout.highlightedFlowIDs(for: .outcome(.reorganize)).isEmpty)
        #expect(layout.highlightedFlowIDs(for: .outcome(.keep)).isEmpty)
        #expect(layout.highlightedFlowIDs(for: nil).isEmpty)
    }
}
