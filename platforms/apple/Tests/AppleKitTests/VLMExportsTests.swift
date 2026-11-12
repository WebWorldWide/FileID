import CryptoKit
import Foundation
import Testing
@testable import FileIDAppleKit
@testable import FileIDAppleVLM
import FileIDModelInfo
@testable import FileIDVLMCore

/// Collects the events of one `fileid_apple_vlm_submit` request.
private final class Reply: @unchecked Sendable {
    let lock = NSLock()
    var tokens: [String] = []
    var terminal: (UInt32, [String: Any])?
    let done = DispatchSemaphore(value: 0)
}

private let collect: FileIDAppleVLMCallback = { context, event, payload, length in
    let reply = Unmanaged<Reply>.fromOpaque(context!).takeUnretainedValue()
    let data = payload.map { Data(bytes: $0, count: length) } ?? Data()
    reply.lock.lock()
    defer { reply.lock.unlock() }
    switch event {
    case 1: reply.tokens.append(String(decoding: data, as: UTF8.self))
    case 3, 4:
        reply.terminal = (event, (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:])
        reply.done.signal()
    default: break
    }
}

private func submit(_ request: [String: Any], timeout: Double = 30) throws -> (UInt32, [String: Any]) {
    let reply = Reply()
    let body = try JSONSerialization.data(withJSONObject: request)
    let context = Unmanaged.passRetained(reply)
    defer { context.release() }
    let handle = body.withUnsafeBytes { bytes in
        fileid_apple_vlm_submit(bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count, collect, context.toOpaque())
    }
    #expect(handle != 0)
    #expect(reply.done.wait(timeout: .now() + timeout) == .success)
    reply.lock.lock()
    defer { reply.lock.unlock() }
    return try #require(reply.terminal)
}

private func spec(kind: String = "qwen3_vl_2b", root: URL, revision: String? = "abc") -> [String: Any] {
    var spec: [String: Any] = [
        "kind": kind, "repo": "mlx-community/Test-VLM", "huggingFaceRoot": root.path, "displayName": "Test VLM",
        "vision": true, "ramBudgetGB": 3.0, "licenseAccepted": true,
    ]
    if let revision { spec["revision"] = revision }
    return spec
}

@Suite("FileIDAppleKit VLM requests (Stage 7)", .serialized)
struct VLMExportsTests {
    init() { VLMHost.shared.reopen() }

    @Test func statusReportsNoLoadedModel() throws {
        let (event, reply) = try submit(["op": "status"])
        #expect(event == 3)
        #expect(reply["loaded"] == nil)
        #expect(reply["parked"] as? Bool == false)
        #expect((reply["footprintBytes"] as? Int ?? 0) > 0)
    }

    @Test func unknownAndIncompleteRequestsFail() throws {
        let (event, reply) = try submit(["op": "nonsense"])
        #expect(event == 4)
        #expect(reply["message"] as? String == "Unknown request nonsense.")
        #expect(reply["cancelled"] as? Bool == false)
        let (analyzeEvent, analyze) = try submit(["op": "analyze"])
        #expect(analyzeEvent == 4)
        #expect(analyze["message"] as? String == "No file.")
    }

    @Test func refusesRequestsWithoutBodyOrCallback() {
        #expect(fileid_apple_vlm_submit(nil, 0, collect, nil) == 0)
        let body = Array(#"{"op":"status"}"#.utf8)
        #expect(fileid_apple_vlm_submit(body, body.count, nil, nil) == 0)
        fileid_apple_vlm_cancel(0)
        fileid_apple_vlm_cancel(.max)
    }

    @Test func loadingAMissingModelFailsWithoutCreatingIt() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDAppleVLM-\(UUID().uuidString)")
        let (event, reply) = try submit(["op": "load", "model": spec(root: root)])
        #expect(event == 4)
        let message = reply["message"] as? String ?? ""
        #expect(message.contains("isn't installed") || message.contains("mlx.metallib is missing") || message.contains("model catalog"))
        #expect(!FileManager.default.fileExists(atPath: root.path))
    }

    @Test func offlineVerificationFollowsTheVerifiedManifest() throws {
        let model = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDAppleVLM-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: model, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: model) }
        let weights = Data("weights".utf8)
        try weights.write(to: model.appendingPathComponent("model.safetensors"))
        let digest = SHA256.hash(data: weights).map { String(format: "%02x", $0) }.joined()
        let manifest: [String: Any] = ["version": 1, "revision": "abc",
                                       "files": [["path": "model.safetensors", "size": weights.count, "sha256": digest]]]
        try JSONSerialization.data(withJSONObject: manifest).write(to: model.appendingPathComponent(".fileid-verified-abc"))
        #expect(VLMDownloader.verified(modelDir: model, revision: "abc"))
        #expect(FileManager.default.fileExists(atPath: model.appendingPathComponent(".fileid-verified-abc-v2").path))
        #expect(VLMDownloader.verified(modelDir: model, revision: "abc"))
        #expect(!VLMDownloader.verified(modelDir: model, revision: "other"))
        try Data("tampered".utf8).write(to: model.appendingPathComponent("model.safetensors"))
        #expect(!VLMDownloader.verified(modelDir: model, revision: "abc"))
    }

    /// The Rust host's spec must agree with the catalog this library reads
    /// (repository, pinned revision, model folder), or the load is refused.
    @Test func hostSpecsMustAgreeWithTheCatalog() throws {
        let kind = AIModelKind.qwen3VL2B
        let root = ModelCachePaths.huggingFaceRoot
        let pin = try #require(ModelManifest.vlmPin(forKind: kind.rawValue))
        func spec(repo: String = kind.sourceRepo, revision: String? = pin.revision, root: URL = root, kind name: String = kind.rawValue) throws -> VLMModelSpec {
            let body: [String: Any] = ["kind": name, "repo": repo, "huggingFaceRoot": root.path, "revision": revision as Any,
                                       "displayName": kind.displayName, "vision": true, "ramBudgetGB": kind.ramBudgetGB, "licenseAccepted": true]
            return try JSONDecoder().decode(VLMModelSpec.self, from: JSONSerialization.data(withJSONObject: body.compactMapValues { $0 is NSNull ? nil : $0 }))
        }
        #expect(VLMModelRegistry.disagreement(try spec()) == nil)
        #expect(VLMModelRegistry.disagreement(try spec(repo: "someone/Other-VLM"))?.contains("repositories") == true)
        #expect(VLMModelRegistry.disagreement(try spec(revision: "0000000"))?.contains("revisions") == true)
        #expect(VLMModelRegistry.disagreement(try spec(root: URL(fileURLWithPath: "/tmp/elsewhere")))?.contains("model folders") == true)
        #expect(VLMModelRegistry.disagreement(try spec(kind: "not_a_model"))?.contains("catalog") == true)
    }

    /// Every model kind the Rust engine offers on the Mac names the catalog's
    /// repository and pin (the Rust side lists them in `apple::vlm::MODELS`).
    @Test func rustHostKindsAreInTheCatalog() throws {
        let vlm = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("windows/src/engine/src/apple/vlm.rs")
        let source = try String(contentsOf: vlm, encoding: .utf8)
        let kinds = source.matches(of: /\("([a-z0-9_]+)", "[^"]+", [0-9.]+\)/).map { String($0.output.1) }
        #expect(kinds.count >= 5)
        for name in kinds {
            let kind = try #require(AIModelKind(rawValue: name), "\(name) isn't in the Swift catalog")
            #expect(ModelManifest.vlmPin(forKind: name)?.repo == kind.sourceRepo, "\(name)")
        }
    }
}
