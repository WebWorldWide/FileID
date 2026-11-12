import Foundation
import Testing
@testable import FileIDEngine
import FileIDShared

/// `shared/test-corpus/mlx-verified-install.json`: the Swift and Rust engines
/// record a verified MLX install in one format, so either accepts the other's.
@Suite("MLX verified-install records shared with the Rust engine")
struct VLMSentinelInteropTests {
    struct Fixture: Decodable {
        struct File: Decodable {
            let path: String
            let text: String?
            let byte: UInt8?
            let count: Int?
        }
        let revision: String
        let files: [File]
        let swiftManifest: String
        let swiftManifestSHA256: String
        let rustManifest: String
        let rustManifestSHA256: String
    }

    private final class Counter: @unchecked Sendable {
        private let lock = NSLock()
        private var calls = 0
        var count: Int { lock.withLock { calls } }
        func hash(_ url: URL) throws -> String {
            lock.withLock { calls += 1 }
            return try sha256HexOfFile(at: url)
        }
    }

    private func load() throws -> (Fixture, URL) {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let data = try Data(contentsOf: root.appendingPathComponent("shared/test-corpus/mlx-verified-install.json"))
        let fixture = try JSONDecoder().decode(Fixture.self, from: data)
        let modelDir = FileManager.default.temporaryDirectory.appendingPathComponent("FileID-MLX-Interop-\(UUID().uuidString)")
        for file in fixture.files {
            let url = modelDir.appendingPathComponent(file.path)
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            let bytes = file.text.map { Data($0.utf8) } ?? Data(repeating: file.byte ?? 0, count: file.count ?? 0)
            try bytes.write(to: url)
        }
        return (fixture, modelDir)
    }

    private func sentinel(_ fixture: Fixture, _ modelDir: URL) -> URL {
        modelDir.appendingPathComponent(".fileid-verified-\(fixture.revision)")
    }

    @Test("the Swift writer produces the fixture's Swift record byte for byte")
    func swiftWriterMatchesFixture() async throws {
        let (fixture, modelDir) = try load()
        defer { try? FileManager.default.removeItem(at: modelDir) }
        let listed = fixture.files.map { VLMRepoFile(path: $0.path, size: Int64(($0.text?.utf8.count) ?? ($0.count ?? 0)), sha256: nil) }
        try await VLMDownloader.writeVerifiedSentinel(sentinel(fixture, modelDir), modelDir: modelDir, revision: fixture.revision, files: listed)
        let written = try String(contentsOf: sentinel(fixture, modelDir), encoding: .utf8)
        #expect(written == fixture.swiftManifest)
    }

    @Test("a Rust record and a Rust-ordered identity record are accepted without hashing")
    func rustRecordsAreAccepted() async throws {
        let (fixture, modelDir) = try load()
        defer { try? FileManager.default.removeItem(at: modelDir) }
        let record = sentinel(fixture, modelDir)
        try Data(fixture.rustManifest.utf8).write(to: record)
        let counter = Counter()
        #expect(await VLMDownloader.verifiedSentinelIsValid(record, modelDir: modelDir, revision: fixture.revision, hash: { try counter.hash($0) }))
        #expect(counter.count == 2)
        let upgraded = try JSONDecoder().decode(VLMDownloader.IdentitySentinel.self, from: Data(contentsOf: VLMDownloader.identitySentinelURL(record)))
        #expect(upgraded.manifestSHA256 == fixture.rustManifestSHA256)

        // The Rust engine writes the identity record with serde's field order.
        let files = try fixture.files.map { file -> String in
            let identity = try #require(VLMDownloader.fileIdentity(modelDir.appendingPathComponent(file.path), path: file.path))
            return #"{"path":"\#(identity.path)","size":\#(identity.size),"device":\#(identity.device),"inode":\#(identity.inode),"modifiedSeconds":\#(identity.modifiedSeconds),"modifiedNanoseconds":\#(identity.modifiedNanoseconds),"changedSeconds":\#(identity.changedSeconds),"changedNanoseconds":\#(identity.changedNanoseconds)}"#
        }
        let rustIdentity = #"{"version":2,"revision":"\#(fixture.revision)","manifestSHA256":"\#(fixture.rustManifestSHA256)","files":[\#(files.sorted().joined(separator: ","))]}"#
        try Data(rustIdentity.utf8).write(to: VLMDownloader.identitySentinelURL(record))
        #expect(await VLMDownloader.verifiedSentinelIsValid(record, modelDir: modelDir, revision: fixture.revision, hash: { try counter.hash($0) }))
        #expect(counter.count == 2)
    }
}
