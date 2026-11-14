// Parts orphaned in .fileid-staging by a dead process
// are reclaimed on the next download, without ever touching entries an
// in-flight download (this process, or a live sibling process) owns.
// Finished pieces of a hash-pinned download are the exception: a later
// launch reuses them, so they go only when stale or unreadable.
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

    @Test("a fresh resumable folder survives the sweep")
    func freshResumableKept() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let staging = try #require(openResumable(dir))
        try Data(count: 4).write(to: staging.pieceURL(0))
        staging.unlock()
        sweepStaleStagingEntries(in: dir, currentPID: 1000, isProcessAlive: { _ in false })
        #expect(exists(staging.pieceURL(0)))
    }

    @Test("a resumable folder untouched for longer than the limit is removed")
    func staleResumableRemoved() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let staging = try #require(openResumable(dir))
        try Data(count: 4).write(to: staging.pieceURL(0))
        staging.unlock()
        try age(staging.folder, by: 8 * 24 * 60 * 60)
        sweepStaleStagingEntries(in: dir, currentPID: 1000, isProcessAlive: { _ in false })
        #expect(!exists(staging.folder))
    }

    @Test("a resumable folder a download is using survives even when old")
    func lockedResumableKept() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let staging = try #require(openResumable(dir))
        defer { staging.unlock() }
        try Data(count: 4).write(to: staging.pieceURL(0))
        try age(staging.folder, by: 8 * 24 * 60 * 60)
        sweepStaleStagingEntries(in: dir, currentPID: 1000, isProcessAlive: { _ in false })
        #expect(exists(staging.pieceURL(0)))
    }

    @Test("a resumable folder without a readable manifest is removed")
    func resumableWithoutManifestRemoved() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let folder = dir.appendingPathComponent("resume-0000000000000000-0000000000000000", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try touch(folder, "piece-0")
        sweepStaleStagingEntries(in: dir, currentPID: 1000, isProcessAlive: { _ in true })
        #expect(!exists(folder))
    }

    private func openResumable(_ dir: URL) -> ResumableStaging? {
        ResumableStaging.open(
            stagingDir: dir, dest: dir.deletingLastPathComponent().appendingPathComponent("model.bin"),
            sha256: String(repeating: "ab", count: 32), totalBytes: 8, pieceBytes: 4, pieceCount: 2)
    }

    private func age(_ folder: URL, by seconds: TimeInterval) throws {
        let fm = FileManager.default
        let then = Date().addingTimeInterval(-seconds)
        for name in try fm.contentsOfDirectory(atPath: folder.path) {
            try fm.setAttributes([.modificationDate: then],
                                 ofItemAtPath: folder.appendingPathComponent(name).path)
        }
        try fm.setAttributes([.modificationDate: then], ofItemAtPath: folder.path)
    }

    @Test("missing staging dir is a no-op")
    func missingDirNoop() {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-sweep-missing-\(UUID().uuidString)/.fileid-staging")
        sweepStaleStagingEntries(in: dir, isProcessAlive: { _ in false })
        #expect(!exists(dir))
    }
}
