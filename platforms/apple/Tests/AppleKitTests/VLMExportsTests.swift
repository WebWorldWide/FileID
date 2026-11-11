import CryptoKit
import Foundation
import Testing
@testable import FileIDAppleKit
@testable import FileIDAppleVLM

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
        #expect(message.contains("isn't installed") || message.contains("mlx.metallib is missing"))
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

    @Test func modelKindsTakeTheHostsSpec() throws {
        let root = FileManager.default.temporaryDirectory
        let data = try JSONSerialization.data(withJSONObject: spec(kind: "test_kind", root: root).merging(["architecture": "smolvlm"]) { $1 })
        VLMModelRegistry.shared.register(try JSONDecoder().decode(VLMModelSpec.self, from: data))
        let kind = AIModelKind(rawValue: "test_kind")
        #expect(kind.sourceRepo == "mlx-community/Test-VLM")
        #expect(kind.ramBudgetGB == 3.0)
        #expect(ModelLicenseAcceptance.isAccepted(for: kind))
        #expect(ModelCachePaths.huggingFaceRoot.path == root.standardizedFileURL.path)
        #expect(!DeepAnalyze.acceptsResize(architecture: MLXModelCatalog.entry(forRepo: kind.sourceRepo)?.architecture))
    }
}
