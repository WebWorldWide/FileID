import CryptoKit
import Foundation
import Testing
@testable import FileIDShared

@Suite("ResumableStaging — finished pieces survive a relaunch")
struct ResumableStagingTests {
    private let pieces = [Data("abcd".utf8), Data("efgh".utf8), Data("ij".utf8)]

    private var wholeHash: String {
        SHA256.hash(data: pieces.reduce(Data(), +)).map { String(format: "%02x", $0) }.joined()
    }

    private func makeStagingDir() throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-resume-\(UUID().uuidString)", isDirectory: true)
            .appendingPathComponent(".fileid-staging", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private func cleanup(_ stagingDir: URL) {
        try? FileManager.default.removeItem(at: stagingDir.deletingLastPathComponent())
    }

    private func dest(_ dir: URL, _ name: String = "model.bin") -> URL {
        dir.deletingLastPathComponent().appendingPathComponent(name)
    }

    private func open(_ dir: URL, name: String = "model.bin", sha256: String? = nil,
                      totalBytes: Int64 = 10, pieceBytes: Int64 = 4) -> ResumableStaging? {
        ResumableStaging.open(stagingDir: dir, dest: dest(dir, name), sha256: sha256 ?? wholeHash,
                              totalBytes: totalBytes, pieceBytes: pieceBytes, pieceCount: 3)
    }

    private func exists(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.path)
    }

    @Test("a later launch reuses the finished pieces and assembles the verified file")
    func reuseAfterRelaunch() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let first = try #require(open(dir))
        try pieces[0].write(to: first.pieceURL(0))
        try pieces[1].write(to: first.pieceURL(1))
        first.unlock()

        sweepStaleStagingEntries(in: dir, currentPID: 1000, isProcessAlive: { _ in false })
        let second = try #require(open(dir))
        defer { second.unlock() }
        #expect(second.folder == first.folder)
        #expect(second.hasFinishedPiece(0, length: 4))
        #expect(second.hasFinishedPiece(1, length: 4))
        #expect(!second.hasFinishedPiece(2, length: 2))
        #expect(ResumableStaging.stagedBytes(in: dir, dest: dest(dir), sha256: wholeHash) == 8)

        try pieces[2].write(to: second.pieceURL(2))
        let final = dir.appendingPathComponent("1000-TEST-final")
        try assembleStagedPieces((0..<3).map(second.pieceURL), into: final, total: 10,
                                 expectedSHA256: wholeHash, approxBytes: 0,
                                 discardPieces: second.discard)
        #expect(try Data(contentsOf: final) == pieces.reduce(Data(), +))
    }

    @Test("a piece with the wrong length is not reused")
    func shortPieceRejected() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let first = try #require(open(dir))
        try Data("ab".utf8).write(to: first.pieceURL(0))
        first.unlock()
        let second = try #require(open(dir))
        defer { second.unlock() }
        #expect(!second.hasFinishedPiece(0, length: 4))
    }

    @Test("a piece with the right length but wrong bytes fails the hash and removes every piece")
    func corruptPieceRejected() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let first = try #require(open(dir))
        try Data("XXXX".utf8).write(to: first.pieceURL(0))
        try pieces[1].write(to: first.pieceURL(1))
        first.unlock()

        let second = try #require(open(dir))
        defer { second.unlock() }
        #expect(second.hasFinishedPiece(0, length: 4))
        try pieces[2].write(to: second.pieceURL(2))
        let final = dir.appendingPathComponent("1000-TEST-final")
        #expect(throws: StreamingDownloadError.self) {
            try assembleStagedPieces((0..<3).map(second.pieceURL), into: final, total: 10,
                                     expectedSHA256: wholeHash, approxBytes: 0,
                                     discardPieces: second.discard)
        }
        #expect(!exists(final))
        #expect(!exists(second.folder))
        #expect(ResumableStaging.stagedBytes(in: dir, dest: dest(dir), sha256: wholeHash) == 0)
    }

    @Test("a different size or piece layout for the same file starts over")
    func changedLayoutDiscardsOldPieces() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let first = try #require(open(dir))
        try pieces[0].write(to: first.pieceURL(0))
        first.unlock()
        let second = try #require(open(dir, totalBytes: 12))
        defer { second.unlock() }
        #expect(second.folder != first.folder)
        #expect(!exists(first.folder))
        #expect(!second.hasFinishedPiece(0, length: 4))
    }

    @Test("another file's pieces in the same folder are left alone")
    func otherArtifactKept() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let other = try #require(open(dir, name: "other.bin"))
        try pieces[0].write(to: other.pieceURL(0))
        other.unlock()
        let mine = try #require(open(dir))
        defer { mine.unlock() }
        #expect(exists(other.pieceURL(0)))
    }

    @Test("a second download of the same file while the first is running gets no shared pieces")
    func heldFolderNotShared() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let first = try #require(open(dir))
        defer { first.unlock() }
        #expect(open(dir) == nil)
    }

    @Test("a manifest that does not match the folder's identity discards its pieces")
    func mismatchedManifestDiscardsPieces() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        let first = try #require(open(dir))
        try pieces[0].write(to: first.pieceURL(0))
        first.unlock()
        try Data("{}".utf8).write(to: first.folder.appendingPathComponent("manifest.json"))
        let second = try #require(open(dir))
        defer { second.unlock() }
        #expect(!exists(second.pieceURL(0)))
    }

    @Test("downloads without a valid pinned hash never get resumable staging")
    func unpinnedNotResumable() throws {
        let dir = try makeStagingDir()
        defer { cleanup(dir) }
        #expect(open(dir, sha256: "not-a-hash") == nil)
    }
}
