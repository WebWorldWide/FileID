// Locks the compiled ModelManifest table to the canonical
// shared/models/manifest.json — the Swift sibling of the Windows
// engine's manifest_consistency.rs. Either side drifting (a URL, hash,
// revision, or size changed in one place but not the other) fails here.
import Testing
@testable import FileIDShared
import Foundation

private func repoRootURL() -> URL {
    URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()   // SharedTests
        .deletingLastPathComponent()   // Tests
        .deletingLastPathComponent()   // apple
        .deletingLastPathComponent()   // platforms
        .deletingLastPathComponent()   // repo root
}

@Suite("ModelManifest — locked to shared/models/manifest.json")
struct ModelManifestTests {

    private func manifestJSON() throws -> [String: Any] {
        let url = repoRootURL().appendingPathComponent("shared/models/manifest.json")
        let data = try Data(contentsOf: url)
        let object = try JSONSerialization.jsonObject(with: data)
        return try #require(object as? [String: Any])
    }

    @Test("macOS static artifacts match the JSON (both directions)")
    func artifactsMatchJSON() throws {
        let rows = try #require(manifestJSON()["artifacts"] as? [[String: Any]])
        var expected: [String: (id: String, sha256: String, approxBytes: Int64)] = [:]
        for row in rows {
            let platforms = try #require(row["platforms"] as? [String])
            guard platforms.contains("macos") else { continue }
            let url = try #require(row["url"] as? String)
            let id = try #require(row["id"] as? String)
            let sha256 = try #require(row["sha256"] as? String)
            let approxBytes = try #require(row["approxBytes"] as? NSNumber).int64Value
            expected[url] = (id, sha256, approxBytes)
        }
        #expect(ModelManifest.artifacts.count == expected.count)
        for artifact in ModelManifest.artifacts {
            let row = try #require(expected[artifact.url],
                                   "table URL missing from JSON: \(artifact.url)")
            #expect(row.id == artifact.id)
            #expect(row.sha256 == artifact.sha256)
            #expect(row.approxBytes == artifact.approxBytes)
        }
        let tableURLs = Set(ModelManifest.artifacts.map(\.url))
        for url in expected.keys {
            #expect(tableURLs.contains(url), "JSON macos artifact missing from table: \(url)")
        }
    }

    @Test("VLM repo pins match the JSON (both directions)")
    func vlmReposMatchJSON() throws {
        let rows = try #require(manifestJSON()["vlmRepos"] as? [[String: Any]])
        var expected: [String: (kind: String, revision: String, approxBytes: Int64)] = [:]
        for row in rows {
            let platforms = try #require(row["platforms"] as? [String])
            guard platforms.contains("macos") else { continue }
            let repo = try #require(row["repo"] as? String)
            let kind = try #require(row["kind"] as? String)
            let revision = try #require(row["revision"] as? String)
            let approxBytes = try #require(row["approxBytes"] as? NSNumber).int64Value
            expected[repo] = (kind, revision, approxBytes)
        }
        #expect(ModelManifest.vlmRepos.count == expected.count)
        for pin in ModelManifest.vlmRepos {
            let row = try #require(expected[pin.repo],
                                   "table repo missing from JSON: \(pin.repo)")
            #expect(row.kind == pin.kind)
            #expect(row.revision == pin.revision)
            #expect(row.approxBytes == pin.approxBytes)
        }
        let tableRepos = Set(ModelManifest.vlmRepos.map(\.repo))
        for repo in expected.keys {
            #expect(tableRepos.contains(repo), "JSON macos vlm repo missing from table: \(repo)")
        }
    }

    @Test("lookup helpers resolve by exact URL / repo")
    func lookupHelpers() throws {
        let sface = try #require(URL(string:
            "https://huggingface.co/opencv/face_recognition_sface/resolve/main/face_recognition_sface_2021dec.onnx"))
        #expect(ModelManifest.sha256(forURL: sface)
            == "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79")
        let unknown = try #require(URL(string: "https://huggingface.co/unknown/repo/resolve/main/x.onnx"))
        #expect(ModelManifest.sha256(forURL: unknown) == nil)

        let qwen = try #require(ModelManifest.vlmPin(forRepo: "mlx-community/Qwen2.5-VL-7B-Instruct-4bit"))
        #expect(qwen.revision == "fdcc572e8b05ba9daeaf71be8c9e4267c826ff9b")
        #expect(ModelManifest.vlmPin(forRepo: "mlx-community/does-not-exist") == nil)
    }
}
