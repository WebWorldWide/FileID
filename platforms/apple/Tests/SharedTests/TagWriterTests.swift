// Unit tests for TagWriter. Pure-merge tests are deterministic; the
// roundtrip test writes/reads a temp file in NSTemporaryDirectory().
import Testing
@testable import FileIDShared
import Foundation

@Suite("TagWriter — merge logic")
struct TagWriterMergeTests {
    @Test("adding a new tag appends")
    func addNew() {
        let merged = TagWriter.mergeTags(existing: ["Mom"], adding: ["Dad"])
        #expect(merged == ["Mom", "Dad"])
    }

    @Test("adding a duplicate (different case) is a no-op")
    func dupCase() {
        let merged = TagWriter.mergeTags(existing: ["Mom"], adding: ["mom", "MOM"])
        #expect(merged == ["Mom"])
    }

    @Test("whitespace-only tags are dropped")
    func dropsWhitespace() {
        let merged = TagWriter.mergeTags(existing: [], adding: ["  ", "\n", "Real"])
        #expect(merged == ["Real"])
    }

    @Test("preserves existing order, appends new")
    func order() {
        let merged = TagWriter.mergeTags(
            existing: ["Mom", "Dad", "Brother"],
            adding: ["Mom", "Sister"]
        )
        #expect(merged == ["Mom", "Dad", "Brother", "Sister"])
    }
}

@Suite("TagWriter — bulk batch results")
struct TagWriterBatchTests {
    /// New "added vs unchanged" shape (P9).
    @Test("addTagsBulk distinguishes added from unchanged")
    func addedVsUnchanged() throws {
        let dir = URL(fileURLWithPath: NSTemporaryDirectory())
        let a = dir.appendingPathComponent("FileIDBatchA-\(UUID().uuidString).txt")
        let b = dir.appendingPathComponent("FileIDBatchB-\(UUID().uuidString).txt")
        try "x".write(to: a, atomically: true, encoding: .utf8)
        try "y".write(to: b, atomically: true, encoding: .utf8)
        defer {
            try? FileManager.default.removeItem(at: a)
            try? FileManager.default.removeItem(at: b)
        }
        // Pre-tag `a` with "Mom"; leave `b` untagged.
        try TagWriter.setTags(["Mom"], at: a)

        let result = TagWriter.addTagsBulk(["Mom"], to: [a, b])
        #expect(result.added == 1, "expected 1 added (b), got \(result.added)")
        #expect(result.unchanged == 1, "expected 1 unchanged (a), got \(result.unchanged)")
        #expect(result.failed == 0)
        #expect(result.succeeded == 2, "succeeded = added + unchanged")
    }
}

@Suite("TagWriter — file roundtrip")
struct TagWriterRoundtripTests {

    /// Create a tiny scratch file in tmp, write tags, read them back,
    /// then clean up. Doesn't depend on any pre-existing file.
    @Test("set then read returns the same tags")
    func roundtrip() throws {
        let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("FileIDTagWriterTest-\(UUID().uuidString).txt")
        try "scratch".write(to: tmp, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: tmp) }

        try TagWriter.setTags(["Mom", "Beach"], at: tmp)
        let got = TagWriter.readTags(at: tmp).sorted()
        #expect(got == ["Beach", "Mom"], "got \(got)")
    }

    @Test("addTags merges with existing")
    func addMerges() throws {
        let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("FileIDTagWriterTest-\(UUID().uuidString).txt")
        try "scratch".write(to: tmp, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: tmp) }

        try TagWriter.setTags(["Existing"], at: tmp)
        _ = try TagWriter.addTags(["New"], at: tmp)
        let got = Set(TagWriter.readTags(at: tmp))
        #expect(got == Set(["Existing", "New"]))
    }

    @Test("removeTags drops only what's listed")
    func removeOnlyListed() throws {
        let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("FileIDTagWriterTest-\(UUID().uuidString).txt")
        try "scratch".write(to: tmp, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: tmp) }

        try TagWriter.setTags(["Mom", "Dad", "Beach"], at: tmp)
        _ = try TagWriter.removeTags(["Dad"], at: tmp)
        let got = Set(TagWriter.readTags(at: tmp))
        #expect(got == Set(["Mom", "Beach"]))
    }

    @Test("setTags with empty array clears tags")
    func clear() throws {
        let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("FileIDTagWriterTest-\(UUID().uuidString).txt")
        try "scratch".write(to: tmp, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: tmp) }

        try TagWriter.setTags(["Mom"], at: tmp)
        try TagWriter.setTags([], at: tmp)
        #expect(TagWriter.readTags(at: tmp).isEmpty)
    }
}
