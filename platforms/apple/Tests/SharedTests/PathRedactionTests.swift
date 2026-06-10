// Port of the Windows engine's redaction_tests (platform.rs): the
// passthrough must be anchored to FileID's own state root — an
// arbitrary user path embedding "Application Support" must redact.
import Testing
import Foundation
@testable import FileIDShared

@Suite("redactPathForLog")
struct PathRedactionTests {

    private var stateRoot: String {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first!.appendingPathComponent("FileID", isDirectory: true).path
    }

    @Test("own state tree passes through verbatim")
    func ownTreePassesThrough() {
        let p = stateRoot + "/Models/sface/weights.onnx"
        #expect(redactPathForLog(p) == p)
        #expect(redactPathForLog(stateRoot) == stateRoot)
    }

    @Test("sibling dir that merely starts with the root name redacts")
    func siblingPrefixRedacts() {
        let r = redactPathForLog(stateRoot + "Backup/secret/file.jpg")
        #expect(r == "…/secret/file.jpg")
        #expect(!r.contains("FileIDBackup"))
    }

    @Test("a user path embedding 'Application Support' still redacts")
    func embeddedSubstringRedacts() {
        let r = redactPathForLog("/Volumes/NAS/Backups/Library/Application Support/Photos/IMG.jpg")
        #expect(r == "…/Photos/IMG.jpg")
        #expect(!r.contains("NAS"))
    }

    @Test("deep user path keeps last two components only")
    func deepUserPath() {
        let r = redactPathForLog("/Users/adam/Pictures/Vacation/IMG.jpg")
        #expect(r == "…/Vacation/IMG.jpg")
        #expect(!r.contains("adam"))
    }

    @Test("empty input collapses to ellipsis")
    func emptyInput() {
        #expect(redactPathForLog("") == "…")
    }
}
