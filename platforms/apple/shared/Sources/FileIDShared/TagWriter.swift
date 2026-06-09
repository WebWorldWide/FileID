// TagWriter — writes macOS Finder tags onto files via the standard
// `URLResourceKey.tagNamesKey` API.
//
// Finder tags are an Apple-blessed, system-wide tag mechanism that
// shows up everywhere macOS shows tags: Finder sidebar, Spotlight
// (`tag:Mom` queries), Smart Folders, Notes, Mail. They survive moves
// across volumes that support extended attributes.
//
// Writes are reversible — `setTags` overwrites, so the user can clear
// FileID's tags by passing an empty list. Finder tags stay the source
// of truth (editable in Finder if FileID is ever uninstalled); the only
// journal is the UI's last-batch undo record built from `TagOutcome`s,
// which captures the exact per-file diff so undo removes ONLY what
// FileID added — never the user's own tags.
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

    /// Read the Finder tags currently on a file. Returns an empty array
    /// if the file has none (or if the read fails — see throwing variant
    /// `readTagsThrowing` if you need to distinguish).
    public static func readTags(at url: URL) -> [String] {
        (try? readTagsThrowing(at: url)) ?? []
    }

    public static func readTagsThrowing(at url: URL) throws -> [String] {
        let values: URLResourceValues
        do {
            values = try url.resourceValues(forKeys: [.tagNamesKey])
        } catch {
            throw Error.readFailed("\(url.lastPathComponent): \(error.localizedDescription)")
        }
        return values.tagNames ?? []
    }

    /// Replace the file's Finder tags with `tags` exactly. Empty array
    /// clears all tags. Idempotent — same tags as already set is a no-op.
    ///
    /// Uses `NSURL.setResourceValue(_:forKey:)` rather than the
    /// `URLResourceValues.tagNames` setter, which is macOS 26-only.
    public static func setTags(_ tags: [String], at url: URL) throws {
        // Normalize: trim whitespace, drop empties, dedupe preserving order.
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

    /// Add the supplied tags to whatever the file already has. Existing
    /// tags from other tools (or from the user's own Finder sessions)
    /// are preserved. This is the right call for "tag this person's
    /// photos" — never blow away a user-applied "Important" tag.
    @discardableResult
    public static func addTags(_ tags: [String], at url: URL) throws -> [String] {
        let existing = readTags(at: url)
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
        let existing = readTags(at: url)
        let toRemove = Set(tags.map { $0.lowercased() })
        let kept = existing.filter { !toRemove.contains($0.lowercased()) }
        if kept.count != existing.count {
            try setTags(kept, at: url)
        }
        return kept
    }

    /// Bulk apply: add `tags` to every URL. Returns per-URL results so
    /// the UI can surface partial failures. Doesn't bail on first error.
    ///
    /// `added`     — files that were modified (at least one new tag applied).
    /// `unchanged` — files that already had every tag we tried to add.
    /// `failed`    — files where the write threw.
    /// `succeeded` — `added + unchanged` (kept as a convenience).
    public struct BatchResult: Sendable {
        public let added: Int
        public let unchanged: Int
        public let failed: Int
        public let firstError: String?

        public var succeeded: Int { added + unchanged }
    }

    public static func addTagsBulk(_ tags: [String], to urls: [URL]) -> BatchResult {
        let detailed = addTagsBulkDetailed(tags, to: urls)
        return BatchResult(added: detailed.outcomes.count, unchanged: detailed.unchanged,
                           failed: detailed.failed, firstError: detailed.firstError)
    }

    /// One modified file in a detailed bulk run: the exact tags newly
    /// added (after − before, case-insensitive). This is what makes undo
    /// precise — it removes only what FileID applied, never a tag the
    /// user already had.
    public struct TagOutcome: Codable, Sendable, Equatable {
        public let path: String
        public let addedTags: [String]

        public init(path: String, addedTags: [String]) {
            self.path = path
            self.addedTags = addedTags
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

    public static func addTagsBulkDetailed(_ tags: [String], to urls: [URL]) -> DetailedBatchResult {
        var outcomes: [TagOutcome] = []
        var unchanged = 0
        var failed = 0
        var firstError: String?
        for url in urls {
            do {
                let before = readTags(at: url)
                let after = try addTags(tags, at: url)
                if after == before {
                    unchanged += 1
                } else {
                    let beforeLower = Set(before.map { $0.lowercased() })
                    let newOnes = after.filter { !beforeLower.contains($0.lowercased()) }
                    outcomes.append(TagOutcome(path: url.path, addedTags: newOnes))
                }
            } catch {
                failed += 1
                if firstError == nil {
                    firstError = error.localizedDescription
                }
            }
        }
        return DetailedBatchResult(outcomes: outcomes, unchanged: unchanged,
                                   failed: failed, firstError: firstError)
    }

    /// Undo a previous detailed bulk apply: remove ONLY the recorded
    /// added tags from each file. A file whose recorded tags are already
    /// gone counts as undone (removeTags is a no-op for absent tags).
    public static func undoBulkAdd(_ outcomes: [TagOutcome])
        -> (undone: Int, failed: Int, firstError: String?) {
        var undone = 0
        var failed = 0
        var firstError: String?
        for outcome in outcomes {
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
        return (undone, failed, firstError)
    }

    // MARK: - Merging

    /// Case-insensitive merge that preserves the EXISTING capitalization
    /// when a duplicate is added. So if "Mom" exists and the caller adds
    /// "mom", the result keeps "Mom".
    public static func mergeTags(existing: [String], adding new: [String]) -> [String] {
        var out = existing
        let lowerExisting = Set(existing.map { $0.lowercased() })
        for tag in new {
            let trimmed = tag.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { continue }
            if !lowerExisting.contains(trimmed.lowercased()) {
                out.append(trimmed)
            }
        }
        return out
    }
}
