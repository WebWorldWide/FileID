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
