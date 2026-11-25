import Foundation
import Testing
@testable import FileID

@Suite struct ThumbnailOfflineAliasTests {
    private func scratch() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(component: "fileid-thumb-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    @Test func aCachedThumbnailIsFoundByPathAndSizeWithoutTheFileBeingThere() throws {
        let dir = try scratch()
        defer { try? FileManager.default.removeItem(at: dir) }
        let blob = dir.appending(component: "blob.jpg")
        try Data("jpeg".utf8).write(to: blob)
        let alias = ThumbnailDiskCache.aliasURL(in: dir, path: "/Volumes/Gone/a.jpg", size: 256, scale: 2)
        ThumbnailDiskCache.link(alias: alias, to: blob)
        #expect(ThumbnailDiskCache.load(alias) == Data("jpeg".utf8))
        #expect(ThumbnailDiskCache.aliasURL(in: dir, path: "/Volumes/Gone/a.jpg", size: 512, scale: 2) != alias)
    }

    @Test func aChangedFileMovesTheAliasToTheNewThumbnail() throws {
        let dir = try scratch()
        defer { try? FileManager.default.removeItem(at: dir) }
        let old = dir.appending(component: "old.jpg"), new = dir.appending(component: "new.jpg")
        try Data("old".utf8).write(to: old)
        try Data("new".utf8).write(to: new)
        let alias = ThumbnailDiskCache.aliasURL(in: dir, path: "/x/a.jpg", size: 256, scale: 2)
        ThumbnailDiskCache.link(alias: alias, to: old)
        ThumbnailDiskCache.link(alias: alias, to: new)
        #expect(ThumbnailDiskCache.load(alias) == Data("new".utf8))
    }
}
