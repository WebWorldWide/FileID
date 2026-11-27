import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Read-only gate and folder restore")
struct ReadOnlyGateTests {
    private func root(_ path: String, readOnly: Bool?) -> LibraryRoot {
        LibraryRoot(id: 1, path: path, display: URL(fileURLWithPath: path).lastPathComponent, kind: "external", enabled: true, online: true,
                    fileCount: 1, totalBytes: 1, lastScanAt: nil, lastStatus: nil, readOnly: readOnly)
    }

    @Test func aControlIsOffOnlyWhenEveryTargetIsReadOnly() {
        let gate = ReadOnlyGate([root("/Volumes/Backup", readOnly: true)])
        #expect(gate.reasonIfAll(["/Volumes/Backup/a.jpg", "/Volumes/Backup/b.jpg"])?.contains("\u{201C}Backup\u{201D} is a read-only drive") == true)
        #expect(gate.reasonIfAll(["/Volumes/Backup/a.jpg", "/Users/me/b.jpg"]) == nil)
        #expect(gate.reasonIfAll([String]()) == nil)
        #expect(gate.drive(for: "/Volumes/Backup2/a.jpg") == nil)
    }

    @Test func theAdlonNameGuardStaysEvenWhenTheEngineDoesNotReportIt() {
        let gate = ReadOnlyGate([root("/Volumes/Photos", readOnly: false)])
        #expect(gate.drive(for: "/Volumes/Adlon/x.jpg") == "Adlon")
        #expect(gate.drive(for: "/Volumes/Photos/x.jpg") == nil)
    }

    @Test func aRelaunchWithTheDriveUnpluggedKeepsTheSavedFolder() throws {
        let suite = "restore-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        defaults.set(Data([1, 2, 3]), forKey: "folder")
        let none = PickedFolderRestore.accessibleFolder(defaults: defaults, key: "folder", retain: { _ in nil }, isDirectory: { _ in true })
        #expect(none == nil)
        #expect(defaults.data(forKey: "folder") != nil)
        let gone = PickedFolderRestore.accessibleFolder(defaults: defaults, key: "folder", retain: { _ in URL(fileURLWithPath: "/Volumes/Adlon") }, isDirectory: { _ in false })
        #expect(gone == nil)
        #expect(defaults.data(forKey: "folder") != nil)
        let back = PickedFolderRestore.accessibleFolder(defaults: defaults, key: "folder", retain: { _ in URL(fileURLWithPath: "/Volumes/Adlon") }, isDirectory: { _ in true })
        #expect(back?.path == "/Volumes/Adlon")
    }
}
