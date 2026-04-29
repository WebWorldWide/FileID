import XCTest
@testable import FileID

final class JunkScorerTests: XCTestCase {

    /// Build a FileRecord without touching disk — pass synthetic creation date
    /// and size so init doesn't fall back to a `resourceValues` read.
    /// Default URL deliberately avoids /tmp/ (which JunkScorer flags as a
    /// junk directory) so the "not junk" assertions don't accidentally
    /// trigger the path heuristic.
    private func makeRecord(
        url: URL = URL(fileURLWithPath: "/Users/test/Photos/synthetic.jpg"),
        creationDate: Date = Date(),
        fileSizeMB: Double = 1.0,
        tags: [String] = [],
        hasFaces: Bool = false,
        aestheticScore: Double = 0.5,
        duplicate: Bool = false
    ) -> FileRecord {
        let r = FileRecord(
            url: url,
            status: .completed,
            creationDate: creationDate,
            fileSizeBytes: Int(fileSizeMB * 1_048_576)
        )
        r.aiTags             = tags
        r.hasFaces           = hasFaces
        r.aestheticScore     = aestheticScore
        r.duplicateGroupUUID = duplicate ? UUID() : nil
        return r
    }

    func testFreshPhotoIsNotJunk() {
        // 2 MB recent photo with no junk markers should score well below
        // the threshold.
        let r = makeRecord(fileSizeMB: 2.0, aestheticScore: 0.7)
        let (score, reasons) = JunkScorer.score(r)
        XCTAssertLessThan(score, JunkScorer.junkThreshold)
        XCTAssertTrue(reasons.isEmpty,
                      "Reasons should only populate when score crosses threshold.")
    }

    func testZeroByteFileFlaggedAsJunk() {
        let r = makeRecord(fileSizeMB: 0.0)
        let (score, reasons) = JunkScorer.score(r)
        XCTAssertGreaterThanOrEqual(score, JunkScorer.junkThreshold)
        XCTAssertFalse(reasons.isEmpty,
                       "An empty file must surface at least one reason.")
        XCTAssertTrue(reasons.contains { $0.contains("Empty") },
                      "Reasons should mention emptiness.")
    }

    func testCacheTaggedFileFlagged() {
        // A small thumbnail-cache file with a .cache extension in
        // /Library/Caches/ should hit multiple junk signals (path + tag +
        // size + extension), comfortably crossing the threshold.
        let r = makeRecord(
            url: URL(fileURLWithPath: "/Users/x/Library/Caches/Thumbs/x.cache"),
            fileSizeMB: 0.02,
            tags: ["Thumbnail", "Cache"]
        )
        let (score, _) = JunkScorer.score(r)
        XCTAssertGreaterThanOrEqual(score, JunkScorer.junkThreshold,
                                    "Score was \(score) — expected ≥ \(JunkScorer.junkThreshold).")
    }

    func testFacesSoftenButDoNotVeto() {
        // A blurry low-aesthetic empty file with faces should still be junk
        // (faces *= 0.65 is a soft penalty, not a veto).
        let r = makeRecord(
            fileSizeMB: 0.0,
            hasFaces: true,
            aestheticScore: 0.1
        )
        let (score, _) = JunkScorer.score(r)
        XCTAssertGreaterThanOrEqual(score, JunkScorer.junkThreshold,
                                    "hasFaces should soften but not veto an empty-file flag.")
    }

    func testScoreClampedToOne() {
        // Stack every junk signal — score must never exceed 1.0.
        let r = makeRecord(
            url: URL(fileURLWithPath: "/tmp/.thumbnails/empty.cache"),
            creationDate: Date(timeIntervalSinceNow: -10 * 365 * 86400),
            fileSizeMB: 0.0,
            tags: ["Thumbnail", "Cache", "Temp"],
            aestheticScore: 0.05,
            duplicate: true
        )
        let (score, _) = JunkScorer.score(r)
        XCTAssertLessThanOrEqual(score, 1.0)
    }
}
