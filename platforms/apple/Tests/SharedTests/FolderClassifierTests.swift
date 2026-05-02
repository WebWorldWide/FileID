// Unit tests for FolderClassifier — the 3-tier (Anchor / Mixed / Junk)
// folder classification logic that drives Restructure.
//
// Tests the classifier in isolation with synthetic FileMeta + KnownPerson
// inputs. No SQLite, no SwiftUI, no I/O.
import Testing
@testable import FileIDShared
import Foundation

private func file(_ id: Int64, year: Int? = nil, month: Int? = 1) -> FileMeta {
    let date: Date? = year.map { y -> Date in
        var c = DateComponents()
        c.year = y; c.month = month; c.day = 1
        return Calendar(identifier: .gregorian).date(from: c) ?? .distantPast
    }
    return FileMeta(id: id, date: date)
}

private let einstein = KnownPerson(id: 1, displayName: "Albert Einstein")
private let curie    = KnownPerson(id: 2, displayName: "Marie Curie")
private let tesla    = KnownPerson(id: 3, displayName: "Nikola Tesla")
private let people   = [einstein, curie, tesla]

@Suite("FolderClassifier — name classification")
struct NameClassifierTests {

    @Test("year-only folder name → timeYear anchor")
    func yearOnly() {
        let r = FolderClassifier.classifyName("2019", knownPersons: [])
        #expect(r == .timeYear(2019))
    }

    @Test("year-month folder name → timeMonthYear anchor")
    func yearMonth() {
        let r = FolderClassifier.classifyName("2019-05", knownPersons: [])
        #expect(r == .timeMonthYear(month: 5, year: 2019))
    }

    @Test("known person name → namedPerson anchor")
    func knownPerson() {
        let r = FolderClassifier.classifyName("Albert Einstein", knownPersons: people)
        if case .namedPerson(_, let name) = r {
            #expect(name == "Albert Einstein")
        } else {
            Issue.record("expected namedPerson, got \(String(describing: r))")
        }
    }

    @Test("folder name CONTAINING person → namedPerson")
    func nameContaining() {
        // "Marie Curie's Laboratory" should match person "Marie Curie".
        let r = FolderClassifier.classifyName("Marie Curie's Laboratory", knownPersons: people)
        if case .namedPerson(_, let name) = r {
            #expect(name == "Marie Curie")
        } else {
            Issue.record("expected namedPerson, got \(String(describing: r))")
        }
    }

    @Test("junk denylist returns nil")
    func junkPatterns() {
        for junk in ["Untitled folder", "untitled", "Camera Roll", "DCIM", "Screenshots", "New Folder"] {
            let r = FolderClassifier.classifyName(junk, knownPersons: [])
            #expect(r == nil, "expected nil for '\(junk)', got \(String(describing: r))")
        }
    }

    @Test("Finder-duplicated names are junk")
    func finderDuplicates() {
        for name in ["Photos (1)", "Holiday (copy)", "Pictures - Copy", "Album 2 copy"] {
            let r = FolderClassifier.classifyName(name, knownPersons: [])
            #expect(r == nil, "expected nil for '\(name)', got \(String(describing: r))")
        }
    }

    @Test("unrecognized but plausible name → explicitMeaningfulName")
    func explicitName() {
        let r = FolderClassifier.classifyName("Vacation Photos", knownPersons: [])
        #expect(r == .explicitMeaningfulName)
    }
}

@Suite("FolderClassifier — full tier decision")
struct TierTests {

    @Test("year-named folder is always Anchor")
    func yearAnchor() {
        // Even with mixed contents, "2019" anchors.
        let files = [file(1, year: 2018), file(2, year: 2020), file(3, year: 2017)]
        let tier = FolderClassifier.classifyOne(
            folderName: "2019",
            files: files,
            knownPersons: [],
            personLookup: [:]
        )
        if case .anchor = tier { /* ok */ } else {
            Issue.record("expected .anchor, got \(tier)")
        }
    }

    @Test("person-named folder + matching content → Anchor")
    func personAnchorPure() {
        let files = [file(1), file(2), file(3), file(4)]
        let lookup: [Int64: [String]] = [
            1: ["Albert Einstein"],
            2: ["Albert Einstein"],
            3: ["Albert Einstein"],
            4: ["Albert Einstein"],
        ]
        let tier = FolderClassifier.classifyOne(
            folderName: "Albert Einstein",
            files: files,
            knownPersons: people,
            personLookup: lookup
        )
        if case .anchor = tier { /* ok */ } else {
            Issue.record("expected .anchor, got \(tier)")
        }
    }

    @Test("person-named folder + 1 outlier (75% homogeneous) → Mixed with outlier set")
    func personMixed() {
        let files = [file(1), file(2), file(3), file(4)]
        let lookup: [Int64: [String]] = [
            1: ["Marie Curie"],
            2: ["Marie Curie"],
            3: ["Marie Curie"],
            4: ["Albert Einstein"],   // outlier
        ]
        let tier = FolderClassifier.classifyOne(
            folderName: "Marie Curie's Laboratory",
            files: files,
            knownPersons: people,
            personLookup: lookup
        )
        if case .mixed(_, _, let outliers) = tier {
            #expect(outliers == [4], "outliers should be {4}, got \(outliers)")
        } else {
            Issue.record("expected .mixed, got \(tier)")
        }
    }

    @Test("junk-named folder → Junk regardless of content")
    func junkTier() {
        // Even if all 4 files are Curie photos, "Untitled folder" stays junk.
        let files = [file(1), file(2), file(3), file(4)]
        let lookup: [Int64: [String]] = [
            1: ["Marie Curie"], 2: ["Marie Curie"],
            3: ["Marie Curie"], 4: ["Marie Curie"],
        ]
        let tier = FolderClassifier.classifyOne(
            folderName: "Untitled folder",
            files: files,
            knownPersons: people,
            personLookup: lookup
        )
        #expect(tier == .junk, "expected .junk, got \(tier)")
    }

    @Test("explicitMeaningfulName + 100% homogeneous → Anchor")
    func explicitNameAnchor() {
        let files = [file(1), file(2), file(3)]
        let lookup: [Int64: [String]] = [
            1: ["Nikola Tesla"], 2: ["Nikola Tesla"], 3: ["Nikola Tesla"],
        ]
        let tier = FolderClassifier.classifyOne(
            folderName: "Lab Photos 1894",
            files: files,
            knownPersons: people,
            personLookup: lookup
        )
        if case .anchor = tier { /* ok */ } else {
            Issue.record("expected .anchor for content-homogeneous explicit name, got \(tier)")
        }
    }

    @Test("explicitMeaningfulName + heterogeneous content → Junk")
    func explicitNameJunk() {
        // "Misc Folder" looks like nothing — content is mixed → junk.
        let files = [file(1), file(2), file(3), file(4), file(5)]
        let lookup: [Int64: [String]] = [
            1: ["Albert Einstein"], 2: ["Marie Curie"],
            3: ["Nikola Tesla"], 4: ["Albert Einstein"], 5: [],
        ]
        let tier = FolderClassifier.classifyOne(
            folderName: "Mystery Folder",
            files: files,
            knownPersons: people,
            personLookup: lookup
        )
        #expect(tier == .junk, "expected .junk, got \(tier)")
    }
}

@Suite("FolderClassifier — corpus integration shape")
struct CorpusShapeTests {
    /// Mirrors the layout `scripts/build_corpus.sh` produces. Validates
    /// the classifier returns the expected mix of tiers across the
    /// curated test corpus.
    @Test("test corpus produces 4 anchors / 1 mixed / 2 junk")
    func corpusShape() {
        let lookup: [Int64: [String]] = [
            // Albert Einstein/
            10: ["Albert Einstein"], 11: ["Albert Einstein"],
            12: ["Albert Einstein"], 13: ["Albert Einstein"],
            // Marie Curie/
            20: ["Marie Curie"], 21: ["Marie Curie"],
            // Nikola Tesla/
            30: ["Nikola Tesla"], 31: ["Nikola Tesla"], 32: ["Nikola Tesla"],
            // 2019/
            40: [],
            // Marie Curie's Laboratory/  (Mixed: 2 Curie + 1 Einstein outlier)
            50: ["Marie Curie"], 51: ["Marie Curie"], 52: ["Albert Einstein"],
            // Untitled folder/
            60: ["Albert Einstein"], 61: [], 62: ["Albert Einstein"],
            // Camera Roll/
            70: [], 71: ["Marie Curie"],
        ]
        let byFolder: [String: [FileMeta]] = [
            "/lib/Albert Einstein":         [file(10), file(11), file(12), file(13)],
            "/lib/Marie Curie":             [file(20), file(21)],
            "/lib/Nikola Tesla":            [file(30), file(31), file(32)],
            "/lib/2019":                    [file(40, year: 2019)],
            "/lib/Marie Curie's Laboratory":[file(50), file(51), file(52)],
            "/lib/Untitled folder":         [file(60), file(61), file(62)],
            "/lib/Camera Roll":             [file(70), file(71)],
        ]
        let cs = FolderClassifier.classifyAll(
            byFolder: byFolder,
            knownPersons: people,
            personLookup: lookup
        )
        var anchors = 0, mixed = 0, junk = 0
        for c in cs {
            switch c.tier {
            case .anchor: anchors += 1
            case .mixed:  mixed   += 1
            case .junk:   junk    += 1
            }
        }
        #expect(anchors == 4, "expected 4 anchor folders, got \(anchors)")
        #expect(mixed   == 1, "expected 1 mixed folder, got \(mixed)")
        #expect(junk    == 2, "expected 2 junk folders, got \(junk)")
    }
}
