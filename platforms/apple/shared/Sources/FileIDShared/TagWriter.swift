// Writes Finder tags via `URLResourceKey.tagNamesKey`; they are shared with Finder, Spotlight and Smart Folders, and survive moves on xattr-capable volumes.
// setTags overwrites, so the undo journal (the last batch's TagOutcomes) removes only the tags FileID added, never the user's own.
// The journal is rewritten on every bulk batch (an all-unchanged batch clears it); undo is gated by an age and file-identity guard.
import Foundation

public enum TagWriter {

    public enum Error: Swift.Error, LocalizedError {
        case readFailed(String)
        case writeFailed(String)

        public var errorDescription: String? {
            switch self {
            case .readFailed(let m):  return "Couldn't read tags: \(m)"
            case .writeFailed(let m): return "Couldn't write tags: \(m)"
            }
        }
    }

    /// Returns an empty array when the file has no tags or the read fails; use `readTagsThrowing` to tell them apart.
    public static func readTags(at url: URL) -> [String] {
        (try? readTagsThrowing(at: url)) ?? []
    }

    public static func readTagsThrowing(at url: URL) throws -> [String] {
        // URL caches resourceValues per instance, so re-resolve from the path: a stale list would break read-modify-write (undo).
        let fresh = URL(fileURLWithPath: url.path)
        let values: URLResourceValues
        do {
            values = try fresh.resourceValues(forKeys: [.tagNamesKey])
        } catch {
            throw Error.readFailed("\(url.lastPathComponent): \(error.localizedDescription)")
        }
        return values.tagNames ?? []
    }

    /// Replaces the file's Finder tags exactly; an empty array clears them. Uses NSURL.setResourceValue because the tagNames setter is macOS 26-only.
    public static func setTags(_ tags: [String], at url: URL) throws {
        try ReadOnlyLocations.requireSourceMutation(url)
        var seen: Set<String> = []
        let cleaned = tags
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .filter { seen.insert($0).inserted }

        do {
            try (url as NSURL).setResourceValue(cleaned as NSArray,
                                                  forKey: .tagNamesKey)
        } catch {
            throw Error.writeFailed("\(url.lastPathComponent): \(error.localizedDescription)")
        }
    }

    /// Adds the supplied tags to the existing ones, preserving tags from other tools or the user (never blow away a user's "Important" tag).
    @discardableResult
    public static func addTags(_ tags: [String], at url: URL) throws -> [String] {
        try ReadOnlyLocations.requireSourceMutation(url)
        let existing = try readTagsThrowing(at: url)
        let merged = mergeTags(existing: existing, adding: tags)
        if merged != existing {
            try setTags(merged, at: url)
        }
        return merged
    }

    /// Remove the supplied tags from the file's tag set. Tags not
    /// present are silently ignored. Returns the resulting tag list.
    @discardableResult
    public static func removeTags(_ tags: [String], at url: URL) throws -> [String] {
        try ReadOnlyLocations.requireSourceMutation(url)
        let existing = try readTagsThrowing(at: url)
        let toRemove = Set(tags.map { $0.lowercased() })
        let kept = existing.filter { !toRemove.contains($0.lowercased()) }
        if kept.count != existing.count {
            try setTags(kept, at: url)
        }
        return kept
    }

    /// Bulk apply: adds `tags` to every URL and returns per-URL results without bailing on the first error. `added` changed, `unchanged` already had them,
    /// `failed` threw; `succeeded` is added + unchanged.
    public struct BatchResult: Sendable {
        public let added: Int
        public let unchanged: Int
        public let failed: Int
        public let firstError: String?

        public var succeeded: Int { added + unchanged }
    }

    public static func addTagsBulk(_ tags: [String], to urls: [URL]) -> BatchResult {
        // journal: nil keeps Cleanup and People tagging out of the Library "Undo last tags" journal, which is reserved for the bulk-tag sheet.
        let detailed = addTagsBulkDetailed(tags, to: urls, journal: nil)
        return BatchResult(added: detailed.outcomes.count, unchanged: detailed.unchanged,
                           failed: detailed.failed, firstError: detailed.firstError)
    }

    /// One modified file in a detailed bulk run: the exact tags newly added (after − before, case-insensitive), so undo removes only what FileID applied.
    public struct TagOutcome: Codable, Sendable, Equatable {
        public let path: String
        public let addedTags: [String]
        /// Identity captured at apply time (nil in entries from older builds). Undo skips an entry on mismatch so a replacement file's tags are never stripped.
        public let fileSize: Int64?
        public let modifiedAt: Date?
        /// When the batch was applied; undo ignores entries older than `undoJournalMaxAge`.
        public let appliedAt: Date?

        public init(path: String, addedTags: [String]) {
            self.path = path
            self.addedTags = addedTags
            self.fileSize = nil
            self.modifiedAt = nil
            self.appliedAt = nil
        }

        /// Captures the file's identity and an apply timestamp so undo can verify the same file is still there.
        init(capturing path: String, addedTags: [String], at appliedAt: Date = Date()) {
            self.path = path
            self.addedTags = addedTags
            let attrs = try? FileManager.default.attributesOfItem(atPath: path)
            self.fileSize = attrs?[.size] as? Int64
            self.modifiedAt = attrs?[.modificationDate] as? Date
            self.appliedAt = appliedAt
        }
    }

    public struct DetailedBatchResult: Sendable {
        /// Only files that were actually modified.
        public let outcomes: [TagOutcome]
        public let unchanged: Int
        public let failed: Int
        public let firstError: String?

        public var added: Int { outcomes.count }
        public var succeeded: Int { added + unchanged }
    }

    public static func addTagsBulkDetailed(_ tags: [String], to urls: [URL],
                                           journal: UserDefaults? = .standard) -> DetailedBatchResult {
        var outcomes: [TagOutcome] = []
        var unchanged = 0
        var failed = 0
        var firstError: String?
        let appliedAt = Date()
        for url in urls {
            do {
                let before = readTags(at: url)
                let after = try addTags(tags, at: url)
                if after == before {
                    unchanged += 1
                } else {
                    let beforeLower = Set(before.map { $0.lowercased() })
                    let newOnes = after.filter { !beforeLower.contains($0.lowercased()) }
                    outcomes.append(TagOutcome(capturing: url.path, addedTags: newOnes, at: appliedAt))
                }
            } catch {
                failed += 1
                if firstError == nil {
                    firstError = error.localizedDescription
                }
            }
        }
        // Rewrite on every batch, even an all-unchanged one (empty outcomes clears the journal): otherwise undo strips tags from an earlier batch's files.
        if let journal {
            recordUndoJournal(outcomes, in: journal)
        }
        return DetailedBatchResult(outcomes: outcomes, unchanged: unchanged,
                                   failed: failed, firstError: firstError)
    }

    // Undo journal: the last bulk-tag batch, kept in TagWriter so it is rewritten in lock-step with the batch that produced it.

    /// UserDefaults key for the bulk-tag undo journal. Must equal `BulkTagSheet.lastBatchKey`, which reads the same journal.
    public static let undoJournalKey = "bulkTag.lastBatch.v1"

    /// Undo ignores entries older than this: an old batch is likelier to hit a same-named replacement file than to be intentional.
    public static let undoJournalMaxAge: TimeInterval = 7 * 24 * 60 * 60

    /// Overwrites the undo journal with this batch's outcomes; an empty batch clears it.
    static func recordUndoJournal(_ outcomes: [TagOutcome], in defaults: UserDefaults = .standard) {
        if outcomes.isEmpty {
            defaults.removeObject(forKey: undoJournalKey)
        } else if let data = try? JSONEncoder().encode(outcomes) {
            defaults.set(data, forKey: undoJournalKey)
        }
    }

    /// Undo a previous detailed bulk apply: removes only the recorded added tags from each file. Absent tags count as undone.
    public static func undoBulkAdd(_ outcomes: [TagOutcome])
        -> (undone: Int, skipped: Int, failed: Int, firstError: String?) {
        var undone = 0
        var skipped = 0
        var failed = 0
        var firstError: String?
        let fm = FileManager.default
        let now = Date()
        for outcome in outcomes {
            // Age guard: a stale journal is likelier to hit a replacement file than to be an intentional undo.
            if let applied = outcome.appliedAt,
               now.timeIntervalSince(applied) > undoJournalMaxAge {
                skipped += 1
                continue
            }
            // Identity guard: a different file at this path would be mangled. Entries without identity (older builds) are skipped too.
            guard let size = outcome.fileSize, let mtime = outcome.modifiedAt else {
                skipped += 1
                continue
            }
            let attrs = try? fm.attributesOfItem(atPath: outcome.path)
            guard let curSize = attrs?[.size] as? Int64,
                  let curMtime = attrs?[.modificationDate] as? Date,
                  curSize == size,
                  abs(curMtime.timeIntervalSince(mtime)) < 1
            else {
                skipped += 1
                continue
            }
            do {
                try removeTags(outcome.addedTags,
                               at: URL(fileURLWithPath: outcome.path))
                undone += 1
            } catch {
                failed += 1
                if firstError == nil {
                    firstError = error.localizedDescription
                }
            }
        }
        return (undone, skipped, failed, firstError)
    }


    /// Case-insensitive merge that keeps the existing capitalization: "Mom" plus "mom" stays "Mom".
    public static func mergeTags(existing: [String], adding new: [String]) -> [String] {
        var out = existing
        var lowerSeen = Set(existing.map { $0.lowercased() })
        for tag in new {
            let trimmed = tag.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { continue }
            if lowerSeen.insert(trimmed.lowercased()).inserted {
                out.append(trimmed)
            }
        }
        return out
    }
}
