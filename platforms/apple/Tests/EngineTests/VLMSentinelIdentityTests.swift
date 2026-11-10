import Foundation
import Testing
@testable import FileIDEngine
import FileIDShared

private final class HashCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var calls = 0
    var count: Int { lock.withLock { calls } }

    func hash(_ url: URL) throws -> String {
        lock.withLock { calls += 1 }
        return try sha256HexOfFile(at: url)
    }
}

@Suite("VLM verified sentinel identity record")
struct VLMSentinelIdentityTests {
    struct Fixture {
        let root: URL
        let modelDir: URL
        let sentinel: URL
        let weights: URL

        var record: URL { VLMDownloader.identitySentinelURL(sentinel) }
    }

    private func makeFixture() async throws -> Fixture {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileID-VLM-Identity-\(UUID().uuidString)")
        let modelDir = root.appendingPathComponent("model")
        try FileManager.default.createDirectory(at: modelDir, withIntermediateDirectories: true)
        let weights = modelDir.appendingPathComponent("model.safetensors")
        try Data(repeating: 7, count: 4096).write(to: weights)
        try Data("{}".utf8).write(to: modelDir.appendingPathComponent("config.json"))
        let sentinel = modelDir.appendingPathComponent(".fileid-verified-rev")
        let listed = [
            VLMRepoFile(path: "config.json", size: 2, sha256: nil),
            VLMRepoFile(path: "model.safetensors", size: 4096, sha256: nil),
        ]
        try await VLMDownloader.writeVerifiedSentinel(sentinel, modelDir: modelDir, revision: "rev", files: listed)
        return Fixture(root: root, modelDir: modelDir, sentinel: sentinel, weights: weights)
    }

    private func verify(_ fixture: Fixture, _ counter: HashCounter) async -> Bool {
        await VLMDownloader.verifiedSentinelIsValid(
            fixture.sentinel, modelDir: fixture.modelDir, revision: "rev",
            hash: { try counter.hash($0) }
        )
    }

    private func readRecord(_ fixture: Fixture) throws -> VLMDownloader.IdentitySentinel {
        try JSONDecoder().decode(VLMDownloader.IdentitySentinel.self, from: Data(contentsOf: fixture.record))
    }

    private func rewriteWeights(
        _ fixture: Fixture,
        manifest: String? = nil,
        _ change: (VLMDownloader.FileIdentity) -> VLMDownloader.FileIdentity
    ) throws {
        let record = try readRecord(fixture)
        let files = record.files.map { $0.path == "model.safetensors" ? change($0) : $0 }
        let updated = VLMDownloader.IdentitySentinel(
            version: 2, revision: record.revision,
            manifestSHA256: manifest ?? record.manifestSHA256, files: files
        )
        try JSONEncoder().encode(updated).write(to: fixture.record)
    }

    private func identity(_ fixture: Fixture) -> VLMDownloader.FileIdentity? {
        VLMDownloader.fileIdentity(fixture.weights, path: "model.safetensors")
    }

    private func modified(_ url: URL) throws -> Date {
        let values = try url.resourceValues(forKeys: [.contentModificationDateKey])
        return try #require(values.contentModificationDate)
    }

    @Test("installing records every file's identity and an unchanged install skips the hash")
    func unchangedInstallSkipsHash() async throws {
        let fixture = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let record = try readRecord(fixture)
        #expect(record.version == 2)
        #expect(record.files.map(\.path) == ["config.json", "model.safetensors"])
        #expect(record.files.last == identity(fixture))
        let counter = HashCounter()
        #expect(await verify(fixture, counter))
        #expect(await verify(fixture, counter))
        #expect(counter.count == 0)
    }

    @Test("a recorded size, mtime, ctime, inode, device or manifest that differs forces a re-hash")
    func anyRecordedDifferenceForcesRehash() async throws {
        let fixture = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let changes: [(VLMDownloader.FileIdentity) -> VLMDownloader.FileIdentity] = [
            { .init(path: $0.path, size: $0.size + 1, device: $0.device, inode: $0.inode,
                    modifiedSeconds: $0.modifiedSeconds, modifiedNanoseconds: $0.modifiedNanoseconds,
                    changedSeconds: $0.changedSeconds, changedNanoseconds: $0.changedNanoseconds) },
            { .init(path: $0.path, size: $0.size, device: $0.device, inode: $0.inode,
                    modifiedSeconds: $0.modifiedSeconds, modifiedNanoseconds: $0.modifiedNanoseconds + 1,
                    changedSeconds: $0.changedSeconds, changedNanoseconds: $0.changedNanoseconds) },
            { .init(path: $0.path, size: $0.size, device: $0.device, inode: $0.inode,
                    modifiedSeconds: $0.modifiedSeconds, modifiedNanoseconds: $0.modifiedNanoseconds,
                    changedSeconds: $0.changedSeconds - 1, changedNanoseconds: $0.changedNanoseconds) },
            { .init(path: $0.path, size: $0.size, device: $0.device, inode: $0.inode + 1,
                    modifiedSeconds: $0.modifiedSeconds, modifiedNanoseconds: $0.modifiedNanoseconds,
                    changedSeconds: $0.changedSeconds, changedNanoseconds: $0.changedNanoseconds) },
            { .init(path: $0.path, size: $0.size, device: $0.device + 1, inode: $0.inode,
                    modifiedSeconds: $0.modifiedSeconds, modifiedNanoseconds: $0.modifiedNanoseconds,
                    changedSeconds: $0.changedSeconds, changedNanoseconds: $0.changedNanoseconds) },
        ]
        let counter = HashCounter()
        var expected = 0
        for change in changes {
            try rewriteWeights(fixture, change)
            #expect(await verify(fixture, counter))
            expected += 2
            #expect(counter.count == expected)
            #expect(try readRecord(fixture).files.last == identity(fixture))
        }
        try rewriteWeights(fixture, manifest: String(repeating: "0", count: 64)) { $0 }
        #expect(await verify(fixture, counter))
        #expect(counter.count == expected + 2)
        #expect(await verify(fixture, counter))
        #expect(counter.count == expected + 2)
    }

    @Test("touching or replacing a file on disk forces a re-hash")
    func touchOrReplaceForcesRehash() async throws {
        let fixture = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let counter = HashCounter()
        let original = try #require(identity(fixture))
        let mtime = try modified(fixture.weights)

        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fixture.weights.path)
        let chmodded = try #require(identity(fixture))
        #expect(chmodded.modifiedSeconds == original.modifiedSeconds)
        #expect(chmodded.modifiedNanoseconds == original.modifiedNanoseconds)
        #expect(chmodded.changedNanoseconds != original.changedNanoseconds || chmodded.changedSeconds != original.changedSeconds)
        #expect(await verify(fixture, counter))
        #expect(counter.count == 2)

        try FileManager.default.setAttributes([.modificationDate: mtime], ofItemAtPath: fixture.weights.path)
        #expect(await verify(fixture, counter))
        #expect(counter.count == 4)

        let copy = fixture.modelDir.appendingPathComponent("copy.tmp")
        try Data(repeating: 7, count: 4096).write(to: copy)
        try FileManager.default.setAttributes([.modificationDate: mtime], ofItemAtPath: copy.path)
        #expect(rename(copy.path, fixture.weights.path) == 0)
        let replaced = try #require(identity(fixture))
        #expect(replaced.inode != original.inode)
        #expect(await verify(fixture, counter))
        #expect(counter.count == 6)
        #expect(await verify(fixture, counter))
        #expect(counter.count == 6)
    }

    @Test("a swapped same-size file with its old mtime fails verification")
    func swappedFileFails() async throws {
        let fixture = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let mtime = try modified(fixture.weights)
        let handle = try FileHandle(forWritingTo: fixture.weights)
        try handle.write(contentsOf: Data(repeating: 8, count: 4096))
        try handle.close()
        try FileManager.default.setAttributes([.modificationDate: mtime], ofItemAtPath: fixture.weights.path)
        #expect(abs(try modified(fixture.weights).timeIntervalSince(mtime)) < 0.001)
        let counter = HashCounter()
        #expect(!(await verify(fixture, counter)))
        #expect(counter.count > 0)
    }

    @Test("a version-1 sentinel alone is re-hashed once, then upgraded to the identity record")
    func versionOneUpgrades() async throws {
        let fixture = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        try FileManager.default.removeItem(at: fixture.record)
        let counter = HashCounter()
        #expect(await verify(fixture, counter))
        #expect(counter.count == 2)
        #expect(FileManager.default.fileExists(atPath: fixture.record.path))
        #expect(await verify(fixture, counter))
        #expect(counter.count == 2)
    }

    @Test("a missing file fails even when the identity record exists")
    func missingFileFails() async throws {
        let fixture = try await makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        try FileManager.default.removeItem(at: fixture.weights)
        let counter = HashCounter()
        #expect(!(await verify(fixture, counter)))
    }
}
