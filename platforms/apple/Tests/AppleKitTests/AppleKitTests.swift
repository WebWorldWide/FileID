import Foundation
import Testing
@testable import FileIDAppleKit

@Suite("FileIDAppleKit C interface", .serialized)
struct AppleKitTests {
    @Test func reportsItsInterfaceVersion() {
        #expect(fileid_apple_abi_version() == 1)
    }

    @Test func sleepAssertionIsTakenAndReleased() {
        let before = SleepAssertions.shared.count
        let id = "FileID test".withCString { fileid_apple_sleep_begin($0) }
        #expect(id != 0)
        #expect(SleepAssertions.shared.count == before + 1)
        fileid_apple_sleep_end(id)
        fileid_apple_sleep_end(id)
        #expect(SleepAssertions.shared.count == before)
    }

    @Test func bookmarkLeaseResolvesAFolder() throws {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDAppleKit-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let bookmark = try folder.bookmarkData()

        var path: UnsafeMutablePointer<CChar>?
        var stale = true
        let lease = bookmark.withUnsafeBytes { bytes in
            fileid_apple_bookmark_open(bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count, &path, &stale)
        }
        #expect(lease != 0)
        let resolved = path.map { String(cString: $0) }
        fileid_apple_free(path)
        #expect(resolved.map { URL(fileURLWithPath: $0).resolvingSymlinksInPath().path }
                == folder.resolvingSymlinksInPath().path)
        #expect(!stale)
        fileid_apple_bookmark_close(lease)
        #expect(BookmarkLeases.shared.count == 0)
    }

    @Test func garbageBookmarkIsRefused() {
        var path: UnsafeMutablePointer<CChar>?
        var stale = false
        let bytes: [UInt8] = Array("not a bookmark".utf8)
        let lease = fileid_apple_bookmark_open(bytes, bytes.count, &path, &stale)
        #expect(lease == 0)
        #expect(path == nil)
        #expect(fileid_apple_bookmark_open(nil, 0, &path, &stale) == 0)
    }

    @Test func shutdownReleasesEverything() {
        _ = "FileID test".withCString { fileid_apple_sleep_begin($0) }
        fileid_apple_shutdown()
        #expect(SleepAssertions.shared.count == 0)
        #expect(BookmarkLeases.shared.count == 0)
    }

    private func scratchFolder() throws -> URL {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDAppleKit-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }

    // The real Trash is never used in tests: `TrashMover.testRoot` stands in for it.
    @Test func trashItemReportsWhereTheFileLanded() throws {
        let folder = try scratchFolder()
        defer { try? FileManager.default.removeItem(at: folder); TrashMover.testRoot = nil }
        let bin = folder.appendingPathComponent("Trash", isDirectory: true)
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        TrashMover.testRoot = bin
        let file = folder.appendingPathComponent("photo é.jpg")
        try Data("bytes".utf8).write(to: file)

        var landed: UnsafeMutablePointer<CChar>?
        var error: UnsafeMutablePointer<CChar>?
        let status = file.path.withCString { fileid_apple_trash_item($0, &landed, &error) }
        #expect(status == 0)
        #expect(error == nil)
        let path = landed.map { String(cString: $0) }
        fileid_apple_free(landed)
        #expect(path == bin.appendingPathComponent("photo é.jpg").path)
        #expect(!FileManager.default.fileExists(atPath: file.path))
        #expect(FileManager.default.contents(atPath: path ?? "") == Data("bytes".utf8))
    }

    @Test func trashItemOfAMissingFileFails() throws {
        let folder = try scratchFolder()
        defer { try? FileManager.default.removeItem(at: folder); TrashMover.testRoot = nil }
        TrashMover.testRoot = folder
        var landed: UnsafeMutablePointer<CChar>?
        var error: UnsafeMutablePointer<CChar>?
        let missing = folder.appendingPathComponent("missing/none.jpg").path
        #expect(missing.withCString { fileid_apple_trash_item($0, &landed, &error) } != 0)
        #expect(landed == nil)
        #expect(error.map { String(cString: $0) }?.isEmpty == false)
        fileid_apple_free(error)
        #expect(fileid_apple_trash_item(nil, &landed, &error) != 0)
        fileid_apple_free(error)
    }

    @Test func finderTagsRoundTrip() throws {
        let folder = try scratchFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("tagged.jpg")
        try Data("x".utf8).write(to: file)

        var error: UnsafeMutablePointer<CChar>?
        let names = ["Beach", "Grandma Ö"]
        let status = names.map { strdup($0) }.withUnsafeBufferPointer { buffer -> Int32 in
            defer { buffer.forEach { free($0) } }
            let constants = buffer.map { UnsafePointer($0) }
            return constants.withUnsafeBufferPointer { pointers in
                file.path.withCString { fileid_apple_tags_write($0, pointers.baseAddress, pointers.count, &error) }
            }
        }
        #expect(status == 0)
        let json = file.path.withCString { fileid_apple_tags_read($0, &error) }
        let decoded = json.map { String(cString: $0) }
        fileid_apple_free(json)
        #expect(decoded.flatMap { try? JSONDecoder().decode([String].self, from: Data($0.utf8)) } == names)

        #expect(file.path.withCString { fileid_apple_tags_write($0, nil, 0, &error) } == 0)
        let cleared = file.path.withCString { fileid_apple_tags_read($0, &error) }
        #expect(cleared.map { String(cString: $0) } == "[]")
        fileid_apple_free(cleared)

        let missing = folder.appendingPathComponent("missing.jpg").path
        #expect(missing.withCString { fileid_apple_tags_read($0, &error) } == nil)
        #expect(error != nil)
        fileid_apple_free(error)
        #expect(missing.withCString { fileid_apple_tags_write($0, nil, 0, &error) } != 0)
        fileid_apple_free(error)
    }
}
