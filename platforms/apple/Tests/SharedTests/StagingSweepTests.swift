// Pins the C4 fix: parts orphaned in .fileid-staging by a dead process
// are reclaimed on the next download, without ever touching entries an
// in-flight download (this process, or a live sibling process) owns.
import Testing
import Foundation
@testable import FileIDShared

@Suite("sweepStaleStagingEntries")
struct StagingSweepTests {

    private func makeStagingDir() throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-sweep-\(UUID().uuidString)", isDirectory: true)
            .appendingPathComponent(".fileid-staging", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private func cleanup(_ stagingDir: URL) {
        try? FileManager.default.removeItem(at: stagingDir.deletingLastPathComponent())
    }

    @discardableResult
    private func touch(_ dir: URL, _ name: String, age: TimeInterval = 0) throws -> URL {
        let url = dir.appendingPathComponent(name)
        try Data("x".utf8).write(to: url)
        if age > 0 {
            try FileManager.default.setAttributes(
                [.modificationDate: Date().addingTimeInterval(-age)],
                ofItemAtPath: url.path)
        }
        return url
    }

    private func exists(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.path)
    }

    @Test("dead-pid entries are removed, parts and final alike")
    func deadPidRemoved() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let part = try touch(dir, "4242-AAAA-part-0")
        let final = try touch(dir, "4242-AAAA-final")
        sweepStaleStagingEntries(in: dir, currentPID: 1000,
                                 isProcessAlive: { _ in false })
        #expect(!exists(part))
        #expect(!exists(final))
    }

    @Test("current process's entries always survive")
    func currentPidKept() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let part = try touch(dir, "4242-AAAA-part-3", age: 100 * 60 * 60)
        sweepStaleStagingEntries(in: dir, currentPID: 4242,
                                 isProcessAlive: { _ in false })
        #expect(exists(part))
    }

    @Test("live foreign pid with fresh entries survives")
    func liveForeignPidKept() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let part = try touch(dir, "777-BBBB-part-1")
        sweepStaleStagingEntries(in: dir, currentPID: 1000,
                                 isProcessAlive: { _ in true })
        #expect(exists(part))
    }

    @Test("live foreign pid older than maxAge is removed — pid reuse guard")
    func liveForeignPidAgedOut() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let part = try touch(dir, "777-BBBB-part-1", age: 3600)
        sweepStaleStagingEntries(in: dir, currentPID: 1000, maxAge: 60,
                                 isProcessAlive: { _ in true })
        #expect(!exists(part))
    }

    @Test("legacy un-prefixed entries are removed")
    func legacyEntriesRemoved() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let part = try touch(dir, "DEADBEEF-1234-part-0")
        sweepStaleStagingEntries(in: dir, currentPID: 1000,
                                 isProcessAlive: { _ in true })
        #expect(!exists(part))
    }

    @Test("only stale entries go; in-flight neighbors stay")
    func mixedEntries() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let stale = try touch(dir, "31337-CCCC-part-0")
        let mine = try touch(dir, "1000-DDDD-part-0")
        let live = try touch(dir, "888-EEEE-part-0")
        sweepStaleStagingEntries(in: dir, currentPID: 1000,
                                 isProcessAlive: { $0 == 888 })
        #expect(!exists(stale))
        #expect(exists(mine))
        #expect(exists(live))
    }

    @Test("missing staging dir is a no-op")
    func missingDirNoop() {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-sweep-missing-\(UUID().uuidString)/.fileid-staging")
        sweepStaleStagingEntries(in: dir, isProcessAlive: { _ in false })
        #expect(!exists(dir))
    }
}
