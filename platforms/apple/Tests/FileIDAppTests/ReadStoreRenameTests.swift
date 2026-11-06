import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Suggested-name renames")
struct ReadStoreRenameTests {
    private func folder() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDRename-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        return root
    }

    @Test("Names taken on disk or earlier in the batch get a numbered bump; a case-only rename keeps its slot")
    func planBumpsCollisions() throws {
        let root = try folder()
        defer { try? FileManager.default.removeItem(at: root) }
        for name in ["beach.jpg", "IMG_1.jpg", "IMG_2.JPG", "Sunset.jpg", "keep.jpg"] {
            try Data(name.utf8).write(to: root.appendingPathComponent(name))
        }
        let candidates = [
            CatalogWrites.RenameCandidate(fileID: 1, path: root.appendingPathComponent("IMG_1.jpg").path, proposed: "beach"),
            CatalogWrites.RenameCandidate(fileID: 2, path: root.appendingPathComponent("IMG_2.JPG").path, proposed: "beach"),
            CatalogWrites.RenameCandidate(fileID: 3, path: root.appendingPathComponent("Sunset.jpg").path, proposed: "sunset"),
            CatalogWrites.RenameCandidate(fileID: 4, path: root.appendingPathComponent("keep.jpg").path, proposed: "keep"),
            CatalogWrites.RenameCandidate(fileID: 5, path: root.appendingPathComponent("keep.jpg").path, proposed: nil)
        ]
        let (plans, unnamed) = CatalogWrites.planRenames(candidates)
        #expect(plans.map(\.fileID) == [1, 2, 3])
        #expect(plans.map(\.target.lastPathComponent) == ["beach_2.jpg", "beach_3.JPG", "sunset.jpg"])
        #expect(unnamed == 1)
    }

    @Test("Undo skips a file that moved, changed, or whose old name is taken")
    func undoBlockers() throws {
        let root = try folder()
        defer { try? FileManager.default.removeItem(at: root) }
        let new = root.appendingPathComponent("beach.jpg")
        try Data("photo".utf8).write(to: new)
        let outcome = ReadStore.RenameOutcome(fileID: 1, oldPath: root.appendingPathComponent("IMG_1.jpg").path, newPath: new.path)
        #expect(CatalogWrites.undoBlocker(outcome, catalogPath: new.path) == nil)
        #expect(CatalogWrites.undoBlocker(outcome, catalogPath: root.appendingPathComponent("elsewhere.jpg").path) == "moved")

        try Data("other".utf8).write(to: root.appendingPathComponent("IMG_1.jpg"))
        #expect(CatalogWrites.undoBlocker(outcome, catalogPath: new.path) == "occupied")

        try Data("edited photo".utf8).write(to: new)
        #expect(CatalogWrites.undoBlocker(outcome, catalogPath: new.path) == "changed")
    }
}
