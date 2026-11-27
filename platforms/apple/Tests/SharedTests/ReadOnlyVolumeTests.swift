import Foundation
import Testing
@testable import FileIDShared
@testable import FileIDModelInfo

@Suite("Read-only volumes", .serialized)
struct ReadOnlyVolumeTests {
    // The probe is process-wide, so it flags only paths carrying this marker; other suites running alongside never see it.
    private static let marker = "fileid-ro-volume-tests"
    private static func scratch(_ name: String) -> URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("\(marker)/\(UUID().uuidString)/\(name)")
    }

    private func withProbe<T>(_ name: String?, _ body: () throws -> T) rethrows -> T {
        let saved = ReadOnlyLocations.pathOverride
        let marker = Self.marker
        ReadOnlyLocations.pathOverride = { url in url.path.contains(marker) ? name : nil }
        defer { ReadOnlyLocations.pathOverride = saved }
        return try body()
    }

    @Test func aVolumeFlaggedReadOnlyRefusesWritesWithASentenceNamingTheDrive() throws {
        let url = Self.scratch("a.jpg")
        try withProbe("Backup") {
            do {
                try ReadOnlyLocations.requireSourceMutation(url)
                Issue.record("expected a refusal")
            } catch {
                let text = error.localizedDescription
                #expect(text.contains("\u{201C}Backup\u{201D} is a read-only drive"))
                #expect(!text.contains("example data"))
            }
        }
        try withProbe(nil) { try ReadOnlyLocations.requireWritable(url) }
    }

    @Test func aBatchWithOneReadOnlyTargetIsRefusedWholeWithTheCount() {
        let urls = (0..<3).map { Self.scratch("\($0).jpg") } + [FileManager.default.temporaryDirectory.appendingPathComponent("elsewhere-\(UUID().uuidString).jpg")] + [Self.scratch("x.jpg")]
        let refusal = withProbe("Backup") { ReadOnlyLocations.batchRefusal(Array(urls.prefix(4))) }
        #expect(refusal?.readOnly == 3)
        #expect(refusal?.total == 4)
        #expect(refusal?.errorDescription?.contains("3 of 4 files are on it") == true)
        #expect(withProbe("Backup") { ReadOnlyLocations.batchRefusal([urls[3]]) } == nil)
    }

    @Test func addTagsOnAReadOnlyDriveRefusesBeforeReadingTags() throws {
        let url = Self.scratch("tag.jpg")
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("x".utf8).write(to: url)
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        withProbe("Backup") {
            #expect(throws: ReadOnlyLocations.ProtectionError.self) { try TagWriter.addTags(["Trip"], at: url) }
            let result = TagWriter.addTagsBulk(["Trip"], to: [url])
            #expect(result.failed == 1 && result.added == 0)
            #expect(result.firstError?.contains("read-only drive") == true)
        }
    }

    @Test func aRemountIsNoticedOnceTheCachedAnswerExpires() {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("remount.jpg")
        let cache = ReadOnlyLocations.VolumeCache()
        let start = Date()
        #expect(ReadOnlyLocations.volumeName(for: url, cache: cache, now: start, probe: { _ in "Backup" }) == "Backup")
        #expect(ReadOnlyLocations.volumeName(for: url, cache: cache, now: start + 1, probe: { _ in nil }) == "Backup", "cached inside the TTL")
        #expect(ReadOnlyLocations.volumeName(for: url, cache: cache, now: start + ReadOnlyLocations.volumeTTL + 1, probe: { _ in nil }) == nil)
    }

    @Test func aLinkIsJudgedByTheVolumeItPointsOnto() throws {
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("link-volume-\(UUID().uuidString)")
        let target = base.appendingPathComponent("target-dir")
        try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let link = base.appendingPathComponent("link")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: target)
        final class Seen: @unchecked Sendable { var path = "" }
        let seen = Seen()
        _ = ReadOnlyLocations.volumeName(for: link.appendingPathComponent("new/file.jpg"), cache: ReadOnlyLocations.VolumeCache(), now: Date()) { url in
            seen.path = url.path
            return nil
        }
        #expect(seen.path.hasSuffix("/target-dir/new/file.jpg"), "probed \(seen.path)")
    }

    @Test func addTagsStopsWhenTheExistingTagsCannotBeReadInsteadOfOverwritingThem() {
        let missing = FileManager.default.temporaryDirectory.appendingPathComponent("no-such-\(UUID().uuidString).jpg")
        do {
            try TagWriter.addTags(["Trip"], at: missing)
            Issue.record("expected the read failure to stop the write")
        } catch TagWriter.Error.readFailed {
        } catch {
            Issue.record("expected readFailed, got \(error)")
        }
    }
}
