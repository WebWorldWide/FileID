import Foundation
import Testing
@testable import FileID

@Suite("Security-scoped bookmark handoff", .serialized)
struct SecurityScopedBookmarkTests {
    @Test("interprocess bookmark resolves without app-scoped options")
    func interprocessBookmarkResolvesInReceivingProcess() throws {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-ipc-bookmark-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }

        let bookmark = try SecurityScopedBookmark.makeIPCBookmark(for: folder)
        var stale = false
        let resolved = try URL(
            resolvingBookmarkData: bookmark,
            options: [],
            relativeTo: nil,
            bookmarkDataIsStale: &stale
        )
        defer { resolved.stopAccessingSecurityScopedResource() }

        #expect(resolved.standardizedFileURL.path == folder.standardizedFileURL.path)
    }
}
