import Foundation
import Testing
@testable import FileID

@Suite("Spotlight sends only what changed")
struct SpotlightFingerprintsTests {
    private func row(_ id: Int64, path: String = "/p/a.jpg", name: String? = "Beach day",
                     description: String? = "A dog on a beach", tags: [String] = ["dog", "beach"]) -> SpotlightRow {
        SpotlightRow(id: id, path: path, kind: "image", smartName: name, description: description, tags: tags)
    }

    private func sent(_ rows: [SpotlightRow]) -> SpotlightFingerprints {
        SpotlightFingerprints(byID: Dictionary(uniqueKeysWithValues: rows.map { ($0.id, SpotlightFingerprints.fingerprint($0)) }))
    }

    @Test func unchangedRowsAreNotSentAgain() {
        let known = sent([row(1), row(2)])
        #expect(known.changed([row(1), row(2)]).isEmpty)
        #expect(known.changed([row(1, tags: ["beach", "dog"])]).isEmpty)
    }

    @Test func aChangeToAnyIndexedFieldSendsTheRow() {
        let known = sent([row(1)])
        #expect(known.changed([row(1, path: "/p/b.jpg")]).map(\.id) == [1])
        #expect(known.changed([row(1, name: "Dog")]).map(\.id) == [1])
        #expect(known.changed([row(1, description: nil)]).map(\.id) == [1])
        #expect(known.changed([row(1, tags: ["dog"])]).map(\.id) == [1])
        #expect(known.changed([row(1, tags: ["dog", "beach", "sea"])]).map(\.id) == [1])
        #expect(known.changed([row(1), row(3)]).map(\.id) == [3])
    }

    @Test func fieldsDoNotRunTogether() {
        #expect(SpotlightFingerprints.fingerprint(row(1, name: "ab", description: "c"))
                != SpotlightFingerprints.fingerprint(row(1, name: "a", description: "bc")))
    }

    @Test func rowsNoLongerInTheLibraryAreReportedGone() {
        #expect(sent([row(1), row(2), row(3)]).vanished(keeping: [1, 3]) == [2])
    }

    @Test func whatWasSentSurvivesARelaunchAndAMissingOrDamagedFileMeansNothingWasSent() throws {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-spotlight-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let url = folder.appendingPathComponent("spotlight-sent.bin")
        #expect(SpotlightFingerprints.load(from: url).byID.isEmpty)
        let known = sent([row(1), row(Int64.max - 1, tags: [])])
        try known.save(to: url)
        #expect(SpotlightFingerprints.load(from: url) == known)
        try Data("not the file".utf8).write(to: url)
        #expect(SpotlightFingerprints.load(from: url).byID.isEmpty)
    }
}
