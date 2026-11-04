import Foundation
import Testing
@testable import FileIDShared

@Suite("User home outside the sandbox container")
struct UserHomeTests {
    @Test func containerPathsAreRecognized() {
        #expect(UserHome.isContainer("/Users/a/Library/Containers/com.fileid.app/Data"))
        #expect(!UserHome.isContainer("/Users/a"))
    }

    @Test func resolvedHomeIsNeverTheContainer() {
        #expect(!UserHome.path.isEmpty)
        #expect(!UserHome.isContainer(UserHome.path))
        #expect(UserHome.trashURL.lastPathComponent == ".Trash")
        #expect(UserHome.trashURL.deletingLastPathComponent().path == UserHome.url.path)
    }
}
