// Undo-journal invariants for bulk tagging. The journal must be rewritten on
// EVERY batch (an all-unchanged batch CLEARS it, so "Undo last tags" can't
// replay a different earlier batch), and undo must refuse a stale or
// identity-mismatched entry, mirroring the rename journal. (F-C3-034)
//
// Each test uses an isolated UserDefaults suite so the shared journal key is
// never touched concurrently by other suites.
import Testing
@testable import FileIDShared
import Foundation

@Suite("TagWriter — undo journal (F-C3-034)")
struct TagUndoJournalTests {

    private func isolatedDefaults() -> (UserDefaults, String) {
        let name = "fileid.tagundo.test.\(UUID().uuidString)"
        return (UserDefaults(suiteName: name)!, name)
    }

    private func scratch(_ hint: String, body: String = "x") throws -> URL {
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("FileIDTagUndo\(hint)-\(UUID().uuidString).txt")
        try body.write(to: url, atomically: true, encoding: .utf8)
        return url
    }

    @Test("an all-unchanged batch CLEARS the journal — no stale earlier batch")
    func allUnchangedClearsJournal() throws {
        let (defaults, name) = isolatedDefaults()
        defer { defaults.removePersistentDomain(forName: name) }

        let a = try scratch("BatchA")
        let b = try scratch("BatchB")
        defer {
            try? FileManager.default.removeItem(at: a)
            try? FileManager.default.removeItem(at: b)
        }

        // Batch 1: A genuinely changes → journal records this batch.
        let first = TagWriter.addTagsBulkDetailed(["Vacation"], to: [a], journal: defaults)
        #expect(first.outcomes.count == 1)
        #expect(defaults.data(forKey: TagWriter.undoJournalKey) != nil)

        // Batch 2: B already carries the tag → all-unchanged. The journal MUST
        // be cleared; otherwise "Undo last tags" would strip batch 1 from A.
        try TagWriter.setTags(["Vacation"], at: b)
        let second = TagWriter.addTagsBulkDetailed(["Vacation"], to: [b], journal: defaults)
        #expect(second.outcomes.isEmpty)
        #expect(second.unchanged == 1)
        #expect(defaults.data(forKey: TagWriter.undoJournalKey) == nil,
                "all-unchanged batch left a stale earlier batch in the journal")
    }

    @Test("a changed batch overwrites the journal with only its own outcomes")
    func changedBatchOverwritesJournal() throws {
        let (defaults, name) = isolatedDefaults()
        defer { defaults.removePersistentDomain(forName: name) }

        let a = try scratch("OverA")
        let b = try scratch("OverB")
        defer {
            try? FileManager.default.removeItem(at: a)
            try? FileManager.default.removeItem(at: b)
        }

        _ = TagWriter.addTagsBulkDetailed(["First"], to: [a], journal: defaults)
        _ = TagWriter.addTagsBulkDetailed(["Second"], to: [b], journal: defaults)

        let data = try #require(defaults.data(forKey: TagWriter.undoJournalKey))
        let journal = try JSONDecoder().decode([TagWriter.TagOutcome].self, from: data)
        #expect(journal.map(\.path) == [b.path],
                "journal must hold only the most recent batch")
        #expect(journal.first?.addedTags == ["Second"])
    }

    @Test("undo of the most recent matching batch removes only FileID's tags")
    func undoMatchingBatch() throws {
        let (defaults, name) = isolatedDefaults()
        defer { defaults.removePersistentDomain(forName: name) }

        let a = try scratch("Match")
        defer { try? FileManager.default.removeItem(at: a) }
        try TagWriter.setTags(["Important"], at: a)

        let result = TagWriter.addTagsBulkDetailed(["Vacation"], to: [a], journal: defaults)
        let undo = TagWriter.undoBulkAdd(result.outcomes)
        #expect(undo.undone == 1)
        #expect(undo.skipped == 0)
        #expect(undo.failed == 0)
        #expect(TagWriter.readTags(at: a) == ["Important"],
                "the user's own tag must survive undo")
    }

    @Test("undo SKIPS a path now occupied by a different file (identity guard)")
    func undoSkipsReplacedFile() throws {
        let (defaults, name) = isolatedDefaults()
        defer { defaults.removePersistentDomain(forName: name) }

        let a = try scratch("Identity")   // 1-byte body
        defer { try? FileManager.default.removeItem(at: a) }

        let result = TagWriter.addTagsBulkDetailed(["Vacation"], to: [a], journal: defaults)
        #expect(result.outcomes.count == 1)

        // A DIFFERENT, larger file now occupies the same path and coincidentally
        // carries the same tag the journal recorded. Without the identity guard,
        // undo would strip "Vacation" from this unrelated file.
        try FileManager.default.removeItem(at: a)
        try "a much larger replacement body for this path".write(to: a, atomically: true, encoding: .utf8)
        try TagWriter.setTags(["Vacation", "Keep"], at: a)

        let undo = TagWriter.undoBulkAdd(result.outcomes)
        #expect(undo.undone == 0)
        #expect(undo.skipped == 1)
        #expect(Set(TagWriter.readTags(at: a)) == Set(["Vacation", "Keep"]),
                "identity mismatch must not strip the replacement file's tags")
    }

    @Test("undo ignores a journal entry older than the max age (age guard)")
    func undoSkipsStaleEntry() throws {
        let a = try scratch("Stale")
        defer { try? FileManager.default.removeItem(at: a) }
        try TagWriter.setTags(["Vacation"], at: a)

        // Identity still matches, but the batch is back-dated past the max age.
        let stale = TagWriter.TagOutcome(
            capturing: a.path, addedTags: ["Vacation"],
            at: Date(timeIntervalSinceNow: -(TagWriter.undoJournalMaxAge + 60)))
        let undo = TagWriter.undoBulkAdd([stale])
        #expect(undo.skipped == 1)
        #expect(undo.undone == 0)
        #expect(TagWriter.readTags(at: a) == ["Vacation"],
                "a weeks-old journal entry must not strip tags")
    }
}
