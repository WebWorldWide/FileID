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
}
