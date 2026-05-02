// FolderClassifier — decides whether each source folder under a library
// root is an Anchor (meaningfully named, content is homogeneous), Mixed
// (meaningful name, mostly homogeneous with outliers), or Junk (generic
// or random-named, content is heterogeneous).
//
// Pure-Swift, no UI, no I/O. Takes a list of files-per-folder and the
// known persons table; returns one classification per folder. Tested
// independently of the SwiftUI Restructure view.
//
// Three signals decide a folder's tier:
//   1. Name pattern: known person, year/month, generic-junk denylist,
//      or "explicit meaningful name" (anything else — user-given).
//   2. Content homogeneity: fraction of files matching the dominant
//      person / year. Threshold defaults: 85 % homogeneous = anchor,
//      60-85 % = mixed, < 60 % = junk regardless of name.
//   3. The user's explicit naming intent: if the user named a folder
//      after a person, we keep that folder even when content is split
//      (we don't second-guess intent — we just dissolve outliers).
import Foundation

public enum FolderClassifier {

    // MARK: - Result types

    /// Why a folder is meaningful (or NIL if its name is junk).
    public enum NameReason: Sendable, Equatable {
        case namedPerson(personID: Int64, displayName: String)
        case timeYear(Int)
        case timeMonthYear(month: Int, year: Int)
        case explicitMeaningfulName    // not junk, not a known pattern
    }

    public enum DominantSignal: Sendable, Equatable {
        case person(personID: Int64, displayName: String, fraction: Double)
        case year(Int, fraction: Double)
        case none
    }

    /// Final tier decision.
    public enum Tier: Sendable, Equatable {
        /// Keep the folder + all its files exactly where they are.
        case anchor(reason: NameReason)
        /// Keep the folder name + the matching files. Outliers move out
        /// into their own anchor buckets.
        case mixed(reason: NameReason, dominant: DominantSignal,
                   outlierFileIDs: Set<Int64>)
        /// Dissolve the folder. Every file re-buckets via the heuristic.
        case junk
    }

    public struct Classification: Sendable {
        public let folderPath: String
        public let folderName: String
        public let fileCount: Int
        public let tier: Tier
    }

    // MARK: - Tunables

    /// Below this homogeneity, even a meaningfully-named folder gets
    /// dissolved. Pretty name with no underlying pattern → junk.
    static let minAnchorHomogeneity: Double = 0.60
    /// At or above this, the folder is purely an Anchor — no outliers.
    static let pureAnchorHomogeneity: Double = 0.85

    /// Folder names that are obvious "this folder isn't organized."
    /// Compared case-insensitively after normalization. Substring match
    /// (so "Untitled folder 12" still trips). Curated list — add
    /// patterns sparingly to avoid false positives.
    static let junkNamePatterns: [String] = [
        "untitled folder", "untitled", "new folder", "new",
        "camera roll", "dcim", "imports", "import",
        "screenshots", "screenshot", "downloads", "download",
        "temp", "tmp", "to sort", "to organize",
        "misc", "miscellaneous", "stuff", "random",
    ]

    // MARK: - Entry point

    /// Classify every folder represented in `byFolder`.
    /// `knownPersons` come from the People table.
    /// `personLookup` is fileID → list of person displayNames touching that file.
    public static func classifyAll(
        byFolder: [String: [FileMeta]],
        knownPersons: [KnownPerson],
        personLookup: [Int64: [String]]
    ) -> [Classification] {
        byFolder.map { (path, files) -> Classification in
            let folderName = URL(fileURLWithPath: path).lastPathComponent
            let tier = classifyOne(folderName: folderName, files: files,
                                    knownPersons: knownPersons,
                                    personLookup: personLookup)
            return Classification(folderPath: path,
                                  folderName: folderName,
                                  fileCount: files.count,
                                  tier: tier)
        }
        .sorted { $0.folderPath < $1.folderPath }
    }

    /// Single-folder classifier. Public for testability.
    public static func classifyOne(
        folderName: String,
        files: [FileMeta],
        knownPersons: [KnownPerson],
        personLookup: [Int64: [String]]
    ) -> Tier {
        // Step 1: classify by name. nil → name is junk.
        let nameReason = classifyName(folderName, knownPersons: knownPersons)
        guard let reason = nameReason else { return .junk }

        // Step 2: examine content. Find dominant person / year + homogeneity.
        let signal = dominantSignal(for: files, personLookup: personLookup)
        let homogeneity = signalFraction(signal)

        // Step 3: combine into tier.
        switch reason {
        case .timeYear, .timeMonthYear:
            // User explicitly named this a year folder; trust them.
            return .anchor(reason: reason)

        case .namedPerson(_, let displayName):
            // Person-named folder. The dominant signal SHOULD also be that
            // person; if it isn't, we still respect the user's naming
            // intent but classify as Mixed so outliers move out.
            if homogeneity >= pureAnchorHomogeneity,
               case .person(_, let dom, _) = signal,
               dom.caseInsensitiveCompare(displayName) == .orderedSame {
                return .anchor(reason: reason)
            }
            // Outliers = files that are NOT this folder's person.
            let outliers = filesNotMatchingPerson(displayName, files: files,
                                                    personLookup: personLookup)
            if !outliers.isEmpty, homogeneity >= minAnchorHomogeneity {
                return .mixed(reason: reason, dominant: signal,
                              outlierFileIDs: outliers)
            }
            // Fewer than 60% match the named person. Trust the name —
            // anchor with no outlier set; user can dissolve manually.
            return .anchor(reason: reason)

        case .explicitMeaningfulName:
            // Generic "looks meaningful" name. Rely entirely on content.
            if homogeneity >= pureAnchorHomogeneity {
                return .anchor(reason: reason)
            }
            if homogeneity >= minAnchorHomogeneity, case .person = signal {
                let outliers = filesNotMatchingDominant(signal, files: files,
                                                         personLookup: personLookup)
                return .mixed(reason: reason, dominant: signal,
                              outlierFileIDs: outliers)
            }
            // Pretty name but heterogeneous content — junk.
            return .junk
        }
    }

    // MARK: - Name classifier

    static func classifyName(
        _ name: String,
        knownPersons: [KnownPerson]
    ) -> NameReason? {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let lower = trimmed.lowercased()
        guard !lower.isEmpty else { return nil }

        // 1. Junk denylist (fast reject)
        for junk in junkNamePatterns {
            if lower == junk { return nil }
            if lower.hasPrefix("\(junk) ") { return nil }
            if lower.hasPrefix("\(junk)-") { return nil }
            if lower.hasPrefix("\(junk)_") { return nil }
        }
        // "(1)", "(copy)", " - copy", " 2" suffixes — finder duplication
        if lower.contains("(1)") || lower.contains("(2)") ||
           lower.contains("(copy)") || lower.contains(" - copy") ||
           lower.hasSuffix(" copy") {
            return nil
        }

        // 2. Year-only?
        if let year = Int(trimmed), year >= 1900 && year <= 2100 {
            return .timeYear(year)
        }
        // "2019-05", "2019_05", "2019.05"
        if let ym = parseYearMonth(trimmed) {
            return .timeMonthYear(month: ym.month, year: ym.year)
        }

        // 3. Known person?
        for p in knownPersons {
            let pn = p.displayName.lowercased()
            guard !pn.isEmpty else { continue }
            // Exact match OR folder-contains-name OR name-contains-folder.
            // Helps with "Marie Curie's Laboratory" vs. "Marie Curie".
            if lower == pn || lower.contains(pn) || pn.contains(lower) {
                return .namedPerson(personID: p.id, displayName: p.displayName)
            }
        }

        // 4. Anything else: meaningful name, content decides
        return .explicitMeaningfulName
    }

    static func parseYearMonth(_ s: String) -> (year: Int, month: Int)? {
        // "2019-05", "2019_05", "2019.05", "2019/05"
        let parts = s.split(whereSeparator: { "-_./".contains($0) })
        guard parts.count == 2,
              let year = Int(parts[0]), year >= 1900 && year <= 2100,
              let month = Int(parts[1]), month >= 1 && month <= 12
        else { return nil }
        return (year, month)
    }

    // MARK: - Content homogeneity

    /// Find the strongest signal in a folder's files. Tries person
    /// dominance first (most useful for Restructure), then year. Returns
    /// `.none` if nothing has enough mass.
    static func dominantSignal(
        for files: [FileMeta],
        personLookup: [Int64: [String]]
    ) -> DominantSignal {
        guard !files.isEmpty else { return .none }
        // Person dominance: which person name appears in the most files?
        var personFiles: [String: Int] = [:]
        for f in files {
            if let names = personLookup[f.id], let first = names.first {
                personFiles[first, default: 0] += 1
            }
        }
        if let (name, count) = personFiles.max(by: { $0.value < $1.value }) {
            let fraction = Double(count) / Double(files.count)
            if fraction >= minAnchorHomogeneity {
                return .person(personID: 0, displayName: name, fraction: fraction)
            }
        }
        // Year dominance: which year has the most files?
        var yearCounts: [Int: Int] = [:]
        let cal = Calendar(identifier: .gregorian)
        for f in files {
            guard let date = f.date else { continue }
            yearCounts[cal.component(.year, from: date), default: 0] += 1
        }
        if let (year, count) = yearCounts.max(by: { $0.value < $1.value }) {
            let fraction = Double(count) / Double(files.count)
            if fraction >= minAnchorHomogeneity {
                return .year(year, fraction: fraction)
            }
        }
        return .none
    }

    static func signalFraction(_ s: DominantSignal) -> Double {
        switch s {
        case .person(_, _, let f): return f
        case .year(_, let f):       return f
        case .none:                 return 0
        }
    }

    /// Files that don't match the folder's named person — these are the
    /// outliers in a Mixed-tier folder.
    static func filesNotMatchingPerson(
        _ displayName: String,
        files: [FileMeta],
        personLookup: [Int64: [String]]
    ) -> Set<Int64> {
        var out: Set<Int64> = []
        let target = displayName.lowercased()
        for f in files {
            let names = personLookup[f.id] ?? []
            let hit = names.contains { $0.lowercased() == target }
            if !hit { out.insert(f.id) }
        }
        return out
    }

    static func filesNotMatchingDominant(
        _ signal: DominantSignal,
        files: [FileMeta],
        personLookup: [Int64: [String]]
    ) -> Set<Int64> {
        switch signal {
        case .person(_, let name, _):
            return filesNotMatchingPerson(name, files: files,
                                            personLookup: personLookup)
        case .year(let year, _):
            let cal = Calendar(identifier: .gregorian)
            var out: Set<Int64> = []
            for f in files {
                guard let date = f.date else { out.insert(f.id); continue }
                if cal.component(.year, from: date) != year {
                    out.insert(f.id)
                }
            }
            return out
        case .none:
            return Set(files.map(\.id))
        }
    }
}

// MARK: - Plain DTOs the classifier consumes
//
// Decoupled from FileRow / PersonRow so the classifier can be unit-
// tested without an open SQLite connection.

public struct FileMeta: Sendable, Equatable {
    public let id: Int64
    public let date: Date?
    public init(id: Int64, date: Date?) {
        self.id = id
        self.date = date
    }
}

public struct KnownPerson: Sendable, Equatable {
    public let id: Int64
    public let displayName: String
    public init(id: Int64, displayName: String) {
        self.id = id
        self.displayName = displayName
    }
}
