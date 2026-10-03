import Foundation
import Testing
@testable import FileIDEngine

@Suite("Security-scoped folder access", .serialized)
struct SecurityScopedAccessRegistryTests {
    @Test("bookmark path must match the requested folder")
    func bookmarkPathMustMatch() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-scope-root-\(UUID().uuidString)", isDirectory: true)
        let other = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-scope-other-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let bookmark = try root.bookmarkData(
            options: [],
            includingResourceValuesForKeys: nil,
            relativeTo: nil
        )

        let path = try SecurityScopedAccessRegistry.shared.acquire(path: root.path, bookmark: bookmark)

        #expect(path == root.standardizedFileURL.path)
        #expect(throws: SecurityScopedAccessError.self) {
            try SecurityScopedAccessRegistry.shared.acquire(path: other.path, bookmark: bookmark)
        }
    }

    @Test("malformed bookmarks are rejected")
    func malformedBookmarkIsRejected() {
        #expect(throws: SecurityScopedAccessError.self) {
            try SecurityScopedAccessRegistry.shared.acquire(path: "/tmp/fileid", bookmark: Data([1, 2, 3]))
        }
    }
}
