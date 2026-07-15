import Foundation
import Testing
@testable import FileIDShared

@Suite("Exact full-file digest")
struct ExactFileDigestTests {
    private func file(_ bytes: Data) throws -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-exact-\(UUID().uuidString).bin")
        try bytes.write(to: url)
        return url
    }

    @Test("Differences outside sampled regions remain distinct")
    func unsampledDifference() throws {
        var first = Data(repeating: 0x11, count: 20 * 1024 * 1024)
        var second = first
        first[7 * 1024 * 1024 + 123] = 0x22
        second[7 * 1024 * 1024 + 123] = 0x33
        let a = try file(first)
        let b = try file(second)
        defer {
            try? FileManager.default.removeItem(at: a)
            try? FileManager.default.removeItem(at: b)
        }
        let expected = Int64(first.count)
        #expect(ExactFileDigest.compute(url: a, expectedSize: expected) !=
                ExactFileDigest.compute(url: b, expectedSize: expected))
        #expect(!ExactFileDigest.match(first: a, second: b, expectedSize: expected))
    }

    @Test("A victim changed after grouping fails destructive revalidation")
    func changedVictimFails() throws {
        let original = Data("same bytes".utf8)
        let keeper = try file(original)
        let victim = try file(original)
        defer {
            try? FileManager.default.removeItem(at: keeper)
            try? FileManager.default.removeItem(at: victim)
        }
        let size = Int64(original.count)
        #expect(ExactFileDigest.match(
            first: keeper, second: victim, expectedSize: size))
        try Data("evil bytes".utf8).write(to: victim)
        #expect(!ExactFileDigest.match(
            first: keeper, second: victim, expectedSize: size))
    }

    @Test("Verified path identity rejects a same-path replacement")
    func pathReplacementFailsIdentity() throws {
        let bytes = Data("same bytes".utf8)
        let keeper = try file(bytes)
        let victim = try file(bytes)
        let replacement = try file(bytes)
        defer {
            try? FileManager.default.removeItem(at: keeper)
            try? FileManager.default.removeItem(at: victim)
            try? FileManager.default.removeItem(at: replacement)
        }
        let match = try #require(ExactFileDigest.matchWithIdentity(
            first: keeper, second: victim, expectedSize: Int64(bytes.count)))
        try FileManager.default.removeItem(at: victim)
        try FileManager.default.moveItem(at: replacement, to: victim)
        #expect(!ExactFileDigest.pathStillMatches(match.second))
    }

    @Test("Identical files match and stale expected sizes fail closed")
    func identicalAndStaleSize() throws {
        let bytes = Data("same bytes".utf8)
        let a = try file(bytes)
        let b = try file(bytes)
        defer {
            try? FileManager.default.removeItem(at: a)
            try? FileManager.default.removeItem(at: b)
        }
        #expect(ExactFileDigest.match(
            first: a, second: b, expectedSize: Int64(bytes.count)))
        #expect(ExactFileDigest.compute(
            url: a, expectedSize: Int64(bytes.count + 1)) == nil)
    }
}
