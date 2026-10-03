import Foundation
import Testing
@testable import FileIDEngine

@Suite("Security-scoped folder access", .serialized)
struct SecurityScopedAccessRegistryTests {
    @Test("bookmark path must match requested folder")
    func bookmarkPathMustMatch() throws {
        let registry = SecurityScopedAccessRegistry()
        let root = try makeFolderBookmark()
        defer { try? FileManager.default.removeItem(at: root.url) }
        let other = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-scope-other-\(UUID().uuidString)", isDirectory: true)

        let lease = try registry.acquire(path: root.url.path, bookmark: root.bookmark)
        #expect(lease.path == root.url.standardizedFileURL.path)
        #expect(throws: SecurityScopedAccessError.self) {
            try registry.acquire(path: other.path, bookmark: root.bookmark)
        }
        lease.release()
        #expect(registry.activeAccessCount == 0)
    }

    @Test("malformed bookmarks are rejected")
    func malformedBookmarkIsRejected() {
        let registry = SecurityScopedAccessRegistry()
        #expect(throws: SecurityScopedAccessError.self) {
            try registry.acquire(path: "/tmp/fileid", bookmark: Data([1, 2, 3]))
        }
    }

    @Test("root and operation leases balance security scope access")
    func rootAndOperationLeasesBalance() throws {
        let registry = SecurityScopedAccessRegistry()
        let root = try makeFolderBookmark()
        defer { try? FileManager.default.removeItem(at: root.url) }
        let operation = try registry.acquire(path: root.url.path, bookmark: root.bookmark)
        try registry.replaceRootAccess(path: root.url.path, bookmark: root.bookmark)

        #expect(registry.activeAccessCount == 1)
        operation.release()
        #expect(registry.activeAccessCount == 1)
        registry.releaseRootAccess()
        #expect(registry.activeAccessCount == 0)
    }

    private func makeFolderBookmark() throws -> (url: URL, bookmark: Data) {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-scope-lease-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        let bookmark = try url.bookmarkData(
            options: [],
            includingResourceValuesForKeys: nil,
            relativeTo: nil
        )
        return (url, bookmark)
    }
}
