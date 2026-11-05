import Foundation
import Testing
@testable import FileIDEngine

@Suite("Changes during a scan")
struct FileSystemChangesTests {
    @Test("FSEvents history reports visible changes under the folder only")
    func reportsChangesUnderFolder() async throws {
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDChanges-\(UUID().uuidString)")
        let watched = base.appendingPathComponent("lib"), sibling = base.appendingPathComponent("other")
        try FileManager.default.createDirectory(at: watched, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: sibling, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        try Data("a".utf8).write(to: watched.appendingPathComponent("a.jpg"))
        try await Task.sleep(for: .seconds(1))

        let mark = FileSystemChanges.currentEventID()
        try Data("b".utf8).write(to: sibling.appendingPathComponent("b.jpg"))
        try Data("c".utf8).write(to: watched.appendingPathComponent(".DS_Store"))
        try await Task.sleep(for: .seconds(1))
        #expect(FileSystemChanges.changed(under: watched, since: mark) == false)

        try FileManager.default.moveItem(at: watched.appendingPathComponent("a.jpg"),
                                         to: watched.appendingPathComponent("renamed.jpg"))
        try await Task.sleep(for: .seconds(1))
        #expect(FileSystemChanges.changed(under: watched, since: mark) == true)
    }
}
