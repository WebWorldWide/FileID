import Foundation
import Testing
@testable import FileIDEngine

@Suite("Engine Trash")
struct EngineTrashTests {
    /// Shipped and ordinary test builds have no test Trash root: the Trash folders are
    /// the user's, whatever FILEID_TEST_TRASH_ROOT says. Only `-DFILEID_TEST_TRASH_ROOT`
    /// conformance builds replace them.
    @Test func ordinaryBuildsUseTheUsersTrash() {
        #if !FILEID_TEST_TRASH_ROOT
        #expect(EngineTrash.folders(for: URL(fileURLWithPath: "/tmp/x.jpg")) == nil)
        #endif
    }
}
