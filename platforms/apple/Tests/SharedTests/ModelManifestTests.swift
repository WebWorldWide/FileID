// The generated ModelManifest table matches shared/models/manifest.json, and
// the curated VLM picks resolve through it.
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
        var expected: [String: (id: String, kind: String, sha256: String, approxBytes: Int64, license: String)] = [:]
        for row in rows {
            let platforms = try #require(row["platforms"] as? [String])
            guard platforms.contains("macos") else { continue }
            let repo = try #require(row["repo"] as? String)
            let revision = try #require(row["revision"] as? String)
            let path = try #require(row["path"] as? String)
            let url = "https://huggingface.co/\(repo)/resolve/\(revision)/\(path)"
            let id = try #require(row["id"] as? String)
            let kind = try #require(row["kind"] as? String)
            let sha256 = try #require(row["sha256"] as? String)
            let approxBytes = try #require(row["approxBytes"] as? NSNumber).int64Value
            let license = try #require(row["license"] as? String)
            expected[url] = (id, kind, sha256, approxBytes, license)
        }
        #expect(ModelManifest.artifacts.count == expected.count)
        for artifact in ModelManifest.artifacts {
            let row = try #require(expected[artifact.url],
                                   "table URL missing from JSON: \(artifact.url)")
            #expect(row.id == artifact.id)
            #expect(row.kind == artifact.kind)
            #expect(row.sha256 == artifact.sha256)
            #expect(row.approxBytes == artifact.approxBytes)
            #expect(row.license == artifact.license)
        }
        let tableURLs = Set(ModelManifest.artifacts.map(\.url))
        for url in expected.keys {
            #expect(tableURLs.contains(url), "JSON macos artifact missing from table: \(url)")
        }
    }

    @Test("VLM repo pins match the JSON (both directions)")
    func vlmReposMatchJSON() throws {
        let rows = try #require(manifestJSON()["vlmRepos"] as? [[String: Any]])
        var expected: [String: (kind: String, revision: String, approxBytes: Int64, license: String)] = [:]
        for row in rows {
            let platforms = try #require(row["platforms"] as? [String])
            guard platforms.contains("macos") else { continue }
            let repo = try #require(row["repo"] as? String)
            let kind = try #require(row["kind"] as? String)
            let revision = try #require(row["revision"] as? String)
            let approxBytes = try #require(row["approxBytes"] as? NSNumber).int64Value
            let license = try #require(row["license"] as? String)
            expected[repo] = (kind, revision, approxBytes, license)
        }
        #expect(ModelManifest.vlmRepos.count == expected.count)
        for pin in ModelManifest.vlmRepos {
            let row = try #require(expected[pin.repo],
                                   "table repo missing from JSON: \(pin.repo)")
            #expect(row.kind == pin.kind)
            #expect(row.revision == pin.revision)
            #expect(row.approxBytes == pin.approxBytes)
            #expect(row.license == pin.license)
        }
        let tableRepos = Set(ModelManifest.vlmRepos.map(\.repo))
        for repo in expected.keys {
            #expect(tableRepos.contains(repo), "JSON macos vlm repo missing from table: \(repo)")
        }
    }

    @Test("VLM license policies match the canonical manifest")
    func vlmLicensePoliciesMatchJSON() throws {
        let manifest = try manifestJSON()
        let rows = try #require(manifest["vlmRepos"] as? [[String: Any]])
        let policies = try #require(manifest["licensePolicies"] as? [String: [String: Any]])
        for row in rows {
            let repo = try #require(row["repo"] as? String)
            let license = try #require(row["license"] as? String)
            let kindValue = try #require(row["kind"] as? String)
            let kind = try #require(AIModelKind(rawValue: kindValue))
            let policy = try #require(policies[license])
            #expect(kind.sourceRepo == repo)
            #expect(kind.licensePolicyKey == license)
            #expect(kind.licenseTermsURL?.absoluteString
                == ((policy["termsRequired"] as? Bool) == true ? policy["licenseUrl"] as? String : nil))
        }
        #expect(AIModelKind.favorites.allSatisfy { ModelManifest.vlmPin(forKind: $0.rawValue) != nil })
        #expect(AIModelKind.gemma3_4B.licenseTermsURL?.absoluteString == "https://ai.google.dev/gemma/terms")
        #expect(AIModelKind.qwen2VL7B.licenseName == "Apache License 2.0")
        #expect(AIModelKind.paligemma3B.licenseName == "Gemma Terms of Use")
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
